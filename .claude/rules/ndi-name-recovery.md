---
paths:
  - "scripts/strih_mv_scenes.py"
  - "scripts/set-ndi-mapping.py"
  - "scripts/obs_phase2.py"
  - "scripts/lib/ndi-name-selfheal.sh"
  - "tests/python/test_strih_mv_scenes_reattach_1158.py"
  - "tests/python/test_ndi_mapping_heal_1158.py"
  - "tests/python/test_obs_phase2_reenforce_ndi_name_1158.py"
  - "tests/harness_ndi_name_selfheal_1158.rs"
---

# NDI `ndi_source_name` recovery — an EMPTY name is a PERMANENT wedge the in-loop watchdogs can't fix (#1158)

The whole reason this recovery layer exists (live-confirmed on strih 2026-08-20):

**An EMPTY `ndi_source_name` STOPS the DistroAV receiver thread** — `ndi_source_update` logs
`No NDI Source selected; Requesting Source Thread Stop.` and the thread exits. So the in-loop
`#767` (connected-but-silent) and `#1096` (graceful-disconnect) auto-rebind watchdogs — which live
INSIDE the running receiver loop — can NEVER fire for an empty-name input. It is a permanent black
wedge until a NAME is re-applied (the owner had to open Properties / re-run set-ndi-mapping by
hand). This is fundamentally OUTSIDE vendored reach — the fix is at the NAME-managing layer (the
E2E harness), never in `vendor/distroav`. The connected-but-silent receiver-STALL class (frames
stop while the sender is healthy) IS already covered by #767+#1096 — see
`.claude/rules/distroav-receiver-lifecycle.md`; do NOT add a second vendored auto-revive for it.

## WHO empties the name: the reattach CLEAR-then-SET (the #1114 leave-empty), now fixed

`strih_mv_scenes.py reattach()` (the E2E harness's per-input reconnect nudge) is a CLEAR-then-SET:
it SETs `ndi_source_name=""` (forces a fresh receiver) then re-applies the name. #1114 deliberately
LEFT it `""` when the sender vanished from the DistroAV finder during the clear-settle (to avoid the
#795 mangle — SetInputSettings of a name absent from the finder corrupts it). That leave-empty was
the permanent wedge. **#1158 changed the vanished-branch to re-enforce the #399 BASELINE** (not the
stale bound name — cam1's was `CAM1 (30p)`, undiscoverable garbage; only the baseline `CAM1 (usb)`
recovered it) when the baseline IS discoverable, else leave `""` + a loud `#1158` stderr line.
reattach is E2E-only (no live-event path), so the empty-name class is E2E-harness-induced.

## ONE shared policy: `obs_phase2.reenforce_ndi_name` — discoverable → set → read-back-verify → else OFFLINE

The single home for the recovery policy so the callers can never disagree (the way #399 and #1114
once did). `reenforce_ndi_name(ws, input, desired) -> REENFORCE_{HEALED,OFFLINE,VERIFY_FAILED}`:

- NEVER `SetInputSettings` a `desired` absent from `_ndi_source_list(ws, input)` (the DistroAV
  finder) → returns OFFLINE (avoids the proven #795 mangle). An offline baseline is a real rig
  degradation → the caller screams / fails loud, never a silent retry.
- After setting, READ IT BACK — a mismatch is `REENFORCE_VERIFY_FAILED` (a mangle caught LOUD),
  never a false HEALED.

Three consumers of the ONE primitive: (a) `strih_mv_scenes.reattach()`'s vanished-branch (baseline
from `set-ndi-mapping.baseline_sender_for` via the lazy `_baseline_sender_for` importlib load — the
#399 FULL_MAP is the single baseline authority, never a hardcoded `CAM{N} (usb)`); (b)
`set-ndi-mapping.py --heal` (`heal_active_mapping`, heals **differs-from-baseline** not empty-only —
a #795 mangle leaves a DRIFTED non-empty name #1096 can't rebind either; skips correct inputs;
`_heal_exit_code` = 0 healed / 1 verify-failed / 3 nothing-healable); (c) `scripts/lib/
ndi-name-selfheal.sh` → `recording-e2e.sh [4c/8]`.

## The [4c/8] self-heal MUST be called in an `if` (the #1133 set-e class)

`recording-e2e.sh [4c/8]` runs under `set -euo pipefail`. `ndi_name_selfheal_run` (→ `set-ndi-mapping
--heal`) exits NON-ZERO when nothing was healable (exit 3), so a BARE call would abort the whole run
(`ci-testing-gotchas.md` #1133). It is called in an `if`-condition, which suppresses `set -e` inside
it. On exit 0 (healed ≥1) the loop re-samples after the settle; otherwise the normal retry/abort
proceeds (the `#1158` log lines already surfaced the reason). Keep it in an `if`, never a bare
statement or a `|| true` tail that discards the "did it heal?" branch.

## Testing (all Tier-0, no cargo compile)

The python pieces (`reenforce_ndi_name`, `heal_active_mapping`, `_heal_exit_code`,
`baseline_sender_for`, reattach's vanished-branch) run under `pytest` locally with fake-WS/fake-op
stand-ins — a genuine RED→GREEN. The sourced lib's env-seam (`NDI_NAME_SELFHEAL_CMD`) + its #1133
`set -e` safety are a `tests/harness_ndi_name_selfheal_1158.rs` (CI) mirrored by a direct
`bash -c 'set -euo pipefail; . lib; if ndi_name_selfheal_run …'` locally. The default heal path
(`python3 set-ndi-mapping.py --heal` against live OBS) is not offline-testable; the seam is.

## The reattach CLEAR itself SELF-INFLICTS the empty-name wedge on a COLD/STALE finder (#1197)

The #1158 recovery above assumed something ELSE empties the name. #1197 found reattach CREATES the
wedge, live-confirmed by a strih OBS log (gh run 32743557703, 2026-08-24): the CLEAR-then-SET's
up-front finder pre-check can pass on a STALE DistroAV listing (the finder still lists a sender that
is mid-deploy-bounce), so the CLEAR fires (`SetInputSettings ndi_source_name=''` →
`ndi_source_update … No NDI Source selected; Requesting Source Thread Stop`), and by the SET-back's
re-check the sender has dropped → set-back skipped → the input is LEFT EMPTY. Near-deterministic,
because a sender is ALWAYS absent from the finder during its own `[2/8]`/`[2b/8]` deploy bounce.

- **reattach must NEVER leave `""` — RESTORE the original bound name in the vanished-branch when the
  baseline is also offline** (`strih_mv_scenes.reattach()`, #1197). A non-empty name → the receiver
  thread RESTARTS (recoverable by #1096 / the finder-warm poll), vs `""` = a guaranteed stopped-thread
  wedge — the strictly-lesser evil, accepting a possible #795 DRIFT the baseline re-enforce corrects.
  On `REENFORCE_VERIFY_FAILED` the input already holds the just-set (discoverable) baseline, so
  RETURN there — do NOT then blind-set the known-absent original over it (a pointless #795 mangle).
- **The RECOVERY primitive for a COLD finder is a bounded discovery-WAIT, not a one-shot heal:**
  `set-ndi-mapping.py --heal-wait <s>` (`heal_wait_active_mapping` — pure, injected now()/sleep(),
  Tier-0 pytest-able) polls the finder for each active input's #399 baseline to become discoverable,
  then re-enforces via the SAME `reenforce_ndi_name` policy (never blind-sets an absent name). It is
  wall-clock-bounded and early-exits on a warm finder (~one WS round-trip). Wired via the WARN-only
  `mv_reverify_finder_heal_wait` runner (`scripts/lib/mv-reverify-escalate.sh`, seam
  `MV_REVERIFY_HEAL_WAIT_CMD`) at TWO sites: `mv_reverify_or_escalate` after the #1093 force-kill
  restart (ALL active inputs, before the re-check — the fresh OBS's finder is cold) and
  `mv_reverify_resolve_wait` per-camera (before the pixel poll — recovers an already-emptied leg).
- **WARN-only lives at the HELPER, not the call shape (#1133):** the runner's `printf|sed` log is
  `|| true`-guarded and its harness has a `run_under_set_e` case (sources under the real
  `set -euo pipefail`, calls the runner as a BARE statement, asserts the next line runs) — a
  `set -uo`-only harness is blind to a set-e abort.
