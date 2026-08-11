#!/usr/bin/env bash
# camera-box #929 -- reproduce the ASRC resampling-quality A/B measurement (see RESULTS.md).
#
# Builds asrc_ab_harness.c against the system libswresample/libavutil, then runs the exact
# scenario matrix RESULTS.md's tables report: THD+N at rest vs actively compensating across
# three engine configs, plus the reissue-cadence isolation sweep. Prints everything to stdout;
# nothing here is part of the Rust build or CI -- a manual research/measurement tool.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

echo "[1/4] building asrc_ab_harness..."
gcc -O2 -Wall -Wextra -o asrc_ab_harness asrc_ab_harness.c \
    "$(pkg-config --cflags libswresample libavutil)" \
    $(pkg-config --libs libswresample libavutil) -lm

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo
echo "[2/4] THD+N + CPU matrix: config x {0, 300} ppm"
for cfg in bypass default maxq_moderate maxq_extreme; do
    for ppm in 0 300; do
        out="$WORK/${cfg}_ppm${ppm}.f32"
        ./asrc_ab_harness --mode quality --config "$cfg" --ppm "$ppm" --duration 10 --out "$out"
        python3 analyze_thdn.py "$out" --label "${cfg} ppm=${ppm}"
    done
done

echo
echo "[3/4] reissue-cadence isolation sweep (config=default, ppm=300)"
for n in 1 4 47 469; do
    out="$WORK/reissue_${n}.f32"
    ./asrc_ab_harness --mode quality --config default --ppm 300 --duration 10 \
        --reissue-every "$n" --out "$out"
    python3 analyze_thdn.py "$out" --label "reissue_every=${n}"
done

echo
echo "[4/4] compensation-effectiveness sweep (requested vs achieved ppm, config=default)"
for ppm in 0 1 2 5 8 10 10.4 10.5 11 15 20 50 100 300; do
    ./asrc_ab_harness --mode compensation --config default --ppm "$ppm" --duration 5
done

echo
echo "Done -- see RESULTS.md for the interpretation of these numbers."
