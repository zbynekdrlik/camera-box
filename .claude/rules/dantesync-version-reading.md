---
paths:
  - "scripts/dantesync-version-gate.sh"
---

# Reading dantesync's own VERSION (not its offset/lock state) — #862

**Corrected 2026-07-30 (follow-up fix) — the ORIGINAL version of this note was factually wrong.**
It claimed dantesync has no readable version on Windows and must be parsed from a startup log
line. That premise was never verified live, and the resulting gate (`dantesync-version-gate.sh`)
shipped hard-blocking every E2E run: `journalctl -u dantesync` never actually carries a version
line on Linux (cam1-4, imag-nb, dev1 all returned ""), and the strih/stream bundle-state servers
deployed at the time never picked up the new `dantesync_version` key either — 7 of 8 boxes read
UNKNOWN on every run. See the #862 supervisor-verification comment for the full incident.

## The actual answer: `dantesync --version` answers on EVERY platform

```
$ dantesync --version                                    # Linux / dev1, on PATH (/usr/local/bin)
dantesync 1.8.25
$ ssh newlevel@<strih-or-stream-ip> '"C:\Program Files\DanteSync\dantesync.exe" --version'
dantesync 1.8.20
```

Confirmed live 2026-07-30 across cam1, imag-nb, dev1 (bare `dantesync --version`, on PATH) and
strih/stream (the full quoted exe path over SSH — **OpenSSH-for-Windows runs the command via
`cmd.exe` directly; no PowerShell wrapper is needed**, unlike several OTHER Windows facets this
repo reads via `powershell -NoProfile -Command "..."`, e.g. `bundle-state-server.py`'s
`ndi_runtime_version`/`port4455_owner`). One uniform reader
(`dantesync-version-gate.sh`'s `read_dantesync_version_output`) now covers every node kind — no
journal parsing, no bundle-state coupling, no per-platform special-casing beyond the exe path
itself. `dantesync_version_from_version_output` parses the `"dantesync X.Y.Z"` stdout (last match
wins, defensive-only — there is no real multi-line noise expected from a single `--version` call).

**The bundle-state additions this gate originally made are REVERTED**, not merely unused:
`scripts/bundle_state_gather.py`'s `dantesync_version_from_log` + the `dantesync_version` kwarg on
`build_bundle_state`, and `scripts/bundle-state-server.py`'s `read_dantesync_log` +
`DEFAULT_DANTESYNC_LOG_FILE` + `--dantesync-log`, are all gone. Nothing else in the repo ever
consumed that key (verified by grep before removing) — leaving it half-wired (present in the
gather code, absent from the deployed servers, unused by the gate) was exactly the kind of
misleading dead path that caused this incident in the first place. If a genuine future need for a
dantesync-log-based read ever arises, that is new work with its own live evidence, not a reason to
resurrect this path from memory.

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

## The lesson: a design comment's factual premise still needs LIVE verification before shipping

The original `#862` design comment stated the Windows-no-version-info premise as settled fact and
built the whole read path on it, without ever running `dantesync --version` against a live box
first. A one-line live check (`ssh ... dantesync --version`) would have caught both broken sources
before any code was written. When a gate's read path depends on "X can only be read this way",
verify that claim against the real target BEFORE designing around it — especially when the
gate is fail-closed and will hard-block real work the moment it's wrong.
