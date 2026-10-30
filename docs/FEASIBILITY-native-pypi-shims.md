# Feasibility Report: Native Binary Shims via PyPI

**Date**: 2025-03-08
**Status**: Feasible — proven pattern used by ruff, uv, and ty

---

## Executive Summary

**Yes, zccache can ship native Rust binaries via PyPI with zero Python in the hot path.** The approach is battle-tested by Astral's tools (ruff, uv, ty) which collectively handle millions of installs. The key mechanism is maturin's `bindings = "bin"` mode, which places compiled Rust executables directly into the wheel's `.data/scripts/` directory — bypassing `console_scripts` entry points entirely.

---

## How It Works

### Wheel Layout (No Python Code)

```
zccache-0.1.0.data/
    scripts/
        zccache           # native binary (Unix)
        zccache.exe       # native binary (Windows)
        zccache-daemon    # native binary (Unix)
        zccache-daemon.exe
zccache-0.1.0.dist-info/
    METADATA
    WHEEL
    RECORD
```

When `pip install zccache` runs, the installer copies the native binaries directly into the user's PATH directory (`bin/` on Unix, `Scripts/` on Windows). No Python shebang rewriting, no entry point resolution, no interpreter startup.

### Runtime Chain

```
compiler invocation → zccache (native binary) → done
```

Not:
```
compiler invocation → python → wrapper → subprocess → zccache (native binary)
```

### Configuration Required

```toml
# pyproject.toml
[build-system]
requires = ["maturin>=1.0,<2.0"]
build-backend = "maturin"

[project]
name = "zccache"
version = "0.1.0"
description = "A high-performance local compiler cache daemon"
requires-python = ">=3.9"

[tool.maturin]
bindings = "bin"
manifest-path = "crates/zccache-cli/Cargo.toml"
strip = true
```

---

## Proof Points

| Project | Binary | PyPI installs/month | Approach |
|---------|--------|---------------------|----------|
| **ruff** | Rust linter | ~30M+ | maturin `bindings = "bin"` |
| **uv** | Rust package manager | ~20M+ | maturin `bindings = "bin"` |
| **ty** | Rust type checker | early | maturin `bindings = "bin"` |

All three use the identical pattern: maturin, `bindings = "bin"`, `strip = true`, platform matrix builds in GitHub Actions via `maturin-action`.

---

## Platform Matrix

### Required Targets (Tier 1)

| Platform | Wheel tag | Notes |
|----------|-----------|-------|
| Linux x86_64 (glibc) | `manylinux_2_17_x86_64` | Rust requires glibc ≥ 2.17 |
| Linux aarch64 (glibc) | `manylinux_2_17_aarch64` | Cross-compile via zig or Docker |
| macOS x86_64 | `macosx_10_12_x86_64` | Intel Macs |
| macOS ARM64 | `macosx_11_0_arm64` | Apple Silicon |
| Windows x86_64 | `win_amd64` | Native MSVC build |

### Optional Targets (Tier 2)

| Platform | Wheel tag | Notes |
|----------|-----------|-------|
| Linux x86_64 (musl) | `musllinux_1_1_x86_64` | Alpine Linux |
| Linux aarch64 (musl) | `musllinux_1_1_aarch64` | Alpine ARM |
| Windows ARM64 | `win_arm64` | Snapdragon laptops |

All wheels use the `py3-none-{platform}` tag since there's no Python ABI dependency.

---

## Impact on Current Project

### What Changes

| Component | Current | After |
|-----------|---------|-------|
| `pyproject.toml` | setuptools + console_scripts for dev helpers | maturin `bindings = "bin"` for distribution |
| Build backend | setuptools | maturin |
| CI | cargo check/test/clippy | + maturin build per platform |
| `ci/soldr.py` | Used for `run_zccache*` dev entry points | Kept - development-only, not shipped |
| Distribution | None | PyPI wheels with native binaries |

### What Stays the Same

- All 11 Rust crates - no changes
- `soldr cargo` development workflow - no changes
- CI hooks (tool_guard, lint, readme_guard) - no changes
- Cargo.toml workspace - no changes

### The Development Entry Point Question

The existing `ci/soldr.py` dev helpers serve a **different purpose** - they run local zccache binaries through `soldr cargo run`. They are NOT distribution shims. These two concerns are orthogonal:

- **Development**: `soldr cargo build` uses soldr for toolchain resolution and the default soldr-managed compiler wrapper
- **Distribution**: `pip install zccache` installs the native binary from the maturin-built wheel

The dev helpers stay as development infrastructure. They don't ship in the wheel.

---
## Two-Binary Problem

zccache produces **two** binaries: `zccache` (CLI) and `zccache-daemon`. Maturin's `bindings = "bin"` mode can only target one `Cargo.toml` manifest at a time.

### Options

1. **Single binary with subcommands** (recommended)
   Merge daemon into CLI: `zccache daemon start` launches the daemon process. One binary, one manifest, simple wheel. This is what sccache does.

2. **Two maturin builds, one wheel**
   Use a custom build script that runs maturin twice and merges the output. More complex, fragile.

3. **Two PyPI packages**
   `zccache` (CLI) and `zccache-daemon` as separate wheels. Unnecessary complexity for users.

4. **CLI embeds daemon binary**
   Include the daemon binary as a resource in the CLI crate. Inflates binary size.

**Recommendation**: Option 1. A compiler cache tool should be a single `zccache` command. The daemon is an implementation detail. `zccache start` / `zccache wrap` / `zccache stop` is the user-facing surface.

---

## Known Gotchas

### PyPI Size Limits
- Per-file: 100 MB (default, can request increase)
- Stripped Rust binaries typically 5–20 MB — well within limits
- A 6-platform matrix × 2 binaries ≈ 60–120 MB total upload per release

### manylinux Compliance
- Pure Rust binaries with no C dependencies are straightforward
- Maturin has built-in auditwheel reimplementation
- `cargo-zigbuild` (via `--zig` flag) is the cleanest cross-compilation path

### macOS Code Signing
- Binaries from `pip install` are NOT quarantined (no `com.apple.quarantine` xattr)
- Notarization is **impossible** for bare binaries in wheels — not a problem in practice (ruff/uv prove this)
- Ad-hoc signing (`codesign -s -`) recommended for safety

### Windows Defender
- Unsigned Rust binaries rarely trigger false positives (unlike PyInstaller's shared bootloader)
- Code signing with a trusted certificate is nice-to-have, not required
- Submit false positives to Microsoft WDSI portal if they occur

### Cross-Compilation
- Linux x86_64: native build on `ubuntu-latest`
- Linux aarch64: `cargo-zigbuild` or `cross` in Docker
- macOS: native builds on `macos-latest` (ARM64) + `macos-13` (x86_64)
- Windows: native build on `windows-latest`

---

## CI/CD Sketch

```yaml
# .github/workflows/auto-release.yml
jobs:
  build:
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            args: --zig
          - os: ubuntu-latest
            target: aarch64-unknown-linux-gnu
            args: --zig
          - os: macos-13
            target: x86_64-apple-darwin
          - os: macos-latest
            target: aarch64-apple-darwin
          - os: windows-latest
            target: x86_64-pc-windows-msvc
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: PyO3/maturin-action@v1
        with:
          command: build
          args: --release --strip --target ${{ matrix.target }} ${{ matrix.args }}
      - uses: actions/upload-artifact@v4
        with:
          name: wheel-${{ matrix.target }}
          path: target/wheels/*.whl

  publish:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
      - uses: PyO3/maturin-action@v1
        with:
          command: upload
          args: --non-interactive --skip-existing target/wheels/*.whl
```

---

## Verdict

| Question | Answer |
|----------|--------|
| Can we bypass Python entirely at runtime? | **Yes** |
| Is this a proven pattern? | **Yes** — ruff, uv, ty |
| Does it require significant project changes? | **No** — pyproject.toml + CI only |
| Any Rust code changes needed? | **No** (unless merging CLI + daemon) |
| Major risks? | **None** — straightforward for pure Rust |
| Recommended tool? | **maturin** with `bindings = "bin"` |

**Bottom line**: This is a solved problem. The exact tooling and patterns exist, are mature, and are used at massive scale. The only design decision is whether to merge the CLI and daemon into a single binary (recommended).
