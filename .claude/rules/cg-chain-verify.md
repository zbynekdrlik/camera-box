---
paths:
  - "scripts/cg-chain-verify.sh"
  - "scripts/lib/cg-chain-verify.sh"
  - "tests/harness_cg_chain_verify_1300.rs"
---

# CG-chain receiver-side verdict (#1300)

`scripts/cg-chain-verify.sh` is the CG chain's equivalent of `recording-e2e.sh` for the FIFO-audit
level: per hop (SongPlayer `SP-*` → cg OBS/RESOLUME-SNV → strih `cg` / stream `NDI obs hudba`) it
reads ONE aligned `genlock-fifo audit` window, runs the cadence-agnostic playback verdict, reads
the per-source `asrc: source '<x>' estimated=` ppm residual, prints a per-hop table + overall
PASS/FAIL, and **exits non-zero (3) on FAIL** (a verdict tool, NOT an always-exit-0 preflight). It
is what ACCEPTS the songplayer genlock series (songplayer 146–151) from the receiver side — the
sender contract is issue 1294 (its §8 acceptance = this receiver audit). Pixel-level burn-id
contiguity through the chain is issue 1301, NOT here.

## The verdict is a REPLICA pinned to Rust — never re-derive the thresholds

The decision is `camera_box::resolume_playback::evaluate` (skew ≤ 20 ms, ZERO
`dropped_due`/`underruns`/`relocks`/`late_holds`/`backward_regime_ticks` deltas, samples ≥ 2) and
the window summary is `camera_box::jitter_audit` parse+`summarize` (last-minus-first saturating
deltas, `max_abs_head_skew_ms`). Those live in Rust (probe-free, default features), and the real
CLI `genlock-jitter-report --verdict-source '<name>'` already runs them — but it needs a built
binary and is not Tier-0 testable. So `scripts/lib/cg-chain-verify.sh` is a PURE bash/awk REPLICA
(`cg_chain_summarize_window` + `cg_chain_verdict`), and `tests/harness_cg_chain_verify_1300.rs` is
the PARITY GATE: it feeds the SAME raw-log + the SAME windows to BOTH the Rust functions and the
bash replica and asserts identical summaries + PASS/FAIL. **If you change the Rust bounds
(`PlaybackBounds`) or `jitter_audit::summarize`, the parity test goes RED — update the replica to
match, never let them drift.** Do not re-derive thresholds in bash from scratch.

## Hops, sources, and the reader seam

- `cg-obs` → each SongPlayer video input, ENUMERATED dynamically from the log (never a static list
  — burn-target-enumeration discipline). The name pattern is an operator-overridable ERE,
  `CG_CHAIN_CGOBS_SRC_RE` (default `sp-.*_video`, the issue-1300 Work spec), matched
  CASE-INSENSITIVELY (so `SP-1_video`/`sp-1_video` both match). **The EXACT live RESOLUME-SNV source
  name is NOT yet confirmed against a real OBS log (the rig is off) — confirm it at the first live
  run and either pin it via `CG_CHAIN_CGOBS_SRC_RE` or update this default; a pattern that matches
  nothing makes the cg-obs hop report `NO SOURCES` and FAIL (fail-closed, never a false PASS).**
  `strih` → `cg`. `stream` → `NDI obs hudba`.
- The log tail per hop is supplied explicitly so the tool is self-contained and ships no untested
  ssh/MCP default: `CG_CHAIN_<HOP>_LOG=<file>` (tests; and the supervisor's path for cg-obs — paste
  the win-resolume MCP FileRead of the RESOLUME-SNV OBS log to a file) or `CG_CHAIN_<HOP>_CMD="<cmd>"`
  (live: a byte-safe `ssh ... powershell -c "gc <obslog> | select -last 4000"`, or a bundle-state
  `:8899` fetch). `<HOP>` is upper-cased with `-`→`_` (`cg-obs`→`CG_OBS`).
- Raw bytes are stripped byte-safe (`cg_chain_strip_high_bytes` = `LC_ALL=C tr -d '\200-\377'`)
  BEFORE any awk/python parse, per `ps-log-byte-safety-extraction.md` — the `genlock-fifo audit`
  line carries the `≈` glyph co-resident on the parsed line, so a PowerShell `gc` re-encode would
  otherwise blind grep / feed invalid UTF-8 downstream.

## asrc residual band

`cg_chain_parse_asrc_ppm` reads the newest `estimated=<X>ppm` per source; `cg_chain_asrc_in_band`
asserts `|X| ≤ floor` (default 10, `--asrc-floor-ppm`). Per `asrc-residual-floor.md`: a steady
≈ +8 ppm is the physical Dante-GM-vs-UTC floor (PASS), the ≈ −18 ppm DVS/PTP port-collision
signature is out of band (FAIL); an ABSENT asrc line is `UNKNOWN`, never a false out-of-band. An
out-of-band present reading folds into the hop verdict; `UNKNOWN` never fails.

## Soak + rig-health + Tier-0

- `--soak-hours N --csv <path>`: repeat per window, append `ts,hop,source,verdict,skew,deltas,ppm`
  rows (the 24 h ±20 ms flatness in issue 1294 §8 is then a plot, not a claim). The inter-window
  sleep is the `CG_CHAIN_SLEEP_CMD` seam (tests set it to `:`), so no test ever blocks on a sleep.
- `rig-health-audit.py` calls `check_cg_chain()` REPORT-ONLY: it emits the `CG_CHAIN_REPORT_VERDICT
  = "NOTE"` row (never PASS/WARN/FAIL, so it never changes the audit exit code). The `#787
  resolume-rate exemption` (`CAMERA_SRC_RE = ^NDI cam\d+$`) is unchanged.
- **Gotcha — an awk comment inside the single-quoted awk program must carry NO apostrophe.** The
  `cg_chain_summarize_window` / `cg_chain_enumerate_sources` awk bodies are bash single-quoted
  (`awk '…'`), so a `#`-comment line containing an apostrophe (`jitter_audit's`) terminates the
  bash string mid-program → `bash -n` / `shellcheck` syntax error (cost a cycle this session). Keep
  awk-internal comments apostrophe-free (and mind bare `(`/`)` after a stray quote).
- **Tier-0 (#557, no cargo of any compiling shape):** `bash -n` + `shellcheck -S warning`; source
  the lib and drive the pure functions over fixtures; pytest for the python part
  (`tests/python/test_cg_chain_rig_health_1300.py`); `cargo fmt --all --check`. The Rust parity
  harness runs on CI — that is the FIRST place the replica-vs-Rust equality actually executes.
