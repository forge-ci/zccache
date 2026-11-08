# syntax=docker/dockerfile:1.7

# Isolated Linux environment for the local gate's `tests` lane
# (zackees/ci.yml#168 GATE-005, #196 GATE-009). zccache's test suite starts
# zccache daemons and touches zccache state roots, so it never runs on a
# developer host; `bosn run --task gate-test` runs it here instead. The
# package set mirrors what the hosted ubuntu-24.04 Integration runner gives
# the suite (clang, a C/C++ toolchain, CMake/Ninja, Python 3).
FROM ubuntu:24.04@sha256:a853f94d226358a79c740cfc7bce0c289748f3fe3488d921d038ccd752c61b60

# #2002: use the published release embedding zccache 1.15.12 for the
# real Soldr/Cargo freshness fixtures, not the older embedded cache.
ARG SOLDR_VERSION=0.9.52
ARG SOLDR_SHA256=abdf474d0f41e2389409b9f30d83a7c857b7c735fedacd115a68c1606e8c0dc9

# ZCCACHE_TEST_ISOLATED is the marker ci/nextest_isolation_guard.sh accepts
# in place of CI=true. Only isolated environments set it.
ENV ZCCACHE_TEST_ISOLATED=1
ENV DEBIAN_FRONTEND=noninteractive \
    SOLDR_VERSION=${SOLDR_VERSION}

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        git \
        libssl-dev \
        ninja-build \
        pkg-config \
        python3 \
        zstd \
 && rm -rf /var/lib/apt/lists/*

RUN curl --fail --location --silent --show-error \
        "https://github.com/zackees/soldr/releases/download/v${SOLDR_VERSION}/soldr-v${SOLDR_VERSION}-x86_64-unknown-linux-gnu.tar.zst" \
        -o /tmp/soldr.tar.zst \
 && echo "${SOLDR_SHA256}  /tmp/soldr.tar.zst" | sha256sum --check - \
 && mkdir /tmp/soldr \
 && tar --zstd -xf /tmp/soldr.tar.zst -C /tmp/soldr \
 && install -m 0755 /tmp/soldr/soldr /tmp/soldr/soldr-daemon /usr/local/bin/ \
 && rm -rf /tmp/soldr /tmp/soldr.tar.zst

# Run as an unprivileged user, like the hosted runner's `runner`. As root,
# read-only cache files stay writable (root bypasses permission checks), and
# the strict artifact-layout validation's warm link misses: tests that pin
# read-only materialization semantics only hold for a non-root user.
RUN useradd --create-home --uid 1001 gate \
 && mkdir -p /target /home/gate/.soldr \
 && chown gate:gate /target /home/gate/.soldr
USER gate
ENV HOME=/home/gate

WORKDIR /work
