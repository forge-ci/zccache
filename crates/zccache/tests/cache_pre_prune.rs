//! Execute the actual pre-prune script with divergent GitHub inventory views.

use std::process::{Command, Stdio};
use std::time::Duration;

const HARNESS: &str = r#"
const fs = require('node:fs');
const assert = require('node:assert/strict');
const root = process.cwd();
const mode = process.argv[1];
const workflow = fs.readFileSync('.github/workflows/cache-pre-prune.yml', 'utf8');
const section = workflow.split('      - name: Forecast and retire old-lock cache generations\n')[1];
const script = section.split('          script: |\n')[1]
  .split('\n').map(line => line.slice(12)).join('\n');
let caches = [{id: 1, key: 'setup-uv-1-current', ref: 'refs/heads/main',
  size_in_bytes: 2807259435, created_at: '2026-11-05T00:00:00Z'}];
if (mode === 'retire') caches.push(
  {id: 2, key: 'soldr-mini-v2-linux-x64-glibc-v0.9.25', ref: 'refs/heads/main',
    size_in_bytes: 1000, created_at: '2026-11-04T00:00:00Z'},
  {id: 3, key: 'soldr-mini-v2-linux-x64-glibc-v0.9.47', ref: 'refs/heads/main',
    size_in_bytes: 1000, created_at: '2026-11-05T00:00:00Z'});
let reads = 0;
const deleted = [];
const infos = [];
const gap = mode === 'budget' ? 20000000000 : 2930787722;
const listed = () => caches.reduce((sum, c) => sum + c.size_in_bytes, 0);
const initialUsage = listed() + gap;
const github = {
  rest: {
    repos: { getBranch: async () => ({data: {commit: {sha: 'abc'}}}) },
    actions: {
      getActionsCacheUsage: async () => ({data: {active_caches_size_in_bytes:
        mode === 'retire' ? initialUsage : listed() + (mode === 'churn' ? 0 : gap)}}),
      getActionsCacheList: Symbol('list'),
      deleteActionsCacheById: async ({cache_id}) => {
        deleted.push(cache_id);
        caches = caches.filter(c => c.id !== cache_id);
      },
    },
  },
  paginate: async () => {
    reads += 1;
    if (mode === 'churn') caches[0].id = reads;
    return caches.map(c => ({...c}));
  },
};
const core = {info: m => infos.push(m), error: m => infos.push(m),
  summary: {addHeading() {return this}, addRaw() {return this}, async write() {}}};
global.setTimeout = f => f();
process.env.GITHUB_WORKSPACE = root;
process.env.ROOT_LOCK_HASH_LF = '01fdce755221f8f4';
process.env.ROOT_LOCK_HASH_CRLF = '01fdce755221f8f4';
const AsyncFunction = Object.getPrototypeOf(async function() {}).constructor;
(async () => {
  let error;
  try { await new AsyncFunction('github', 'core', 'context', 'require', script)(
    github, core, {repo: {owner: 'o', repo: 'r'}, sha: 'abc'}, require); }
  catch (e) {error = e.message;}
  if (mode === 'churn') {
    assert.match(error || '', /did not converge/);
    assert.deepEqual(deleted, []);
    assert.equal(reads, 40, 'must stop before planning or post-delete verification');
  } else if (mode === 'budget') {
    assert.match(error || '', /cache transition forecast failed closed/);
    assert(!infos.some(m => m.startsWith('Pre-prune verified')));
  } else {
    assert.equal(error, undefined);
    assert(infos.some(m => m.startsWith('Pre-prune verified')));
    if (mode === 'retire') assert(deleted.includes(2), 'must actually exercise retire-first');
    for (const line of infos.filter(m => m.includes(' listed bytes / '))) {
      const counts = line.match(/(\d+) listed bytes \/ (\d+) usage bytes/);
      assert.equal(Number(counts[2]) - Number(counts[1]), gap);
    }
  }
})().catch(error => {console.error(error); process.exitCode = 1;});
"#;

fn scenario(mode: &str) -> std::io::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut child = Command::new("node")
        .args(["-e", HARNESS, mode])
        .current_dir(root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let status = zccache_core::wait::child_for(&mut child, Duration::from_secs(10))?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "workflow scenario {mode} failed"
        )));
    }
    Ok(())
}

#[test]
fn divergent_usage_is_reserved_through_verification() -> std::io::Result<()> {
    scenario("gap")?;
    scenario("retire")
}

#[test]
fn equal_bytes_with_changing_ids_fail_before_deleting() -> std::io::Result<()> {
    scenario("churn")
}

#[test]
fn unexplained_usage_still_blocks_an_over_budget_forecast() -> std::io::Result<()> {
    scenario("budget")
}
