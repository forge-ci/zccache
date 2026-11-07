//! Rustc artifact and verdict key computation.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use zccache_hash::ContentHash;

use super::{normalize_key_path, ArtifactKey, ContextKey};
use crate::rustc_args::RustcParsedArgs;

/// Compute the artifact key for a rustc compilation.
///
/// Like `compute_artifact_key` for C/C++, but also incorporates
/// extern crate content hashes (analogous to header content hashes).
pub fn compute_rustc_artifact_key<P: AsRef<Path> + Ord>(
    context_key: &ContextKey,
    file_hashes: &mut [(P, ContentHash)],
    extern_hashes: &mut [(String, ContentHash)],
) -> ArtifactKey {
    compute_rustc_artifact_key_with_root(context_key, file_hashes, extern_hashes, None)
}

/// Everything besides the artifact key that selects a rustc verdict.
///
/// Output bytes are keyed only by compiler inputs. Diagnostics and exit status
/// live behind this second layer so that an invocation cannot consume stderr
/// recorded for a different Dylint input or rendering contract (#1929) while
/// both share identical artifact bytes.
#[derive(Debug, Clone, Copy)]
pub struct RustcVerdictContract<'a> {
    /// Dylint input identity; `None` selects the plain-rustc verdict.
    pub dylint_input_hash: Option<&'a str>,
    /// Supplies the requested diagnostic rendering flags.
    pub args: &'a RustcParsedArgs,
}

impl RustcVerdictContract<'_> {
    /// The verdict keys of this contract for one artifact.
    #[must_use]
    pub fn keys(&self, artifact_key_hex: &str) -> RustcVerdictKeys {
        let mut hasher = kernal_api::hash::Blake3Hasher::new();
        hasher.update(b"zccache-rustc-verdict-key-v2\0");
        hasher.update(artifact_key_hex.as_bytes());
        hasher.update(b"\0mode\0");
        match self.dylint_input_hash {
            Some(hash) => {
                hasher.update(b"dylint\0");
                hasher.update(hash.as_bytes());
            }
            None => {
                hasher.update(b"plain");
            }
        };
        // These flags change stderr but not output bytes, so they split the
        // verdict and share the artifact. `--json` selections change the bytes
        // and live in the context key instead. Spellings rustc treats alike
        // hash alike: no flag means `human` and `auto`. `--color` stays keyed
        // in JSON mode, where rustc rejects it beside `--json`: folding it away
        // would turn that error into a cache hit.
        hasher.update(b"\0format\0");
        hasher.update(
            self.args
                .error_format
                .as_deref()
                .unwrap_or("human")
                .as_bytes(),
        );
        hasher.update(b"\0color\0");
        hasher.update(self.args.color.as_deref().unwrap_or("auto").as_bytes());
        let mut width_neutral = hasher.clone();
        width_neutral.update(b"width-neutral");
        hasher.update(b"width\0");
        hasher.update(
            self.args
                .diagnostic_width
                .as_deref()
                .unwrap_or("unset")
                .as_bytes(),
        );
        RustcVerdictKeys {
            width_neutral: ContentHash::from_bytes(*width_neutral.finalize().as_bytes()).to_hex(),
            width_specific: ContentHash::from_bytes(*hasher.finalize().as_bytes()).to_hex(),
        }
    }
}

/// The two verdict-map keys a request may be served from.
///
/// `--diagnostic-width` only reshapes rendered diagnostics, and cargo passes
/// it only on a terminal, where it follows every resize. A verdict with
/// nothing to render is therefore stored once for all widths, so a width
/// change recompiles only crates that really have diagnostics to re-render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustcVerdictKeys {
    width_neutral: String,
    width_specific: String,
}

impl RustcVerdictKeys {
    /// Build from raw hex keys. For tests that seed a verdict map directly.
    #[must_use]
    pub fn from_width_neutral(key: &str) -> Self {
        Self {
            width_neutral: key.to_string(),
            width_specific: key.to_string(),
        }
    }

    /// The verdict serving this request, if one is stored.
    #[must_use]
    pub fn lookup<'m, V>(&self, verdicts: &'m BTreeMap<String, V>) -> Option<&'m V> {
        verdicts
            .get(&self.width_specific)
            .or_else(|| verdicts.get(&self.width_neutral))
    }

    /// The key to store a verdict with this `stderr` under.
    #[must_use]
    pub fn storage_key(&self, stderr: &[u8]) -> &str {
        if stderr_renders_diagnostics(stderr) {
            &self.width_specific
        } else {
            &self.width_neutral
        }
    }
}

/// Whether `stderr` holds anything but rustc's JSON artifact notifications,
/// which are the only output a clean compile produces. Unrecognized content
/// counts as rendered, so the conservative outcome is a width-specific key.
fn stderr_renders_diagnostics(stderr: &[u8]) -> bool {
    stderr
        .split(|byte| *byte == b'\n')
        .any(|line| !line.is_empty() && !line.starts_with(b"{\"$message_type\":\"artifact\""))
}

/// Compute the rustc artifact key, optionally normalizing project-local files.
///
/// When `key_root` is provided, source and dependency file paths under that
/// root are hashed relative to it. Extern hashes remain keyed by crate name.
pub fn compute_rustc_artifact_key_with_root<P: AsRef<Path> + Ord>(
    context_key: &ContextKey,
    file_hashes: &mut [(P, ContentHash)],
    extern_hashes: &mut [(String, ContentHash)],
    key_root: Option<&Path>,
) -> ArtifactKey {
    compute_rustc_artifact_key_with_root_with(
        context_key,
        file_hashes,
        extern_hashes,
        key_root,
        |path, key_root| normalize_key_path(path, key_root).into(),
    )
}

/// Like [`compute_rustc_artifact_key_with_root`] but the per-header path
/// normalization is delegated to a caller-supplied closure. Used by the
/// daemon's rustc miss/update paths to consult
/// `DepGraph::cached_normalize_key_path` for the per-header allocation
/// amortization (issue #550).
pub fn compute_rustc_artifact_key_with_root_with<P, F>(
    context_key: &ContextKey,
    file_hashes: &mut [(P, ContentHash)],
    extern_hashes: &mut [(String, ContentHash)],
    key_root: Option<&Path>,
    mut normalize: F,
) -> ArtifactKey
where
    P: AsRef<Path> + Ord,
    F: FnMut(&Path, Option<&Path>) -> Arc<str>,
{
    // Issue #571: pre-normalize each path once (O(n) via the cached
    // closure), then sort on the cached Arc<str> keys (O(n log n) byte
    // compares). The previous shape called `normalize` twice per
    // sort-comparison AND once per hash-loop entry — 3 calls per
    // element. With ~600 transitive headers per cpp/rust compile,
    // this collapses ~10k normalize calls into ~600. Hash output is
    // bit-identical: same sort order, same blake3 input bytes.
    let mut indexed: Vec<(Arc<str>, ContentHash)> = file_hashes
        .iter()
        .map(|(p, h)| (normalize(p.as_ref(), key_root), *h))
        .collect();
    indexed.sort_by(|a, b| a.0.cmp(&b.0));
    extern_hashes.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = kernal_api::hash::Blake3Hasher::new();
    hasher.update(b"zccache-rustc-artifact-key-v1\0");
    hasher.update(context_key.0.as_bytes());
    hasher.update(b"\0");

    // Source + dependency file hashes.
    for (path_key, hash) in &indexed {
        hasher.update(path_key.as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
        hasher.update(b"\0");
    }

    // Extern crate content hashes.
    hasher.update(b"externs\0");
    for (name, hash) in extern_hashes.iter() {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
        hasher.update(b"\0");
    }

    ArtifactKey(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

/// Fold rustc env-dep values into an already-computed rustc artifact key
/// (zccache#1021).
///
/// rustc records every `env!()`/`option_env!()` read as a
/// `# env-dep:NAME[=value]` line in its dep-info. The values are compile
/// inputs exactly like extern rlib bytes: `cargo:rustc-env` vars (vergen's
/// `VERGEN_GIT_SHA` and friends) change without any argv/source change,
/// and serving the old artifact ships the stale value inside the rlib.
///
/// Layered on top of the base key rather than folded inside it so that
/// contexts with **no** recorded env-deps (the overwhelmingly common
/// case) keep byte-identical artifact keys with prior releases — no cache
/// invalidation for env-free crates.
///
/// `env_hashes` entries are `(name, value_hash)` where `None` means the
/// variable was **unset** at read time (`option_env!` → `None`) — a
/// distinct variant from every set value. Entries are sorted by name for
/// determinism.
#[must_use]
pub fn fold_rustc_env_deps_into_artifact_key(
    base: ArtifactKey,
    env_hashes: &mut [(String, Option<ContentHash>)],
) -> ArtifactKey {
    if env_hashes.is_empty() {
        return base;
    }
    env_hashes.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = kernal_api::hash::Blake3Hasher::new();
    hasher.update(b"zccache-rustc-env-deps-v1\0");
    hasher.update(base.0.as_bytes());
    hasher.update(b"\0");
    for (name, value_hash) in env_hashes.iter() {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        match value_hash {
            Some(h) => hasher.update(h.as_bytes()),
            None => hasher.update(b"unset"),
        };
        hasher.update(b"\0");
    }
    ArtifactKey(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}
