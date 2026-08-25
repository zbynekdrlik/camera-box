---
paths:
  - "scripts/ndi-halving-watchdog.sh"
  - "scripts/ndi_halving_decision.py"
  - "systemd/ndi-halving-watchdog.*"
  - "tests/harness_ndi_halving_watchdog_1203.rs"
  - "tests/python/test_ndi_halving_decision_1203.py"
---

# NDI per-connection rate-halving auto-heal watchdog (#1203)

A sixth sibling of the dev1-side alert-watchdog family (network-reach #1001 → frozen-input #1052 →
bundle-state #732 → cadence #794 → asio-starve #1023 → this) — but the FIRST with a CURE arm, which
ships GATED OFF. It pages (and, when armed, auto-reattaches) when a stream (receiver) NDI input
degrades to ~HALF the sender's cadence — live-confirmed 2026-08-25 on `NDI 2ME PGM` (15,0/s at a
30,0/s sender; `recv_capture_v3` cap_avg 65,9 ms vs a healthy ~16 ms; FIFO starving, late_holds
self-climbing, depth 11 ≪ cap 69). The proven cure is a receiver REATTACH (`obs_phase2.py
idle-receiver` → `--restore`, overlay keeps the latency pin — restored 30,0/s / 12,6 ms instantly).

## The tap: `recv-timing #797 '<input>': n=… cap_avg=…ms` — PER-INTERVAL n, measured WITHIN-PASS

The vendored DistroAV build (`vendor/distroav/src/ndi-source.cpp:1477`, via `obs_log`'s `[distroav]`
prefix + OBS's `HH:MM:SS.mmm:` log-time prefix) prints, per input, when `elapsed ≥ 5.0 s && n > 0`,
then resets `t797_n = 0` + `t797_last_log = now`:

```
HH:MM:SS.mmm: [distroav] recv-timing #797 '<obs input name>': n=<N> cap_avg=<X>ms cap_max=… out_avg=… out_max=…
```

- **`n=` is PER-INTERVAL (reset-on-read)** — like the asio `starved_blocks` tap (#1023), NOT the
  cumulative `received=` counter that #794 (cadence) / #1052 (frozen-input) persist across passes.
  So each line's `n` counts video frames in exactly `(prev_emit, this_emit]` (~5.0 s).
- **THE LOAD-BEARING DECISION — measure WITHIN ONE PASS from the last TWO lines:**
  `fps = n_curr / (ts_curr − ts_prev)`, both timestamps from the lines' OWN log prefixes (the #794
  phantom-50 principle: numerator AND denominator from the same two real lines, never a wall-clock
  divisor). Cross-pass persistence (cadence's model) is WRONG for a per-interval counter — `n_curr`
  over a 5-min pass gap would read ~0.5 fps. A pair spanning > `NDI_HALVING_MAX_WINDOW_S` (15 s,
  a freeze/gap straddle) or < min-window → UNMEASURABLE → reseed, never a false HALVED. The parse
  is source-EXACT (anchored on the trailing `':`), so `NDI 2ME PGM` never matches `NDI 2ME PGM (mv)`.

## The bands (per-input — each input's own expected_fps → own frame interval)

- **HALVED** = `fps ≤ 0.6×expected` OR `cap_avg ≥ 2.0×(1000/expected)` ms. (Live degraded: 15 fps
  ≤ 18 → HALVED by RATE; cap 65,9 ms is just under 2×33,3 = 66,7, so the rate is the primary
  trigger, the cap corroborates.)
- **HEALTHY** = `fps ≥ 0.85×expected` AND `cap_avg < 1.5×interval`. (Live healthy: 30 fps, 12,6 ms.)
- **BORDERLINE** = between the bands — report-only; it HOLDS the confirm counter (neither advances
  nor resets it). Only HALVED advances confirm; only HEALTHY resets it (via recovery/clear).

## Architecture: pure PYTHON decision + bash orchestrator + gated cure

- **Pure decision = `scripts/ndi_halving_decision.py`** (no I/O), tested by
  `tests/python/test_ndi_halving_decision_1203.py`. Python (the strih-nic-selfheal #1199
  python-mirror precedent) SPECIFICALLY so the decision matrix RED→GREENs LOCALLY under Tier-0
  (#557 blocks all cargo, even `--no-run`; the family `tests/harness_*.rs` are CI-only). The bash
  orchestrator calls `analyze` (parse+measure+classify from the raw log on stdin) once per input per
  pass and `cure-decision` (cure vs page, folding the cooldown predicate) when confirmed.
- **Orchestrator = `scripts/ndi-halving-watchdog.sh`** — REUSES `obs-watchdog-decision.sh`
  `obs_watchdog_confirm` (2-pass) / `obs_watchdog_alert_throttle` (~30 min), the flat ssh OBS-log
  `-Tail` probe (session-agnostic file read, allowed headless per win-ssh-vs-mcp; NEVER nested
  PowerShell), a fail-loud `require_tools` preflight (#833), a "tap broken" WARN after ~2 h of an
  input emitting no recv-timing line (never a silent unknown), and `airuleset.py notify` from dev1.
- **No-double-page guard reads issue-1001's OWN state for BOTH the RECEIVER (stream) AND the SENDER
  (strih, the 2ME PGM producer)** — if either is confirmed down, #1001 owns the page → SKIP (a
  sender-down input would be FROZEN, #1052's job, not halved).
- **Healthy-SIBLING context** = the box-wide vs per-connection discriminator, computed in a two-phase
  pass (phase 1 counts HEALTHY inputs; phase 2 acts). It is CONTEXT in the alert body only — it NEVER
  gates the page (unlike asio-starve #1023, where a healthy sibling is REQUIRED to page).

## The CURE arm ships OFF and is cooldown-gated (grabber-stuck #1128 shape)

- Gated by `NDI_HALVING_SELFHEAL` (default 0). `features-default-on` does NOT apply — this is a
  self-heal ACTUATOR against a live receiver, mirroring grabber-stuck's `CAMERA_BOX_GRABBER_STUCK_SELFHEAL`.
  The unit ALSO ships DISABLED (like every sibling watchdog). Report-only phase first.
- On a CONFIRMED halving: `cure-decision` returns **cure** only if armed AND a per-input cooldown
  (`NDI_HALVING_COOLDOWN_S`, 600 s) has elapsed; otherwise **page**. So a still-halved input WITHIN
  the cooldown pages (no reattach-spam), and one PAST it cures again.
- **The reattach is a two-step idle→restore; the empty-name window is a real hazard** — `idle-receiver`
  clears the NDI name for one render tick (a stopped receiver thread = a permanent wedge, see
  `.claude/rules/ndi-name-recovery.md`). `attempt_reattach` returns a DISTINCT code so the caller can
  react: 0 = reattached, 1 = could-not-start (no PREV captured → name UNTOUCHED, safe), 2 = idled but
  restore FAILED → the input is LEFT with an empty name → the caller PAGES IMMEDIATELY (throttle-
  guarded, sig `idled:<key>`) naming the manual remedy (rig-degradation-alerts-immediately, #1203
  review 🟡2). Each obs_phase2 WS call is bounded by `timeout NDI_HALVING_OBS_WS_CALL_TIMEOUT_S` so
  idle + 2 restores fit inside `TimeoutStartSec` (=180) — a systemd SIGKILL landing BETWEEN the clear
  and the restore would itself manufacture the wedge (🟡3). A `timeout` that kills the idle is SAFE
  (idle prints PREV before clearing → either PREV captured + we restore, or name untouched). The cure
  is armed only deliberately by the supervisor with the OBS-WS password set (an armed-but-passwordless
  pass warns LOUD each pass). `NDI_HALVING_CURE_CMD` overrides the whole reattach for tests; the
  `NDI_HALVING_OBS_PHASE2` seam overrides just the obs_phase2 binary so the internal branch (PREV
  parse / retry / LEFT-IDLED page) is offline-testable.

## Tier-0 verification (#557 — zero cargo)

- Pure decision: `python3 -m pytest tests/python/test_ndi_halving_decision_1203.py -q` (local RED→GREEN).
- Orchestrator: `bash -n` + `shellcheck -S warning`, and a `--dry-run` end-to-end with
  `NDI_HALVING_PROBE_CMD`/`NDI_HALVING_CURE_CMD`/`NDI_HALVING_NOW` seams (confirm → cure →
  page-in-cooldown → cure-past-cooldown, SKIP, tap-broken, recovery, borderline). The `.rs` harness
  can only be `rustfmt --check`ed locally (CI compiles+runs it) — a bash REPLICA of every harness
  assertion is the local proof.
- **Two test-harness gotchas hit building this (both cost a debug loop):** (1) `--dry-run` SKIPS
  `attempt_reattach`, so the CURE-count is only meaningful in a LIVE (non-dry) pass — the harness's
  cure tests run non-dry with a stubbed `AIRULESET_NOTIFY` (records bodies, no Discord). (2) State
  bookkeeping (cure_ts / confirm / throttle) advances in BOTH modes so the cooldown escalation is
  faithful under `--dry-run` (the family "dry-run skips the POST/action, never the bookkeeping"
  convention) — the cure branch writes `cure_ts` even in dry-run.

## Live arm/verify is the SUPERVISOR's rig step (UNVERIFIED from a code lane)

No ssh/MCP to rig boxes from this lane — the actual reattach cure and the on-rig detection are
verified by the supervisor after enabling per `systemd/ndi-halving-watchdog.README.md`.
