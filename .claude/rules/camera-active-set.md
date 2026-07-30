---
paths:
  - "scripts/camera-set.sh"
  - "scripts/recording-e2e.sh"
  - "scripts/rig-mode.sh"
  - "scripts/set-ndi-mapping.py"
---

# CAMERA_ACTIVE_SET — every fleet-enumeration consumer MUST derive from it, never a literal range

`CAMERA_ACTIVE_SET` (`scripts/camera-set.sh`, #827) is the ONE declared list of cameras physically
installed and active TODAY (default `cam1 cam2 cam3 cam4`; cam5/cam6/cam7 retired but fully
resolvable — see the header comment in `camera-set.sh`). **Every place that needs "the list of
cameras to check/sample/sweep right now" must derive it from `CAMERA_ACTIVE_SET`, not from a
literal range or its own hardcoded list.** A retired camera's facts (IP, NDI source name, genlock
fps, strih scene/route) stay fully resolvable forever (`camera_resolve`/`camera_strih_route` never
gate on the active set) — only membership in `CAMERA_ACTIVE_SET` decides whether a camera is
currently swept/checked.

## The bug shape, twice now

1. **#827 initial pass** fixed the STATIC-table consumers (`camera-set.sh`'s own `CAMERA_SET`
   default, `recording-e2e.sh`'s `[0/8]` fleet-preflight target list via
   `camera_active_secondary_set()`, the `CAMBOX_SWEEP` default via `camera_active_sweep_pairs()`,
   `set-ndi-mapping.py`'s `--active` flag).
2. **#827 follow-up** (2026-07-28) found THREE more call sites that still enumerated the fleet via
   a **literal `for _n in 1 2 3 4 5 6 7` range**, only subtracting `PREFLIGHT_EXCLUDED_CAMS` (the
   *temporary* acked-offline list from `CAMBOX_OFFLINE_ACK` — a completely different mechanism
   from the *permanent* `CAMERA_ACTIVE_SET` retirement): the `[0/8]` genlock_burn-OFF pre-check,
   the `[1/8]` frozen-camera-gate MV-liveness preflight, and the `[5/8 pre]` live-freeze-watch
   arming. A retired camera is never a member of `PREFLIGHT_EXCLUDED_CAMS` (it was never evaluated
   for exclusion — it's not even in the preflight target list), so it silently stayed in these
   three derived source lists and got sampled anyway. Live hardware gate run 30310110884 proved it:
   the `[1/8]` preflight sampled `NDI cam5`/`NDI cam6`/`NDI cam7` (retired, unplugged, never emit)
   and failed FROZEN on all three.

**The tell:** any `for _n in 1 2 3 4 5 6 7` (or similar hand-rolled numeric range over the camera
fleet) anywhere in `recording-e2e.sh`/`rig-mode.sh` is the bug pattern, regardless of what
exclusion logic sits next to it — an exclusion list built from a DIFFERENT mechanism (acked-offline)
can never substitute for intersecting with `CAMERA_ACTIVE_SET`.

## The fix pattern — two small pure helpers, not three separate inline loops

`scripts/camera-set.sh` now has two derivation helpers built for exactly this:

- `camera_active_excluding <excluded_space_list>` → cam names in `CAMERA_ACTIVE_SET` minus any
  word in `excluded` (for callers that need to iterate individual cam names, e.g. per-camera SSH
  or OBS checks).
- `camera_active_ndi_sources_excluding_csv <excluded_space_list>` → `"NDI cam1,NDI cam2,..."` CSV
  built from the same filtered list (for callers passing a comma-joined source list to
  `frozen-camera-gate.py --sources` or `live_freeze_watch_start`).

Both take the SAME `PREFLIGHT_EXCLUDED_CAMS`-shaped argument the existing call sites already had —
swapping a literal-range loop for one of these two calls preserves the acked-offline exclusion
behavior byte-for-byte, only fixing which candidates are considered in the first place. **When
adding a NEW fleet-wide consumer, reuse one of these two helpers — never re-invent a third
"active minus excluded" loop.**

## Testing without a rig

Every helper in `camera-set.sh` is a pure bash function over `CAMERA_ACTIVE_SET` + an argument —
test it by sourcing the script with an env override and calling the function directly (see
`tests/harness_camera_set.rs`'s `active_excluding`/`active_ndi_sources_excluding_csv` helpers). The
property that matters: (a) the default active set never includes a retired camera in the derived
list, even with an empty exclusion, and (b) overriding `CAMERA_ACTIVE_SET` to re-add a retired
camera makes it flow through to every derived consumer, proving the reversal actually works (not
just a comment claiming it does).
