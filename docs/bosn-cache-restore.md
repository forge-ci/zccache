# Bosn lane cache restores: where the time goes

In PR #2053's warm exact-hit `dylint` receipt (`e6863305`), setup-soldr spent
**650.6 s** restoring a 944 MB `build-cache` archive (3.19 GB, 14,260 files).
#2053 called that I/O-bound. It is not: the restore is CPU-starved by the host's
cgroup weights. This page records the measurements (2026-11-02) so the next
slow lane gets diagnosed from evidence.

## The restore pipeline

On a local runner (`ACT=true`, `s3-cache` off), setup-soldr restores
`build-cache` like this:

1. `@actions/cache` downloads the blob from act's artifact cache server.
   act stores the blob on the `bosn-ci-cache-v1` volume
   (`actcache/cohort-v1/<ns>/cache/<xx>/<id>`). The volume is ext4 on a
   Samsung 970 EVO Plus NVMe.
2. The outer layer is unpacked with `tar -xf cache.tzst -P --use-compress-program unzstd`.
   That writes `/tmp/setup-soldr-soldr/cache/zccache.tar.zst` into the job
   container's overlayfs, which runs on the inner dockerd's `bosn-act-storage-*`
   volume on the same NVMe.
3. `decompressCache` (setup-soldr `src/lib/cache-compress.ts`) runs
   `zstd -d -T0 -c zccache.tar.zst | tar -xf - -C …`.
4. `walkDirSize` counts the files. Nothing re-hashes or copies the tree a
   second time; artifact payloads are blake3-verified lazily when served
   (`zccache-artifact` staged-v2 manifests).

## Measurements

The probe used the 935 MB blob `72508cce9409fa32/cache/06/6` (same layout:
inner 937 MB `zccache.tar.zst`, 13,497 entries, 3.2 GB inflated). Each case
ran inside the inner dockerd with image `bosn/act-runner:4f2d5083a9d1`, so it
used the job's exact write path. The host was never quiet, so each number
carries the load average observed during it.

| case | load (1 min) | outer unpack | inner `zstd -T0 \| tar` | total |
|---|---|---|---|---|
| read blob only (`cat > /dev/null`) | 68–88 | 1.1 s (≈850 MB/s) | – | – |
| outer `zstd -d` to `/dev/null` | 68–88 | 1.35 s | – | – |
| one restore, alone | 83–94 | 8.4 s | 19.3 s | **27.7 s** (34 MB/s compressed, 115 MB/s written) |
| one restore, alone (repeat) | 71–85 | 5.9 s | 20.2 s | 26.1 s |
| four restores at once | 84–93 | 15.5–16.0 s | 55.3–55.9 s | **71.2–71.5 s** each |
| live lane `b3c3dfec`, 2 runners busy | ~83 | – | – | 68.4 s for a 1.15 GB / 4.05 GB archive |
| receipt `e6863305`, 4 runners busy | 102–131 | – | – | **650.6 s** |

During the four-way run, `vmstat 5` showed 220–365 MB/s written and
**0–4 % iowait**, with CPU 97–99 % busy (us+sy). The engine cgroup's
`io.pressure` full was ~2 % and its `cpu.pressure` full was 12–19 %. The disk
was idle while the tasks waited for CPU.

## Root cause: bosn.slice CPU weight

```
/sys/fs/cgroup/bosn.slice                       cpu.weight=10   cpu.max=max
/sys/fs/cgroup/bosn.slice/docker-<engine>.scope cpu.weight=35   cpu.max=800000 100000  (8 CPUs, all runners)
/sys/fs/cgroup/user.slice                       cpu.weight=100
/sys/fs/cgroup/system.slice                     cpu.weight=100
```

Every Bosn runner shares one engine container capped at 8 CPUs. That container
sits in `bosn.slice` at weight 10, against 100 for `user.slice`, where the agent
fleet's host builds run at load ~100. Under saturation the whole slice gets
about 10/210 of the machine, under one of 16 cores, shared by up to four jobs.
Each job restores dylint, dylint-output and build caches in parallel. The
restore is pure CPU work: two zstd decodes plus tar and overlayfs syscalls. It
slows down in proportion: 26 s with CPU available, 71 s against three peer
restores, and 650 s against three peer lanes plus a saturated `user.slice`.
Nothing on the restore path is I/O-bound.

## What would help, by payoff

1. **Bosn (owner of `bosn.slice`)**: raise `bosn.slice` `cpu.weight` from 10
   to at least 100 while a gate lane runs, or at minimum during setup-soldr's
   restore. You can also cap the agents in `user.slice` instead. The weight is
   a scheduling-priority decision, so it needs its owner's sign-off.
2. **Bosn scheduling**: don't start a fourth runner while host load exceeds
   the core count. Four concurrent restores already take 2.7× as long as one
   restore alone.
3. **setup-soldr, size only**: the build-cache archive is wrapped twice
   (`zccache.tar.zst` inside the `@actions/cache` tar+zstd). Re-encoding the
   same payload as one `tar | zstd -3 --long=30` layer gives **496 MB instead
   of 937 MB** (−47 %), but restore time does not change (25.9 s vs 26.1 s,
   same load). The win is cache-volume footprint: act's store holds 31 GB
   against a 32 GiB budget and maintenance reports `partial`. Restore speed
   gains nothing from it.

No zstd level change speeds up decode in a way that matters here: zstd
decompression speed is roughly independent of the level. Skipping
verification is not on the table either; the restore path does no serial
verification to skip.

## Reproduce

```bash
C=$(docker ps --format '{{.Names}}' | grep bosn-act | head -1)
# Run inside the inner dockerd so the write path matches a job container:
docker exec $C docker run --rm -v /bosn/cache:/c:ro --entrypoint sh \
  bosn/act-runner:<digest> -c '
    f=/c/actcache/cohort-v1/<ns>/cache/<xx>/<id>; mkdir -p /tmp/o /tmp/x
    time tar -xf $f -P --transform="s,.*/,," -C /tmp/o --use-compress-program unzstd
    time sh -c "zstd -d -T0 -q -c /tmp/o/zccache.tar.zst | tar -xf - -C /tmp/x; sync -f /tmp/x"'
cat /sys/fs/cgroup/bosn.slice/cpu.weight /sys/fs/cgroup/bosn.slice/cpu.pressure
```

Always record `uptime` and the busy runner count next to the result.
