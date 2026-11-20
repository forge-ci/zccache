# Replay GitHub Actions locally

Use Bosn's pinned CI engine to run the Linux x86-64 leg of the action
self-test workflow, including on a feature branch.

```bash
bosn ci run --workflow .github/workflows/test-action.yml --job test-action \
  --event workflow_dispatch --matrix os:ubuntu-24.04 \
  --matrix target:x86_64-unknown-linux-gnu
```

Bosn owns the job containers and the shared action checkout cache. Use
`bosn ci list` to find a run, then inspect its logs with `bosn ci logs RUN`.
