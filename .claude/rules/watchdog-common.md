---
paths:
  - "scripts/lib/watchdog-common.sh"
  - "scripts/*-watchdog.sh"
  - "scripts/*-alert-watchdog.sh"
  - "tests/python/test_watchdog_common_1386.py"
  - "tests/python/test_watchdog_errexit_1386.py"
---

# The shared dev1 watchdog glue — `scripts/lib/watchdog-common.sh` (issue 1386)

The dev1 alert watchdogs used to copy their state-file helpers, the recovery decision and the
`:8899` bundle-state fetch between themselves. Each now lives once in
`scripts/lib/watchdog-common.sh`, sourced next to `scripts/lib/obs-watchdog-decision.sh`
(confirm / throttle / `watchdog_notify_key`): `read_state_field`, `write_state_field`,
`clear_throttle`, `clear_box_throttle`, `clear_source_throttle`, `source_key`,
`netreach_box_alerted`, `recovery_latch_fires`, `fetch_bundle_json`. The first pass moved every
helper that was the same code (bash `declare -f`) in at least three watchdogs. The follow-up
(slice C) removed the remaining copies whose code differed: every local `write_state_field`, the
recovery one-liner under three names, and the three fetches that only differed by a test seam.

## Rules for a new or edited watchdog

- **Source the lib, never copy a helper back in.** `tests/python/test_watchdog_common_1386.py`
  fails on any watchdog whose copy of a lib helper has the lib's code by `declare -f` (reformatted
  or commented copies too), whether it sources the lib or not. It also fails on any script but the
  lib that defines `write_state_field`, and on any copy or old name of the recovery decision.
- **Set the globals a helper reads before calling it** — `STATE_FILE`, `NETREACH_STATE_FILE` for
  `netreach_box_alerted`, `CURL_TIMEOUT` / `BUNDLE_PORT` / `BUNDLE_PATH` for `fetch_bundle_json`.
  They are read at call time. The lib runs nothing at source time and never touches shell options;
  a global read only by the lib needs the one-line `# shellcheck disable=SC2034` directive above
  it (with the family's `# read by scripts/lib/watchdog-common.sh` note). CI's `shellcheck -S warning scripts/*.sh scripts/lib/*.sh` puts
  the lib on the same command line and follows the `source=` directive, so it stays quiet without
  the directive; a per-file run (every local recipe) warns. Add it whenever a move leaves a global
  read only by the lib (slice C missed 20 of them, a review caught it).
- **A Tier-0 fetch seam is an ARGUMENT, not a copy.** A watchdog that wants a `*_FETCH_CMD` replay
  seam calls `fetch_bundle_json "$ip" MY_FETCH_CMD` (the variable's NAME). A non-empty value then
  runs as `<cmd> <ip>` in place of curl: one executable file path runs whole (spaces allowed),
  anything else is split on whitespace without globbing or quote handling (`bash <fixture>`, never
  a quoted path with spaces). The name is per call, never a shared global, so a variable exported
  for one watchdog cannot redirect another's fetch. audio-mixer, genlock-lock and vb-matrix pass
  theirs.
- **The one state write never drops state and never ends a pass.** `write_state_field` reads the
  other keys BEFORE opening any file for writing, leaves a file grep cannot read alone, renames its
  mktemp file over the state only after the write into it succeeded (a full disk no longer replaces
  the state with an empty file), rewrites in place from the captured keys when mktemp is unavailable
  (a WARNING line: skipping would freeze every confirm counter), prints one ERROR line on any
  failure and always returns 0. Never add a local copy "because this watchdog needs it different" —
  change the lib and its pytest instead.
- **No watchdog runs its pass with errexit on.** Every lib helper is `set -e` safe, but a pass must
  survive a failing probe or assignment. A sourced lib that runs `set -euo pipefail` at source
  time turns -e back on in the caller, and a later `set -uo pipefail` never clears it — only
  `set +e` does. avsync-heartbeat, imag-obs and obs-session ran every pass with -e on until issue
  1386 (CI caught the issue-1070 latency check dying of it); each now runs `set +e -uo pipefail` right
  after its last source line. `tests/python/test_watchdog_errexit_1386.py` sources every watchdog
  and runs a failing command after the source block. A NEW watchdog `.sh` opens
  `set -euo pipefail` for the script hook, then `set +e` + `set -uo pipefail` after its sources
  (the family convention, `genlock-lock-facet.md`). A test that wants to prove a function is
  -e safe re-arms `set -e` after sourcing the watchdog (`harness_imag_obs_restart_storm_1156.rs`).
- **A helper whose code differs stays local, defined AFTER the source line.** Bash resolves a
  function at call time, so the local copy wins. The documented overrides (the pytest's
  `ALLOWED_OVERRIDES`, named in the lib header) are only `read_state_field` in ndi-portmap,
  netcfg-drift, avsync-lineup and vban-rate: it declares `v` in its first `local`, so its code is
  not the lib's (the behaviour is the same).
- **What stays per-script:** `log()` (its tag), the ssh/log probes, the alert send, `handle_*`,
  every `--dedup-key` (notify discipline: `watchdog-notify-dedup.md`). The recovery DECISION is
  `recovery_latch_fires <was_alerted>`; what a recovery DOES (a machine-channel log line, clearing
  the latch) stays in the watchdog.
- **Where the source line goes:** after `obs-watchdog-decision.sh`. The exceptions are
  bundle-state, ndi-halving and vban-rate, whose `--help` prints a line range that reaches into the
  source block. There it goes after the last source line, so `--help` stays byte-identical.

## Proving a helper move is behaviour-neutral (the issue-1386 recipe)

1. **`declare -f` identity — the load-bearing proof.** Source the old and the new copy of every
   watchdog in a clean subshell (`set --` first, network tools stubbed) and diff each function's
   `declare -f`. It must be identical for every function the old script had; the only additions
   are lib helpers it never names. Two watchdogs end in a bare `main` with no guard (ndi-portmap,
   netcfg-drift): drop that one line in a scratch copy before sourcing. To find the candidates,
   group EVERY function name by its `declare -f` hash, not a hand-picked name list (the first pass
   of issue 1386 missed `fetch_bundle_json` that way). When a caller only renames a call (the
   recovery decision), apply the rename to the old `declare -f` and require equality.
2. **`--dry-run` replay in a network-less namespace.** Run
   `sudo -n unshare -n -- sudo -u "$USER" …` from a script FILE. Use three passes (bad, bad, good)
   over the fetch/probe seams (`*_FETCH_CMD` / `*_PROBE_CMD`) with a PATH `curl` stub serving
   `:8899`/`:8898` fixtures and a `date` stub pinned per pass. Compare stdout, stderr and every
   state file, old vs new, byte for byte. A watchdog with a fetch seam replays without a namespace
   (`env -i` + the seam + stubbed network tools): slice C replayed six bodies through each of the
   three seam watchdogs, confirm → WOULD alert → recovery included.

   Coverage is partial: in issue 1386, 20 of 27 replays wrote state. The ssh-probed watchdogs read
   nothing without the rig, so for them step 1 carries the proof. Prove the replay bites: mutate
   the lib's `write_state_field` in a scratch copy. A watchdog using the lib copy must then differ.
3. **`--help` diff**, and the occurrence-count anchor sweep over every test that names the script.
4. **A deliberate behaviour change is its own RED → GREEN pair**, never part of a move: slice C's
   state-write hardening (a /dev/full symlink as the temp file reproduces a failed temp write for
   any user, root included) and the errexit fix each landed as a failing test first, then the fix,
   then the move.
