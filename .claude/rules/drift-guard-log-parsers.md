---
paths:
  - "scripts/drift-guard.sh"
  - "tests/drift_guard.rs"
---

# drift-guard.sh `*_from_log` parsers must be drain-safe (`|| true`) — issue 514 + #1189

## EVERY consumer of a REMOTE OBS log reads it BOUNDED + marker-family-filtered — a whole-file `cat` over ssh is BANNED (#1151, 14.9.2026)

The drain-safe rule below hardens the PARSERS against SIGPIPE, but it does nothing about the INPUT
size — and an OBS SESSION LOG grows without bound. On **14.9.2026** imag's current OBS log had reached
**1.2 GB / 4.5 M lines** (~90 MB/day of `genlock-fifo audit` + `program-render-audit` +
`multiview-audit` lines since 2026-09-01). Two consumers shipped that WHOLE file over ssh into a bash
variable and the harness bash **SIGSEGV'd (exit 139)**:

- the E2E `[0/8]` projector-vsync reader (`projector_vsync_gather_remote_snippet`, `cat "$f"`) — the
  release E2E aborted right after its `[0/8]` banner. **Fixed in `2e788561b`**:
  `grep -a "projector-vsync: present-vsync" "$f" | tail -n 50`.
- `gather_and_check_imag`'s five-facet gather (`cat "$f"`) — `--check-imag` exited 139, so
  `scripts/rig-mode.sh test` HARD-BLOCKED at its issue-789 TEST-entry gate (`genlock_build … UNKNOWN
  [exit=139]`, fail-closed) and the rig could not enter TEST mode. **Fixed by the bounded pure
  snippet `drift_guard_imag_obs_log_gather_snippet`** (this ticket).

**The rule (a CLASS rule — every future consumer of a remote OBS log obeys it, the #1222
bounded-read discipline generalised):**

1. **Never `cat "$f"` a remote OBS log into a bash variable.** `grep -a` (byte-literal, #1184) the
   MARKER FAMILY the consumer actually grades, so the huge file is STREAMED through grep in bounded
   memory and only the tiny filtered result reaches the variable.
2. **The grep pattern is the UNION of the anchors of every parser fed from that gather.** For
   `gather_and_check_imag`'s five parsers that union is
   `genlock:|projector-vsync:|video settings reset:|fps:` — `genlock:` (note the COLON, so it never
   matches the hyphenated 90 MB/day `genlock-fifo` bulk) covers `genlock_capability_from_log` /
   `genlock_latency_ms_from_log` / `genlock_rt_pin_from_log`; `video settings reset:` + `fps:` covers
   `fps_from_log`; `projector-vsync:` covers `projector_vsync_verdict`.
3. **CAP the filtered stream.** A `tail -n <cap>` suffices when the consumer only wants recent state
   (the E2E projector-vsync fix). When a facet's decisive line is at OBS STARTUP (the fps reset
   block, the genlock arm lines — all first-match parsers), keep BOTH ends: `head -n <cap>` UNION
   `tail -n <cap>` of the (already tiny) filtered stream, so a startup facet is never dropped even
   if a marker family ever grows past the cap. The middle that head+tail then drops is redundant
   repeats, never a first-occurrence line.
4. **Fail closed on empty (#833).** A missing / empty log yields empty filtered output → the parsers
   read UNKNOWN, never a false OK. The caller keeps its `|| true` wrapper so a remote grep-no-match
   (exit 1) never trips its own `set -euo pipefail`.
5. **Put the remote command in a pure snippet function** (`drift_guard_imag_obs_log_gather_snippet`,
   mirroring `scripts/lib/obs-projector-vsync.sh`'s `projector_vsync_gather_remote_snippet`) so the
   marker anchors live in ONE place and are Tier-0-testable without ssh — a big-log fixture (≥ 300 k
   lines) with the decisive line in the MIDDLE (a naive head+tail of the RAW file would miss it; the
   union grep must not) and later lines that a first-match parser must still resolve correctly. See
   `tests/python/test_drift_guard_imag_obs_log_bounded_gather_1151.py` +
   `tests/python/test_projector_vsync_bounded_gather_1151.py`.

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
