---
paths:
  - "scripts/latency_pins_verify.py"
  - "scripts/latency_pins_snapshot.py"
  - "scripts/imag_latency_enforce.py"
  - "scripts/latency-pins-baseline.json"
  - "scripts/bundle_state_gather.py"
  - "tests/python/test_latency_pins_verify.py"
---

# Reading + verifying per-source genlock latency pins (#1061 / #866 latency half)

## The authoritative pin key is `genlock_latency_ms_src` over OBS WS — NOT bundle-state

To read a source's per-source genlock latency pin, read `genlock_latency_ms_src` via
`GetInputSettings` over OBS WebSocket (the key `latency_pins_snapshot.read_pin` /
`imag_latency_enforce` / `latency_pins_verify` all use). **Do NOT use bundle-state's
`ndi_input_latency` facet** (`http://<box>:8899/bundle-state.json`): `bundle_state_gather.ndi_input_latency_csv`
reads DistroAV's STOCK `latency` setting, which the certified config pins to **0** everywhere
(vendor/README.md). So bundle-state reports `...=0` for every input on a perfectly healthy box —
it is the wrong source for anything about the genlock per-source hold, and seeding a pins baseline
from it would bake in all-zeros. (Confirmed live 2026-08-15: bundle-state said `NDI cam1=0..cam7=0`
while WS `genlock_latency_ms_src` said `cam1=3, cam2=6, cam3=20, ...`.)

## Latency verify-at-start is REPORT-ONLY, never overwrite

Per-source latency is the operator's A/V-align domain (repo memory
`latency-is-user-av-align-domain`), so the OBS-start verify may only REPORT drift against the
committed baseline (`scripts/latency-pins-baseline.json`), NEVER force-overwrite — the opposite of
the #1057 burn sweep-off (a burn is never legitimate operator state, so it IS forced off). A
legitimate re-tune is recorded by updating the baseline in a PR. `latency_pins_verify.py` exits
0=on-baseline / 1=drift(loud, names box+input+got+want) / 2=connect-or-enumeration failure.

## Fail CLOSED on enumeration, honest-None on a per-input read (two different rules)

The imag floor path enumerates live NDI inputs; a failed/malformed `GetInputList` must FAIL CLOSED
(raise → exit 2), never a vacuous "0 inputs ⇒ all OK" (`.claude/rules/burn-target-enumeration.md`
/ `camera-active-set.md`). So the enumerate `GetInputList` runs with `ignore_err=False`, and a
floor box that read zero inputs is exit 2. A per-INPUT `GetInputSettings` read is the opposite: a
missing source/key is an honest `None` (N/A), never a fabricated floor value — mirrors
`latency_pins_snapshot.read_pin`.

## Baseline scope

strih baseline covers the default `CAMERA_ACTIVE_SET` (cam1/cam2/cam3); retired-grabber pins
(cam4..7) are deliberately excluded so a stale pin is never checked. stream pins `NDI 2ME PGM`
with a `{want_ms, tolerance_ms}` band (the A/V-align hold re-tunes around ~923ms; a band absorbs
that while catching a gross revert). imag uses the `_all_ndi_inputs_ms` floor sentinel (always 3).
