#!/bin/bash
# obs-box-cpu-latency-hold.sh (issue 1357) -- hold a PM QoS CPU wake-up latency bound; extended header below.
set -euo pipefail
#
# Usage: obs-box-cpu-latency-hold.sh MICROSECONDS
#
# Run by systemd/obs-box-cpu-latency.service on every OBS box (installed by obs_box_cpu_latency in
# scripts/lib/obs-box-baseline.sh). It opens /dev/cpu_dma_latency, writes the bound and then keeps the
# file descriptor open for as long as it runs: the kernel honours a PM QoS CPU latency request exactly
# while the fd that made it stays open, and every cpuidle governor then skips the idle states whose exit
# latency is longer than the bound. Stopping the service closes the fd and drops the request (reversible,
# no reboot). The shell `exec`s into `sleep infinity`, which inherits fd 3, so the long-lived process is
# a plain sleep holding the request and no shell stays around.
#
# The bound is written as a 10-byte hex string (`0x00000096` for 150). The kernel reads a write of
# exactly 4 bytes as a raw binary s32 and anything else as a hex number, so a short string like `0x96`
# (4 bytes) would be taken as the binary value of its four ASCII characters -- a huge, useless bound.
#
# OBS_BOX_CPU_LATENCY_DEV overrides the device path (the tests point it at a scratch file). The script
# never retries: a failure exits non-zero and systemd's Restart=on-failure brings it back.

US="${1-}"
DEV="${OBS_BOX_CPU_LATENCY_DEV:-/dev/cpu_dma_latency}"

case "$US" in
    '' | *[!0-9]*)
        echo "obs-box-cpu-latency: usage: $0 MICROSECONDS (a whole number, got '${US}')" >&2
        exit 2
        ;;
esac
# at most 7 digits (the kernel's own default "no constraint" is 2 s = 2000000 us); 10# drops leading
# zeros so they are never read as octal
if [ "${#US}" -gt 7 ] || [ "$((10#$US))" -gt 2000000 ]; then
    echo "obs-box-cpu-latency: bound ${US} us is out of range (0..2000000)" >&2
    exit 2
fi
US="$((10#$US))"

if ! exec 3>"$DEV"; then
    echo "obs-box-cpu-latency: cannot open ${DEV} for writing -- no latency bound is held" >&2
    exit 1
fi
if ! printf '0x%08x' "$US" >&3; then
    echo "obs-box-cpu-latency: writing the bound to ${DEV} failed -- no latency bound is held" >&2
    exit 1
fi
echo "obs-box-cpu-latency: holding a CPU wake-up latency bound of ${US} us on ${DEV} (idle states with a longer exit latency are skipped while this runs)"
exec sleep infinity
