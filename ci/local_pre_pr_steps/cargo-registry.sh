#!/usr/bin/env bash
set -euo pipefail
z=/target/debug/zccache
hash1="$($z cargo-registry hash --lockfile Cargo.lock)"
hash2="$($z cargo-registry hash --lockfile Cargo.lock)"
test "${#hash1}" -eq 16
test "$hash1" = "$hash2"
if $z cargo-registry hash --lockfile nonexistent.lock >/dev/null 2>&1; then exit 1; fi
soldr cargo fetch
# The builder shares its source registry volume, but /tmp belongs to this run.
scratch="$(mktemp -d)"
export ZCCACHE_CACHE_DIR="$scratch/zccache"
$z cargo-registry save --key local-pre-pr
root="$($z cache-root)"
test -f "$root/cargo-registry/local-pre-pr.tar.gz"
restore_home="$scratch/cargo-home"
mkdir -p "$restore_home"
$z cargo-registry restore --key local-pre-pr --cargo-home "$restore_home"
test "$(find "$restore_home/registry" -type f | wc -l)" -gt 0
$z cargo-registry clean
test ! -e "$root/cargo-registry/local-pre-pr.tar.gz"
