---
paths:
  - "scripts/obs_burn_filter.py"
  - "scripts/rig-mode.sh"
  - "scripts/recording-e2e.sh"
---

# The measurement-burn target set is ENUMERATED from OBS reality, never a static / CAMERA_ACTIVE_SET list

Every place that turns the QR measurement burn OFF, CHECKs it off, or RESTOREs it off must derive
its OBS-input target set by **enumerating the live OBS** (`GetInputList` → every `ndi_source`
input), NOT from a fixed list and NOT from `CAMERA_ACTIVE_SET`. This is the burn-side counterpart to
`camera-active-set.md`, and a case where deriving from `CAMERA_ACTIVE_SET` is the **bug**, not the
fix: an input can be ON AIR while OUTSIDE the active set (a retired camera's grabber feeding a live
scene, a leftover `phase2-probe-src`), so a `CAMERA_ACTIVE_SET`-derived burn list can never see it.

## The shared enumerator seam (#938/#1011) — use it, never re-invent

`scripts/obs_burn_filter.py` owns it (it already had the `_conn`/`_rpc`/`_genlock_burn`/
`compute_burn_on` WS plumbing — extend it, don't build a parallel path):

- `obs_burn_filter.py sweep-check --host <ip>` → enumerates every `ndi_source` input, prints a JSON
  array of `{input, burn_on, genlock_burn, ...}` on **stdout**, human lines on **stderr**; exit 1 if
  any input renders a burn.
- `obs_burn_filter.py sweep-off --host <ip>` → clears `genlock_burn=false` on every ndi input that
  has it ON (idempotent); exit non-zero if any still renders.
- Pure `ndi_source_input_names(input_list)` at module scope = the Tier-0-testable core (unit-tested
  against a multi-input fake WS in `tests/python/test_obs_burn_filter.py`).

Consumers (all ADDITIVE — the pinned per-box PROGRAM-input loops stay as the fast path + safety net,
so no static anchor is touched): rig-mode `toggle_burn` EVENT path + `event_mode_assert` item-3;
recording-e2e cleanup burn-clear + `[0/8]` normalize. The burn ON path stays pinned-only by design
(you only ever turn ON the specific program input a recording captures).

## FAIL CLOSED — a failed enumeration is NOT "clean" (the review lesson)

`GetInputList` can fail (WS error, a #328 timeout raise). A live rig OBS always has ndi inputs, so an
empty result means "could not enumerate", never "no burns". `_all_ndi_inputs` returns `None` on
failure (distinct from `[]`); the sweep actions return `SWEEP_ENUM_FAILED(2)`; and
`event_mode_assert` injects a failing `"<box>:__sweep_unreachable__"=true` sentinel so
`event_assert.py::burns_off_ok()` fails the EVENT contract. Never let an un-enumerable box pass the
burns-off contract on the pinned inputs alone — that silently re-opens the exact leak (guard class
#246/#844; live 2026-08-07 pre-broadcast).

## Anchor-collision lesson (these two scripts are heavy static-anchor territory)

A NEW consumer of a shared array must NOT reuse the exact anchored loop-header literal a test pins by
`.matches(...).count()`. The cleanup sweep first wrote `for _hbs in "${BURN_TARGETS[@]}"` — a THIRD
occurrence — and broke the #252 "exactly 2 loop headers" guard (`harness_recording_e2e_paths`). Fix:
iterate the box IPs directly (`for _swpair in "strih=$STRIH" "stream=$STREAM" "imag=$IMAG_IP"`) —
you only need the per-box IP, not the shared array's triples. And make WIRING guards anchor on the
INVOCATION form (`obs_burn_filter.py" sweep-off`), never the bare token, which a comment can satisfy.
