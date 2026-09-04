---
paths:
  - "scripts/camera-box-version-gate.sh"
  - "scripts/dantesync-version-gate.sh"
  - "scripts/version-integrity-gate.sh"
  - "scripts/recording-e2e.sh"
  - "scripts/drift-guard.sh"
  - "tests/camera_box_version_gate.rs"
  - "tests/dantesync_version_gate.rs"
  - "tests/version_integrity_gate.rs"
  - "tests/drift_guard.rs"
---

# Early-gate PIN doctrine — an early gate PINS to the expected release, fail-closed on UNKNOWN; peer parity is a SUPPLEMENT, never a substitute (#1136)

**Owner's standing rule (2026-08-19, repeated across OBS, dantesync, camera-box):** *"early gates
musia odmietnut vobec bezat ak je nieco v random neaktualnej verzii"* — an early E2E precondition
gate must REFUSE to run at all if any component is on a random / stale version. This has now bitten
the SAME way three times (OBS stack, dantesync daemon, camera-box binary), so it is a CLASS rule,
not a one-off fix.

## The doctrine (apply to EVERY early version/preflight gate)

1. **PIN to the expected release — the primary check.** Every node/box/component the gate covers
   must match a KNOWN-GOOD expected value (a fixed pin, or a moving pin read from a source of
   truth). This is what catches a UNIFORMLY-stale fleet, where every box AGREES on an old version.
2. **Peer parity is a SUPPLEMENT, never a substitute.** "Every box agrees with every other" (a
   relative cross-box compare with no external reference) is a good *diagnostic* (it localizes a
   single-box drift), but on its own it PASSES a fleet that is uniformly stale — the exact hole the
   owner keeps hitting. Parity may sit *alongside* a pin, or be the dormant `--no-main-pin`
   fallback, but it must never be the ONLY check.
3. **Fail CLOSED on UNKNOWN.** An unread node, an unreadable pin, a missing state file → the gate
   REFUSES (a distinct non-clean exit), never a silent pass. "I couldn't check" is a failure, not
   an OK.
4. **The pin must not LAG the newest release — an orphan release is a SCREAMING finding (owner,
   2026-08-19).** Pin-not-latest is right for DETERMINISM (a gate compares against a known value,
   with canary discipline for upgrades) — but a pin that sits BEHIND the newest published version of
   its component is the SAME disease inverted: *"naco vydavas novu upravenu ked ju potom nenasadis…
   cely rig ma byt jednotny a najnovsi"*. So **advancing the pin + deploying the fleet is a MANDATORY
   FINAL STEP of every release**, not an optional follow-up. A component that is PUBLISHED (a new gh
   release / a merged main / a new vendored HEAD) but not yet DEPLOYED+PINNED is an *orphan release*
   — and an orphan release must be a LOUD finding (alarm or gate), never a silent state that a
   maintainer discovers by eye weeks later. Live example (2026-08-19): the dantesync DAEMON was
   upgraded fleet-wide to v1.8.46, but `dantesync-tray.exe` on strih+stream sat at Aug 12 — in NO
   gate at all — until the supervisor deployed it minutes later. The tray was an orphan with nothing
   screaming about it.

## The two comparison models — and how to pin a CONTINUOUSLY-deployed component

`.claude/rules/dantesync-version-reading.md` documents the pin-vs-parity split; the #1136 refinement
is that a "continuously-deployed, no canonical value" component is NOT an excuse to drop the pin:

- **Fixed pin** — for a component that upgrades RARELY + DELIBERATELY (a maintainer bumps the pin as
  part of the upgrade): `dantesync-version-gate.sh`'s `DANTESYNC_VERSION_PIN`,
  `verify-device.sh`'s `NDI_VERSION_PIN`.
- **Moving pin** — for a component deployed on almost every PR (`camera-box` `1.7.0-dev.NNN`): pin to
  a SOURCE OF TRUTH that advances automatically WITH the deploy, so pin and deployed reality move
  together and there is no stale-pin spurious-fail window. `camera-box-version-gate.sh` (#1136) reads
  the pin from `git show origin/main:Cargo.toml`, and the push-to-main auto-deploy (ci.yml
  `deploy-fleet`) pushes that same binary to the fleet — so a merge advances both at once. This is
  what dissolves the old #875-header objection ("a dev build has no stable value to pin against"):
  it has one — origin/main — the moment a deploy keeps the fleet on it.
- **Relative parity, no pin** — ONLY legitimate when the value is genuinely unique-per-build with no
  external truth AND a pin is impossible (`drift-guard.sh`'s `genlock_build_sha`). Even then, prefer
  pinning the deployed SHA to the newest main commit that produced it (see #1137) over bare parity.

## Detecting an orphan release — pinned/deployed vs NEWEST published (per component)

Where feasible, a gate (or a sibling watchdog) compares what is PINNED/DEPLOYED against the NEWEST
PUBLISHED version of the component and reports lag LOUDLY. Whether that lag HARD-GATES or ALARM-
REPORTS is a per-component call — justify it:

- **camera-box (this gate) — the REFERENCE case, orphan-PROOF by construction.** The pin IS the
  newest source of truth (`origin/main` Cargo.toml), and the push-to-main auto-deploy (ci.yml
  `deploy-fleet`) deploys that same version. So the pin can never lag main, and any deploy gap (main
  advanced, a box not yet on it) makes fleet != pin → the E2E pin gate SCREAMS (exit 20). A
  camera-box orphan release is therefore structurally impossible AND self-alarming — no extra lag
  check is needed. This is the shape every other component should reach.
- **dantesync (fixed pin) — CAN lag; add a lag ALARM.** `DANTESYNC_VERSION_PIN` is a hand-bumped
  value, so it can sit behind the newest dantesync gh release (the exact orphan the owner named).
  A `gh release view` compare of the pin against the newest dantesync release should alarm on lag.
  ALARM-report (not hard-gate) is the right severity here: the daemon upgrade is a deliberate canary
  rollout, so a lag is "a release is waiting to be rolled", not "the rig is broken now" — but it must
  be LOUD, and the roll must actually happen (mandatory final step, doctrine 4). This is filed work.
- **genlock vendored bundle — DEPLOYED lags newest vendored HEAD.** The newest "release" is the
  newest main commit touching `vendor/**`; comparing the deployed `genlock_build_sha` against it is
  exactly #1137. Report→fail with a loud "pending vendor commits" escalation (the coordinated-restart
  deploy means a hard block on every E2E is too blunt — justify in #1137).

## Live audit of every early gate (2026-08-19, #1136 + owner's 2026-08-19 orphan-release widening)

| Component / gate | Model | Pin? | Orphan-guarded (pin vs newest)? | Status |
|---|---|---|---|---|
| `dantesync` DAEMON — `dantesync-version-gate.sh` (#862) | every node vs `DANTESYNC_VERSION_PIN`; uniform-stale FAILS; UNKNOWN→refuse | **PIN ✓** (fixed — can lag) | **✓ lag ALARM (report-only, #1139)** | **FIXED #1139**: `dantesync_pin_lag_verdict` SCREAMS (report-only) when the pin lags the newest gh release |
| `dantesync-tray.exe` (strih+stream) | **sha256 vs the v{PIN} release asset (report-only alarm, #1139)** — GUI app has no console `--version` (verified live), so pin by bytes not version | **PIN ✓ (sha, #1139)** | ✓ (via the daemon lag alarm — same release tag) | **FIXED #1139 (detection)**: `dantesync_tray_verdict` SCREAMS when the deployed tray != the pinned release asset; folding the tray into the fleet-upgrade roll (so it advances with the daemon) is the remaining orphan-PROOF step |
| `version-integrity-gate.sh` (#123) OBS/genlock bundle | LIVE Windows/imag OBS stack vs vendor/README.md + bundle manifest SHAs; **+ vendor-pin vs main's newest `vendor/**` commit (report-only alarm, #1137; #1292 merge-base scoped)** | **PIN ✓ (report-only vendor pin, #1137)** | **✓ vendor-pin ALARM (#1137)** | **FIXED #1137**: `genlock_vendor_pin_verdict` SCREAMS (report-only — coordinated-restart deploy makes a hard block too blunt) + names the pending vendor commits. **#1292**: the PENDING_LIST range is merge-base-scoped (never a plain ancestry range — see the #1292 addendum below), and a deployed bundle genuinely AHEAD of main on the dev candidate line is classified OK (a recognized release-candidate build) vs ALARM (an unrecognized ORPHAN build), mirroring `genlock_build_drift_report`'s own AHEAD/orphan split |
| `camera-box-version-gate.sh` (#875) | was relative parity only | **PARITY→PIN ✓ (#1136)** | **✓ orphan-PROOF** — pin = origin/main = newest; auto-deploy closes the gap; any lag → gate screams | fixed here |
| `frame-probe` (cam2 painter binary) | **sha256 vs the candidate probe-tools CI artifact, with a pre-gate AUTO-ALIGN that deploys it to cam2 (report-only pin, #1138)** — the frame-probe sibling of `camera-box-parity-align.sh` | **PIN ✓ (sha, #1138)** | **✓ pin+deploy advance together** — the E2E `[0/8]` align deploys the candidate every run (`scripts/lib/frame-probe-parity-align.sh`), so cam2's painter can no longer silently lag | **FIXED #1138 (detection + auto-align)**: `frame_probe_parity_align_before_gate` (E2E `[0/8]`) fetches the clean `probe-tools-linux-amd64` CI artifact, version-guards it (co-located `camera-box-probe --version` == candidate), and deploys it to `/usr/local/bin/frame-probe` when stale via `deploy-fleet.sh --frame-probe` (frame-probe-only mode + the #892 enable-state-preserving lifecycle); the `[1/8]` pin then CONFIRMS against the SAME artifact bytes (`FRAME_PROBE_ALIGN_CI_BIN`). **FLIPPED to a HARD gate (issue 1235)**: the auto-align is now rig-proven (active deploy path + `[1/8]` pin OK observed end-to-end on the first green 7-cam series), so the `[1/8]` pin runs `--frame-probe-hard` and REFUSES (exit 30 lag / 31 UNKNOWN, fail-closed) — the report-only->hard two-step is complete. HARD mode pins against `FRAME_PROBE_ALIGN_CI_BIN` ONLY (an empty one = UNKNOWN->refuse); the byte-different `$PROBE_BIN_DIR` local fallback stays only in the `--no-main-pin` operator-soak report-only branch |
| `recording-verdict-on-imag` sha gate (#1118) | sha256 vs probe-tools artifact | **PIN ✓** | ✓ (sha of the current CI artifact) | clean (refreshed 2026-08-19) |
| `clock-offset-painter-gate.sh` (#326), DanteSync NTP/PTP (#7) | live offset/lock behaviour | n/a | n/a | not version gates |

Every ACTUAL version gate now either pins-and-is-orphan-guarded or has a filed hole ticket. New
early gates get BOTH from day one — do NOT ship a parity-only / deployed-state-only version gate, and
do NOT ship a pin with no alarm when it lags the newest published release. camera-box is the shape to
copy: pin = the newest source of truth + an auto-deploy that closes the gap, so the pin can never
orphan and any deploy gap self-alarms.

## When you add or touch an early gate — the checklist

- Does it PIN to an expected release (fixed or moving), or only compare peers / the deployed state?
  If the latter, it is a #1136-class hole — add the pin.
- Does it fail CLOSED when the value (or the pin itself) is unreadable? An unreadable pin must
  REFUSE, never silently fall through.
- Is the pin a MOVING source of truth that advances with the deploy (so it never spuriously fails a
  correctly-deployed fleet)? For camera-box that is origin/main + the auto-deploy; for a vendored
  bundle it is the newest main commit touching its source tree (#1137).
- Provide a documented ESCAPE (`--no-main-pin` on camera-box-version-gate.sh) ONLY for a deliberate
  pre-merge / operator soak where the target is knowingly not-yet-release — and name who uses it. The
  automatic push-triggered E2E gate NEVER sets the escape, so it always enforces the pin.
- Can the pin LAG the newest published release (an orphan)? If the pin is hand-bumped (a fixed pin),
  add a lag alarm (compare the pin against the newest gh release / main / vendored HEAD) and make
  advancing the pin + deploying the fleet the MANDATORY final step of the release — never a silent
  "published but not deployed" state. If the pin is a MOVING reference (= the newest source of truth)
  with an auto-deploy, the orphan is structurally impossible and self-alarming (the camera-box shape).

## A box AHEAD of the pin during a release train is a NORMAL state, not a defect — the ancestry-range polarity trap (#1292)

`scripts/drift-guard.sh`'s `imag_genlock_range_log` is the #531 dynamic MOVING-pin gate for the
`genlock_build` facet (imag-nb's `--check-imag`, AND strih/stream's `--compare genlock_build_sha=`
— one shared pure verdict, `genlock_build_drift_report`). It is exactly the "camera-box shape" this
doctrine recommends: pin = origin/main's newest vendored-genlock HEAD, never a hand-bumped SHA. But
its FIRST implementation still hit the doctrine's own blind spot: **a MOVING pin gate must account
for the box being legitimately AHEAD of the pin, not just behind it.**

During any active release train, a box can be running a **release-candidate build deployed from
`origin/dev`** — genuinely ahead of `origin/main`, because the PR that carries its content hasn't
merged yet. A naive `git log <box>..origin/main -- <paths>` (plain ancestry range) reads this as
STALE: this repo's two-branch workflow never merges main's own MERGE commits back into dev, so a
merge commit on main is never a git-ancestor of ANY later dev commit — even one whose independent
dev-side lineage already contains that merge's entire vendor CONTENT (the merge's second parent was
an ancestor of the box; the merge commit itself is not). The box read "2 genlock-commit(s) behind
origin/main" while it was in fact a strict content SUPERSET — the exact false-STALE that HARD-BLOCKED
`rig-mode.sh test` (issue-789 gate) during every single release train, i.e. whenever the rig ran a
release-candidate ahead of main (the common, expected case, not an edge case).

**The fix: scope the STALE range to the common ancestor, `git merge-base(box, origin/main)..origin/main`,
never the box's own straight ancestry.** For a box that is a content superset of main, the merge-base
already sits at the point past which main gained nothing the box doesn't already have, so the range
reads correctly empty. A genuinely-behind box (its own merge-base is far back) still shows every real
missing commit — the fix removes ONLY the false positive on the AHEAD direction. The AHEAD direction
itself (`git log origin/main..box -- <paths>`) is then a SEPARATE positive fact, and it needs its own
disposition, not silent tolerance: a box ahead of main that is reachable from `origin/dev`
(`git merge-base --is-ancestor box origin/dev`) is a recognized release-candidate build → OK; a box
ahead of main that is reachable from NEITHER `origin/main` NOR `origin/dev` is an **orphan build** and
must still SCREAM (DRIFT) — this doctrine's own "an orphan release must SCREAM" rule applies just as
much to the AHEAD direction as to a stale fixed pin lagging a published release.

**The generalizable lesson for any early gate comparing a deployed artifact against a moving
`origin/<branch>` pin via straight ancestry:** a two-branch (or any multi-branch) workflow where a
downstream branch's merge commits are never pulled back upstream makes "ahead" and "behind" NOT
simple opposites of a single `A..B` range — a box can be a strict content superset of the pin while
reading non-empty on a naive ancestry range in EITHER direction, depending on which side you put the
merge commit on. Always resolve the actual common ancestor (`git merge-base`) before comparing, and
treat "ahead but unrecognized" as its own DRIFT case rather than folding it into either OK or STALE
by default. (Root cause + the merge-base fix: `scripts/drift-guard.sh`'s `imag_genlock_range_log` /
`imag_genlock_ahead_log` / `imag_genlock_on_dev`; regression coverage against a synthetic two-branch
repo isolating the exact DAG shape: `tests/drift_guard.rs`.)

**A review follow-up (#1292 too) found + fixed the IDENTICAL bug independently in
`version-integrity-gate.sh`'s own #1137 report-only vendor-pin ALARM** — its `genlock_vendor_pin_verdict`
caller computed PENDING_LIST via the same plain `deployed..origin/main` ancestry range. Fixed by
mirroring the same three-function shape scoped to the whole `vendor/` tree: `vendor_pin_range_log` /
`vendor_pin_ahead_log` / `vendor_pin_on_dev` (`scripts/version-integrity-gate.sh`), extending
`genlock_vendor_pin_verdict` with optional AHEAD_LIST/ON_DEV args (rc 30 for both the LAGS and the
ORPHAN reason — report-only semantics unchanged); regression coverage against its own synthetic
two-branch repo: `tests/version_integrity_gate.rs`. Two independent gates sharing the exact same
polarity trap is the tell that ANY new early gate comparing a moving `origin/<branch>` pin via a
plain ancestry range should be audited for this bug up front, not discovered per-gate.
