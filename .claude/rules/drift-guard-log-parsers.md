---
paths:
  - "scripts/drift-guard.sh"
  - "tests/drift_guard.rs"
---

# drift-guard.sh `*_from_log` parsers must be drain-safe (`|| true`) — issue 514 + #1189

## The convention every `printf '%s\n' "$1" | <consumer>` parser MUST follow

Every log-parsing helper in `scripts/drift-guard.sh` builds a
`printf '%s\n' "$1" | <consumer>` pipeline, and **the pipeline MUST end with `|| true`**
(the sibling `genlock_from_log` / `genlock_latency_ms_from_log` / `pinned_setting` /
`pinned_ndi_min` already do — read `genlock_from_log`'s own comment for the rationale).

WHY: the consumer exits EARLY on a large real log — `awk '{ … exit }'` exits at the first
match, `sed … | head -1` closes after one line — and closes the read end while `printf` is
still writing megabytes (the real imag OBS log is ~778 KB). `printf` then takes SIGPIPE →
`pipefail` yields pipeline status **141** → an unguarded pipeline propagates it → under the
script's own `set -euo pipefail` the caller (e.g. `obs_fps="$(fps_from_log "$obs_log")"`)
dies with **zero output**, fail-closed HARD-BLOCKing the issue-789 rig-mode TEST-entry gate
(`no genlock_build facet [exit=141]`). `|| true` suppresses only the propagated 141 — the
parser's value is fully written + captured BEFORE the early exit, so behavior is unchanged.

This class has recurred twice: issue 514 hardened most parsers with `grep | head -1 || true`;
#1189 caught the four that escaped it (`fps_from_log`, `obs_version_from_log`,
`distroav_version_from_log`, `ndi_runtime_from_log`). **When you ADD a new `*_from_log`
parser, end its pipeline with `|| true` from the start** — do not rely on the input being
small; a `--check-imag` log is large.

## Tier-0 test pattern for a SIGPIPE-under-`set -e` fix (no cargo compile here)

- The bug only manifests under the caller's real `set -euo pipefail` context. `tests/drift_guard.rs`'s
  default `run_sourced` helper is **structurally blind** to a `set -e` abort in the SAME way the
  `-uo`-only harnesses are (the #1133 lesson) — use the `run_sourced_status` helper, which sources
  the script under `set -euo pipefail` and returns the exit code WITHOUT asserting success, so a
  survival test can assert `exit == 0` + a sentinel printed AFTER the command substitution.
- Reproduce SIGPIPE reliably: a **> ~1 MB** synthetic log with the matching lines EARLY (so the
  consumer exits early) and a large filler tail AFTER (so `printf` is still writing past the ~64 KB
  pipe buffer when the consumer exits). Feed it via a temp FILE `cat` inside the bash body — NOT an
  env var (a 1 MB env value blows ARG_MAX at spawn; a shell-function arg has no such limit). See
  `fps_and_version_parsers_survive_a_large_log_without_sigpipe_141_1189` in `tests/drift_guard.rs`.
- Tier-0 local verification (camera-box #477/#557: NO local cargo compile — the Rust test runs on CI
  only): `bash -n scripts/drift-guard.sh`; `shellcheck -S warning scripts/drift-guard.sh` (no NEW
  findings); source the script under `bash -c 'set -euo pipefail; . …; v="$(fps_from_log "$(cat big)")"; echo SENTINEL'`
  and confirm **141-before / 0-after** for each parser; `cargo fmt --all --check` proves the `.rs`
  parses + is formatted.
