---
paths:
  - "scripts/camera-box-version-gate.sh"
  - "scripts/dantesync-version-gate.sh"
  - "scripts/version-integrity-gate.sh"
  - "scripts/recording-e2e.sh"
  - "tests/camera_box_version_gate.rs"
  - "tests/dantesync_version_gate.rs"
  - "tests/version_integrity_gate.rs"
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

## Live audit of every early gate (2026-08-19, #1136)

| Early gate (recording-e2e.sh `[0/8]`) | Model | Pin? | Status |
|---|---|---|---|
| `dantesync-version-gate.sh` (#862) | every node vs `DANTESYNC_VERSION_PIN`; uniform-stale FAILS; UNKNOWN→refuse | **PIN ✓** | clean (fleet 1.8.46) |
| `version-integrity-gate.sh` (#123) | LIVE Windows/imag OBS stack vs vendor/README.md + bundle manifest SHAs | **PIN, but vs DEPLOYED-state** ⚠ | **HOLE → #1137**: pins the deployed manifest, not main's `vendor/**` — a uniformly-stale bundle (03cd9c073, 2 undeployed #1097 commits) passes |
| `camera-box-version-gate.sh` (#875) | was relative parity only | **PARITY→PIN ✓ (#1136)** | fixed here — pin to origin/main |
| `frame-probe` (cam2 painter binary) | none (no `--version`, not auto-deployed) | **UNPINNABLE ✗** | **HOLE → #1138**: mtime Aug 5, ~2 weeks stale, no gate reads it |
| `recording-verdict-on-imag` sha gate (#1118) | sha256 vs probe-tools artifact | **PIN ✓** | clean (refreshed 2026-08-19) |
| `clock-offset-painter-gate.sh` (#326), DanteSync NTP/PTP (#7) | live offset/lock behaviour | n/a | not version gates |

Every ACTUAL version gate now either pins or has a filed hole ticket. New early gates get the pin
from day one — do NOT ship a parity-only or deployed-state-only version gate.

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
