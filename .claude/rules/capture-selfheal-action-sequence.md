---
paths:
  - "src/capture_rate_selfheal.rs"
  - "src/capture_overrate.rs"
  - "src/main.rs"
---

# In-process USB self-heal action sequence — ONE helper, wording is watchdog-anchored (#1149)

## Single source of truth
The in-process USB-reset action sequence — `load_state → decide_selfheal → match {Healthy/Throttled/Heal} → save_state → perform_usb_reset → pending exit code` — is ONE crate-root helper:

```
capture_rate_selfheal::attempt_self_heal(device_path, model, now_epoch_s, state_path, msgs, reset) -> Option<i32>
```

THREE triggers in `src/main.rs`'s capture loop call it, each keeping its OWN guard + band-WARN line:
- **#656/#663/#971 capture-rate** — guard `if should_trigger_selfheal(jitter_confirmed, sustained_chronic)`, msgs `CAPTURE_RATE_SELF_HEAL_MESSAGES`.
- **#1128 grabber-STUCK** — guard `if grabber_stuck_selfheal_enabled && pending_self_heal_exit_code.is_none()`, msgs `GRABBER_STUCK_SELF_HEAL_MESSAGES`.
- **#1193 sustained OVER-RATE** — the detector is `src/capture_overrate.rs::CaptureOverRateTracker` (over-rate majority of the `cap-1s` buckets AND dupe-victim shed churn, both held ~5 min; the churn band is the discriminator, mirroring #1128's corrupted band). Guard `if over_rate_selfheal_enabled && pending_self_heal_exit_code.is_none() && capture_overrate::cooldown_elapsed(load_state(...).last_heal_epoch_s, now, OVERRATE_MIN_HEAL_INTERVAL_S)`, msgs `OVER_RATE_SELF_HEAL_MESSAGES`. Env gate `CAMERA_BOX_GRABBER_OVERRATE_SELFHEAL` (default OFF, canary on cam2 = a supervised post-merge step). It adds a 30-min PER-TRIGGER cooldown FLOOR (checked against the SHARED state file BEFORE the helper) — it deliberately does NOT touch `DEFAULT_MIN_HEAL_INTERVAL_S` (the shared 10-min throttle), so the other two triggers are untouched.

**Adding a FOURTH trigger: call this helper — do NOT re-inline the sequence.** The blocks were duplicated (~90 lines each) before #1149; re-inlining is exactly the maintainability hazard this helper removed (a future change to one copy of a DESTRUCTIVE path silently diverges the other).

**Attribution/watchdog scope (#1193, mirrors #1128):** the over-rate reset line carries the distinct `#1193 over-rate self-heal` tag, so — like the #1128 reset — it is NOT matched by `self_heal_reset_grep_pattern` (`#663 self-heal:`) or `capture_rate_defect_grep_pattern_hard`. Threading it into the dev1 self-heal-attribution table (`restart_event_kind_patterns`) + a `#1193 grabber OVER-RATE` dev1 alert watchdog (the #1128 pattern) is a FOLLOW-UP for when the cam2 canary graduates — not shipped here, since the reset is default-OFF and both `frozen_leg`/`self_heal_reset` are already report-only (#914), so the attribution gap is cosmetic while the canary is armed.

## Trigger-specific wording lives in a struct, not in copied log lines
`SelfHealMessages { tag, critical_prefix, defect_word }` + the two `pub const`s carry the ONLY per-trigger differences; the 4 pure `*_message` builders (`throttled_message`, `save_state_failed_message`, `reset_success_message`, `reset_failed_message`) build every emitted line. To add a trigger, add a const — never a new copy of the format strings.

## The emitted strings are BYTE-ANCHORED by dev1 watchdogs — never drift them
No test reads `src/main.rs` source. Instead the emitted log lines are grep-anchored:
- `tests/harness_capture_rate_guard.rs` — `capture_rate_defect_grep_pattern_hard` keys on the substring `#663 self-heal: USB reset attempt` (and `#656 …DEFECTIVE`, `#971 …CHRONIC`).
- `tests/harness_self_heal_attribution_895.rs` — `self_heal_reset_grep_pattern` matches the same reset-success line.

Both test `scripts/lib/*` grep patterns against HARDCODED samples of these lines. A wording change that drops an anchored substring (a) makes the dev1 watchdogs stop matching real journal lines — SILENTLY — and (b) fails those tests at CI. Preserve the substrings on ANY change; the builder unit tests in `capture_rate_selfheal.rs` assert the full lines byte-for-byte to catch drift early.

## The reset is INJECTED (Tier-0 testability); the caller owns store/pending
`reset: impl FnOnce(&str) -> anyhow::Result<()>` — production passes `perform_usb_reset`; tests pass a fake so the sequencing is unit-testable at crate-root default features WITHOUT firing a real USB re-enumeration. The helper returns `Some(SELF_HEAL_EXIT_CODE=77)` on a successful reset, `Some(SELF_HEAL_RESET_FAILED_EXIT_CODE=78)` on a failed one, `None` on Healthy/Throttled. Each caller applies `running_capture.store(false, Ordering::Relaxed); pending_self_heal_exit_code = Some(code)` only on `Some` — do NOT move store/pending into the helper (the caller also owns the `is_none()` double-reset guard).

## Verifying a change (Tier-0 — main.rs is not locally compilable, CI is its first compile)
`cargo fmt --all --check` (parses the whole crate) + a standalone `rustc --edition 2021 --test` replica of the builders + a byte-fidelity diff of builder output vs the pre-change strings extracted from git. `capture_rate_selfheal` is a DEFAULT-feature module, so its `#[cfg(test)]` tests DO run on CI `cargo test`.
