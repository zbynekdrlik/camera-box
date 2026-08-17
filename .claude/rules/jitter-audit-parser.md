---
paths:
  - "src/jitter_audit.rs"
  - "src/bin/genlock-jitter-report.rs"
  - "src/resolume_playback.rs"
---

# genlock audit-log parser (`src/jitter_audit.rs` + `genlock-jitter-report`)

`src/jitter_audit.rs` is the pure Tier-0 parser that turns captured OBS log text into
per-source/per-sender counter-delta summaries. It now carries **two independent parser
families** over the SAME log — extend the matching one, never cross-wire them:

- **INPUT side** — `parse_audit_line` / `summarize` / `AuditSample` for the
  `genlock-fifo audit '<source>':` line (`vendor/obs-studio/libobs/obs-source.c`
  `genlock_audit_log`). The head-skew / FIFO-health counters (#272/#757/#1009).
- **SEND side (#874)** — `parse_send_audit_line` / `summarize_send` / `SendAuditSample`
  for the two DistroAV send-path lines: `genlock-ndi-output audit '<name>':`
  (`vendor/distroav/src/ndi-output.cpp`) and `genlock-ndi-filter audit '<ndi name>':`
  (`vendor/distroav/src/ndi-filter.cpp`). The load-bearing output is the WINDOW DELTA:
  `delta_dropped` (offered-minus-sent in-window) vs `delta_send_wait_ms` — the issue-707
  discriminator (large send-wait + drops = blocking send / receiver backpressure;
  near-zero send-wait + drops = frames never offered, fault upstream in libobs).

## Adding a NEW `genlock-*` audit line kind

1. **New marker MUST be mutually non-substring** with every existing marker (`genlock-fifo
   audit '`, `genlock-ndi-output audit '`, `genlock-ndi-filter audit '`). That is what lets
   all parsers run over one log independently; add a test asserting each parser rejects the
   others' lines (both directions), like `send_parser_rejects_the_input_audit_line_and_noise_874`.
2. **Reuse the whitespace `key=value` token scan** — quote-extract the name, then scan
   `line[mark_at..].split_whitespace()`; unrecognized tokens (the `'%s':` fragment, an
   emitted-but-derivable field like `dropped=`, the `(#N)` decoration) contain no matching
   `=` and are skipped. Never hand-model the decoration syntax.
3. **Tier-0 RED→GREEN in this module** — it compiles on default features (no probe/OBS/rig).
   NOTE (#771): `# airuleset:build-ok` is DISABLED in camera-box, so `cargo test --lib jitter_audit
   # airuleset:build-ok` is BLOCKED — it does NOT give a local run. Because this module is pure
   `std` (no `use camera_box::…`), get the observable red→green with plain standalone rustc instead:
   `rustc --test --edition 2021 src/jitter_audit.rs -o /tmp/t && /tmp/t` (the #1026 recipe, see
   `.claude/rules/vendored-libobs-change-safety.md`). The emitting C++ under `vendor/distroav/**` is
   CI-only (windows-genlock*/linux-genlock) and is NOT what you touch here — this module is
   read-only log tooling.

## `genlock-jitter-report` CLI landmine — the `--json` #757 contract

The CLI's `--json` output is `summaries_to_json` — the INPUT-side per-source object that
`scripts/prerecord_phase_calibrate.py` consumes. **It must stay input-side only, byte-for-byte
— never add keys.** Surface any new line kind as an ADDITIVE text-mode table (see the #874
send-side table); the `--json` branch parses + returns BEFORE any send-side work so it never
double-parses and never gains keys. A send-only log is valid in text mode (exit 0); the
no-lines error (exit 2) fires only when NO audit line of any kind is present.

## The `--verdict-source` frame-loss verdict seam (#811, resolume-snv maintenance)

`genlock-jitter-report --verdict-source <NAME>` (repeatable) is an ADDITIVE scriptable mode, NOT a
new line kind: it reuses the INPUT-side `summarize_all` pipeline, then evaluates one source's
`AuditSummary` through the pure `src/resolume_playback.rs` verdict (`evaluate`) and prints one
`RESOLUME-VERDICT <name>: PASS/FAIL/ABSENT` line (exit 3 on any FAIL/ABSENT unless
`--verdict-report-only`). It exists for the resolume-snv (CG box) maintenance check — confirm its
NDI feed plays frame-loss-free on strih/stream after a dantesync roll (#811). Landmines to respect:

- **`--json` stays byte-locked (#757) — the verdict mode does NOT touch it.** The verdict branch
  sits AFTER the `--json` early-return, so `--json` still wins if both are passed and its shape
  never gains keys. Never route a verdict field into `summaries_to_json`.
- **The verdict is CADENCE-AGNOSTIC — this is deliberate, do not "fix" it.** resolume is a **non-60**
  CG source (#787 "resolume-rate exemption"), so its FIFO adapts cadence normally: raw
  `delta_holds`/`delta_overruns` MOVE on a healthy window and are NOT gated. The verdict gates only
  skew (`max_abs_head_skew_ms` ≤ bound, default 20 ms) + the pathology set
  (`delta_dropped_due`/`underruns`/`relocks`/`late_holds`/`backward_regime_ticks` == 0);
  `backward_regime_ticks` (#1009) IS the true frame-jump/duplicate signal. Gating holds/overruns
  would false-fail any non-60 source.
- **The verdict lives in a crate-root PURE module** (`src/resolume_playback.rs`, self-contained std,
  no `use crate::…`) so it is standalone-rustc Tier-0 testable (`rustc --test src/resolume_playback.rs`),
  same recipe as `jitter_audit.rs`. The bin maps `AuditSummary → PlaybackWindow` (a trivial field
  copy — cover it through the real bin path, `tests/*` re-implementing its own copy of the mapping
  does NOT catch a field-swap in the bin's copy; #811 review 🔵-1).
- **This is a STANDALONE maintenance verify, NOT an E2E `[0/8]` precondition.** resolume is not a
  measured source in the cam→strih→stream recording path and is a traveling box often powered off —
  wiring it into `recording-e2e.sh`'s dantesync-version-gate roster would fail-CLOSE every E2E run
  when it is away (the imag-nb #1013 pain). The general rule for ANY new maintenance target: an
  arg-driven fleet gate takes a DOCUMENTED node (targets.md + `.claude/skills/ops`), not a code
  roster; keep a non-measured / traveling box out of the E2E hot path.
