---
paths:
  - "src/grabber_stuck.rs"
  - "scripts/lib/grabber-stuck-health.sh"
  - "scripts/grabber-stuck-alert-watchdog.sh"
  - "systemd/grabber-stuck-alert-watchdog.*"
---

# Fast-capture grabber STUCK self-heal + alert (#1128)

## The state this catches, and why it is NOT the same as the existing capture-rate self-heal

The GENKI ShadowCast 2 grabber (CAM1) can enter a state where its internal USB output clock
free-runs at ~62.5 fps (16.0 ms/frame) AND it delivers persistent corrupted buffers
(`V4L2_BUF_FLAG_ERROR`, ~4 per 5 s window). Live-confirmed 2026-08-19 (#1110 comment 5338231650):
`systemctl restart camera-box` merely re-opens the V4L2 device and does NOT clear the grabber's
internal state — the cadence stays 62.x. Only a USB re-enumeration (`echo 0/1 > .../authorized`)
re-negotiates the device and clears it.

The USB re-auth MECHANISM already existed before #1128 (`src/capture_rate_selfheal.rs::
perform_usb_reset` — the exact unbind → `authorized` 0→sleep→1 → process-exit-and-restart dance,
with a rate-limit + escalation state file at `/run/camera-box/capture-rate-selfheal.state`). The
GAP #1128 filled was the DETECTOR: the existing trigger
(`capture_rate_selfheal::should_trigger_selfheal`) is deliberately narrow after #909/#914 — the
ShadowCast jitter-band tolerance was widened to 9 % (so 62.5/60 = 4.17 % never trips it) precisely
to avoid reset-spamming the grabber's benign clock wobble, which the genlock decimation gate
already absorbs into exact NDI output. The only remaining path, the #971 chronic sustained band,
needs 180 consecutive 5 s windows (15 minutes). And `capture.corrupted_frames()`, while logged on
the `Streaming:` line, fed NO decision at all.

## The corrupted band is the DISCRIMINATOR — do not remove it

`src/grabber_stuck.rs::GrabberStuckTracker` keys on the COMBINED signature: over-rate
(`captured_fps >= OVER_RATE_FPS_FLOOR` = 61.5) AND persistent-corrupted (per-window delta of the
cumulative counter `> 0`), BOTH held for `STUCK_CONFIRM_WINDOWS` (6 = ~30 s) consecutive windows.
The corrupted band is what makes it safe to ACT where the plain over-rate band is not: a benign
over-rate wobble has 0 corrupted, so it can NEVER reach `Stuck` — which is the whole reason #1128
does not re-introduce the #909/#914 reset-spam that three tickets spent effort escaping. Bands are
calibrated from #1128's non-overlapping live data (healthy 60.0 ± 0.2 fps / 0 corrupted; stuck
62.2–62.8 fps / 4 corrupted per window). **If you ever tune these, keep the two bands
non-overlapping and keep the corrupted band mandatory — dropping it turns this back into an
over-rate-only reset-spammer.**

**Sibling trigger (#1193 sustained OVER-RATE):** `src/capture_overrate.rs` is the THIRD self-heal
trigger, built on THIS exact two-band-discriminator pattern but for a DIFFERENT signature — over-rate
(a majority of the `cap-1s` buckets ≥ 61) AND dupe-victim SHED CHURN (≥ 3/window), no corrupted
requirement. It catches the cam2 ShadowCast state (61.1 fps + ~6 sheds/5 s, 0 corrupted) that stays
BELOW this STUCK band's 61.5-fps + corrupted signature, so the two detectors are non-overlapping. Same
env-gated-OFF + shared-`attempt_self_heal` shape; see `.claude/rules/capture-selfheal-action-sequence.md`.

Baseline subtlety: the first `observe` of a fresh process records the corrupted-counter BASELINE
(delta unknown → 0), so from a cold start into an already-stuck grabber the corrupted band confirms
one window LATER than the over-rate band (STUCK at window 7, ~35 s, not 6). This is deliberate and
safe (never earlier than the warm path), and is what keeps a large inherited cumulative counter
from masquerading as one window's corruption. The `first_window_baseline...` /
`cold_start_into_an_already_stuck_grabber...` tests pin it.

## The re-auth ACTION ships GATED OFF — the default build only logs

`src/main.rs`'s capture loop feeds each 5 s window into one `GrabberStuckTracker`. On `Stuck` it
ALWAYS logs the report-only marker `#1128 grabber STUCK` (no I/O — the dev1 watchdog greps this).
The actual USB re-auth runs ONLY when `CAMERA_BOX_GRABBER_STUCK_SELFHEAL=1` (default OFF), reusing
the SAME `decide_selfheal`/`perform_usb_reset` path (shared `/run/camera-box/…` state, so both
triggers share the 600 s throttle + escalation), guarded by `pending_self_heal_exit_code.is_none()`
so it never double-resets in a window the #971 chronic band already fired. **This gate is a
rig-safety invariant: enabling live re-auth is a deliberate supervised step, never a provisioning
default** — shipping this changed no live behavior beyond a log line.

## The dev1 alert watchdog RELAYS the marker — one source of truth, one ping per episode

`scripts/grabber-stuck-alert-watchdog.sh` (dev1 `--user` timer, SHIPS DISABLED — enable
deliberately) ssh-reads each `CAMERA_ACTIVE_SET` box's journal for the `#1128 grabber STUCK` marker
within a freshness window and pages via `airuleset.py notify`. It does NOT recompute the STUCK
verdict — the Rust detector decides, the watchdog only relays (the same "self-heal emits, watchdog
pages" pattern as the #663 marker). It reuses `obs-watchdog-decision.sh` (confirm + throttle) and
`camera-set.sh` (fleet enumeration), exactly like the sibling dev1 alert watchdogs
(splitter-port / network-reach / optical-chain). **discord-volume-near-zero:** the throttle is set
huge (`GRABBER_STUCK_WATCH_THROTTLE_PASSES=1000000`) so a chronic stuck grabber pages exactly ONCE
per episode (never a repeated chronic alert); a recovery ping fires once on return-to-OK. NODATA
(ssh blip / box off) never pages and never false-recovers.

**DELIBERATE deviation from the sibling watchdogs on NODATA — do NOT "align" it back (#1128 review
🔵).** The siblings (splitter-port / network-reach) call one `clear_box_throttle` on BOTH OK and
NODATA, which resets the alert signature. Under strict one-ping-per-episode that lets a single
transient NODATA (ssh blip) BETWEEN two stuck passes clear the latch, so the next confirm cycle
pages a SECOND time for the SAME episode. This watchdog splits it: OK (a genuine recovery)
full-clears (`clear_box_throttle`); NODATA clears ONLY the confirm counter
(`clear_box_confirm_only`) and LEAVES the alert sig/passes intact — a mid-episode blip cannot
produce a second page. Pinned by
`a_transient_nodata_blip_mid_episode_does_not_produce_a_second_page`; keep the split if you ever
refactor toward the sibling convention, or the double-page returns.

`scripts/lib/grabber-stuck-health.sh` is
the pure classify (`STUCK`/`OK`/`NODATA`) + marker-fps parse — Tier-0 tested
(`tests/harness_grabber_stuck_health_1128.rs`), the driver by
`tests/harness_grabber_stuck_watchdog_1128.rs`.

## Tier-0 verification (no cargo compile — #557)

`src/grabber_stuck.rs` is a pure crate-root module (default features) — verify RED→GREEN via a
standalone `rustc --edition 2021 --test` on a scratch copy of the file, and `cargo fmt --all
--check`. `src/main.rs` has NO local type-check path — verify via `rustfmt --check` (parse/brace
balance) + cross-checking the reused `capture_rate_selfheal` signatures; CI is the first full
type-check. The bash lib + watchdog: `bash -n`, `shellcheck -S warning`, and a direct sourced run
(`. scripts/lib/grabber-stuck-health.sh; …` and the `--dry-run` driver with `probe_box`/`sshpass`
overridden) — the same net the harnesses encode, runnable with zero rig.
