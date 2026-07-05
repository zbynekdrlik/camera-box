#!/usr/bin/env bash
# scripts/lib/ndi-alive.sh — shared "camera-box is alive and streaming NDI" signal (#451).
#
# Both scripts/deploy-fleet.sh (post-binary-deploy check) and scripts/upgrade-fleet-ndi.sh
# (post-NDI-runtime-swap check) need the SAME answer to "did camera-box survive and keep
# streaming?" — before #451 each script kept its OWN copy of these two grep patterns. #445
# broadened upgrade-fleet-ndi.sh's copy to tolerate cam3's older, non-decimating log shape and
# a generic sender-ready line, but deploy-fleet.sh's copy was never updated alongside it — a
# latent re-break: any box whose camera-box is running WITHOUT genlock decimation configured
# (see src/main.rs — the "N fps emitted / M fps captured" line is only logged when genlock
# decimation is ACTIVE; otherwise it logs the plain "Streaming: X fps" form) would false-fail
# deploy-fleet.sh's post-deploy verification even though the deploy succeeded byte-for-byte and
# the box is genuinely alive. Extracting both patterns into this ONE shared file (sourced by
# both scripts) makes that kind of drift impossible going forward.
#
# Source-only: this file defines two pure functions and performs no side effects on its own.

# emit_ok_grep_pattern -> the journalctl grep proving camera-box's NDI capture->emit path is
# alive: the genlock decimation report ("N fps emitted / M fps captured"), OR the older,
# non-decimating "Streaming: X.Y fps" shape, OR the generic NDI-sender-ready line.
emit_ok_grep_pattern() { echo 'fps emitted .* fps captured|Streaming: [0-9.]+ fps|NDI sender ready'; }

# fatal_grep_pattern -> the exact crash signatures both scripts scan for after a restart/swap.
fatal_grep_pattern() { echo "panic|thread '.*' panicked|SIGSEGV|SIGABRT|core dumped|FATAL"; }
