---
paths:
  - "scripts/dantesync-version-gate.sh"
  - "scripts/dantesync-gate.sh"
  - "scripts/bundle_state_gather.py"
  - "scripts/bundle-state-server.py"
  - "scripts/version-integrity-gate.sh"
---

# Reading dantesync's own VERSION (not its offset/lock state) — #862

`dantesync` has **no embedded Windows VersionInfo resource** (its own `build.rs` sets none), so
the trick `bundle-state-server.py`'s `ndi_runtime_version()` uses for the NDI runtime DLL
(`Get-Item -LiteralPath ... | .VersionInfo.FileVersion`) reads back EMPTY for `dantesync.exe` —
do not reach for it. The version is only ever printed to dantesync's OWN startup log, once per
(re)start:

- **Linux** (console mode, captured by journald under systemd): `"DanteSync v<ver>"`.
- **Windows** (`--service` mode, its own log file `C:\ProgramData\DanteSync\dantesync.log`,
  1MB-rotated to `.old` on the NEXT restart): `"Service Started: v<ver>"`.

**Always take the LAST match, never the first** — a box upgraded and restarted more than once
still carries its OLDER startup line further back in the journal/log, and a naive first-match
grades a stale prior binary's version as "current" (the exact drift that went undetected until a
failed run's post-mortem, `#851`). See `dantesync_version_from_log` in both
`scripts/dantesync-version-gate.sh` (bash, `grep -oE ... | tail -1`) and
`scripts/bundle_state_gather.py` (python, `re.findall(...)[-1]`) for the two implementations of
the same rule.

## Source `version-integrity-gate.sh` for `state_json_value` alone — its guard makes this cheap

`version-integrity-gate.sh`'s `BASH_SOURCE[0] != $0` source-guard returns BEFORE it sources
`drift-guard.sh` or runs its own `main()` — so `. "$HERE/version-integrity-gate.sh"` from a
SIBLING gate script pulls in only the pure helpers declared above that guard
(`state_json_value`, `compare_args_from_state`, `genlock_build_sha_from_state`, the `#826`
verdict functions) with none of drift-guard's much larger surface. Reuse `state_json_value FILE
KEY` to read ANY single flat key out of the SAME `bundle-state.json` a Windows box's standing
`:8899` server already serves — never re-derive a JSON-key parser per gate.

## `CAMBOX_OFFLINE_ACK` / `rig-fleet.txt` is generic over ANY node name, not just cams

`scripts/lib/cambox-offline-ack.sh`'s `cambox_offline_ack_is_acked`/`_reason`/`_effective` match
on a bare string name with no cam-specific validation — reuse it verbatim for excluding
`imag-nb`, `dev1`, `strih`, `stream`, or any future non-cam node from a fleet-wide gate. Never
invent a second offline-exclusion mechanism; the "reachable but reachable != healthy" decision
matrix (`cambox_offline_ack_decide`) and the repo-level `rig-fleet.txt` default file are already
the one shared answer to "how does a knowingly-offline node avoid failing a gate".

## Pin vs relative parity — pick per signal, don't default to one shape

Two DIFFERENT comparison models exist in this repo for "does the fleet agree" and picking the
wrong one silently weakens the gate:

- **Fixed PIN compare** (`dantesync-version-gate.sh`'s `DANTESYNC_VERSION_PIN`,
  `verify-device.sh`'s `NDI_VERSION_PIN`) — for a component that upgrades RARELY and
  DELIBERATELY (a maintainer bumps the pin as part of the upgrade). Catches a fleet that
  uniformly agrees on a STALE version, which a peer-only compare would miss.
- **Relative cross-box parity, no fixed pin** (`drift-guard.sh`'s `genlock_build_sha` parity
  engine) — for a component whose "correct" value changes on every build (a commit SHA) and so
  has no fixed value to pin against; the only checkable invariant is that every box agrees.

The camera-box BINARY's own version (`1.7.0-dev.NNN`, continuously deployed) is the SECOND
shape — filed as `#875`, a deliberate follow-up split from `#862` (dantesync) specifically
because the two signals need different comparison models, not just a different data source.
