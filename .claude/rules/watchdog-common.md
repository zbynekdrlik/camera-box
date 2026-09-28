---
paths:
  - "scripts/lib/watchdog-common.sh"
  - "scripts/*-watchdog.sh"
  - "scripts/*-alert-watchdog.sh"
  - "tests/python/test_watchdog_common_1386.py"
---

# The shared dev1 watchdog state glue — `scripts/lib/watchdog-common.sh` (issue 1386)

The dev1 alert watchdogs used to copy their state-file helpers between themselves. The helpers
that were the SAME code in at least three watchdogs now live once in `scripts/lib/watchdog-common.sh`,
sourced next to `scripts/lib/obs-watchdog-decision.sh` (confirm / throttle / `watchdog_notify_key`):
`read_state_field`, `write_state_field` (the read-first copy), `clear_throttle`,
`clear_box_throttle`, `clear_source_throttle`, `source_key`, `netreach_box_alerted`.

## Rules for a new or edited watchdog

- **Source the lib, never copy a helper back in.** `tests/python/test_watchdog_common_1386.py`
  fails on a watchdog that sources the lib and keeps a local copy with the lib's code.
- **Set `STATE_FILE` (and `NETREACH_STATE_FILE` for `netreach_box_alerted`) before calling.** They
  are read at call time. The lib runs nothing at source time and never touches shell options.
- **Callers run `set -uo pipefail`, never `-e`.** `write_state_field`'s
  `[ -f ] && existing="$(grep -v ...)"` returns non-zero when the file holds only that one key, and
  `-e` would exit on it. A NEW watchdog `.sh` opens `set -euo pipefail` for the script hook, then
  `set +e` + `set -uo pipefail` (the family convention, `genlock-lock-facet.md`).
- **A helper whose code differs stays local, defined AFTER the source line.** Bash resolves a
  function at call time, so the local copy wins, and the lib's throttle helpers call it too. The
  documented overrides (pinned by the pytest's `ALLOWED_OVERRIDES` and named in the lib header):
  - the older `write_state_field` that writes through the state file itself when mktemp fails (11
    watchdogs);
  - the read-first copy with literal-newline printf formats (network-reach, obs-liveness);
  - the fixed-temp-path copy (obs-burn-reconcile);
  - `read_state_field` with a different local declaration (ndi-portmap, netcfg-drift).

  Converging the older write onto the lib copy changes the mktemp-failure path, so it is its own
  change, never part of a dedup.
- **What stays per-script:** `log()` (its tag), fetch/probe, alert send, `handle_*`, every
  `--dedup-key` (notify discipline: `watchdog-notify-dedup.md`).
- **Where the source line goes:** after `obs-watchdog-decision.sh`. The exceptions are
  bundle-state and ndi-halving, whose `--help` prints a line range that reaches into the source
  block. There it goes after the last source line, so `--help` stays byte-identical.

## Proving a helper move is behaviour-neutral (the issue-1386 recipe)

1. **`declare -f` identity.** Source the old and the new copy of every watchdog in a clean subshell
   (`set --` first, network tools stubbed) and diff each function's `declare -f`. It must be
   identical for every function the old script had; the only additions are lib helpers it never
   names. Two watchdogs end in a bare `main` with no guard (ndi-portmap, netcfg-drift): drop that
   one line in a scratch copy before sourcing.
2. **`--dry-run` replay in a network-less namespace.** Run
   `sudo -n unshare -n -- sudo -u "$USER" …` from a script FILE. Use three passes (bad, bad, good)
   over the fetch/probe seams (`*_FETCH_CMD` / `*_PROBE_CMD`) with a PATH `curl` stub serving
   `:8899`/`:8898` fixtures and a `date` stub pinned per pass. Compare stdout, stderr and every
   state file, old vs new, byte for byte.

   Prove the replay bites: mutate the lib's `write_state_field` in a scratch copy. A watchdog using
   the lib copy must then differ, and one keeping its own override must not.
3. **`--help` diff**, and the occurrence-count anchor sweep over every test that names the script.
