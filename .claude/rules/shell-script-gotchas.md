---
paths:
  - "scripts/**/*.sh"
  - "scripts/*.sh"
---

<!-- Moved verbatim from CLAUDE.md (issue 1552). -->

## GOTCHA — a NEW `.sh` file's long header comment must not push `set -euo pipefail` past line 15

`pre-write-script-check.sh` (script-failure-policy.md) blocks a brand-new `.sh` file's `Write` if
`set -euo pipefail` isn't found within the file's first ~15 lines — but this repo's convention for
a new acceptance-gate/guard script (`verify-device.sh`, `clock-offset-guard.sh`, `setup-imag.sh`,
...) is a LONG header comment (checks list, env vars, rationale) BEFORE `set -euo pipefail`, often
80+ lines. Those existing files predate the hook (or were edited incrementally past it); a fresh
`Write` of a NEW file in that style gets hard-blocked (#821, `scripts/verify-imag.sh`'s first
draft). Fix: shebang, a ONE-LINE summary comment pointing at "the extended header below", then
`set -euo pipefail` on/before line ~7, then continue the FULL detailed header comment underneath
it (bash comments work anywhere) — satisfies the hook without shrinking the documentation.

## GOTCHA — a `scripts/lib/*.sh` "_cmd" helper embedded via `$(...)` mid-string gets its trailing newline STRIPPED, gluing it to whatever follows

Several sourced libs (`scripts/lib/v4l2-neutral.sh`, and the same pattern is likely reusable
elsewhere) expose functions that print REMOTE bash TEXT for the caller to embed via
`$(...)` inside a larger ssh command string (e.g. `"...$(some_cmd_fn) more literal text..."`).
**Bash's `$(...)` command substitution UNCONDITIONALLY STRIPS ALL trailing newlines from the
captured output** — a completely standard, well-known behaviour (it's why `$(echo foo)` doesn't
leave a stray blank line), but it is easy to forget when the thing being captured is MULTI-LINE
REMOTE SCRIPT TEXT rather than a simple value. If the helper function's LAST printed statement
relies on its own trailing newline to separate it from whatever literal text the caller
concatenates immediately after the `$(...)` (as `[2/8]`/`[2b/8]` in `recording-e2e.sh` do — the
embedding sits in the MIDDLE of a bigger command string, not at its end), that trailing newline is
gone by the time the text is spliced in, and the function's last command silently swallows
whatever follows as EXTRA ARGUMENTS.

**Live incident (#744/#746, 2026-07-13):** `v4l2_neutral_set_default_cmd`'s last statement was
`v4l2-ctl -d "$V4L2_NEUTRAL_NODE" --get-ctrl=saturation,contrast 2>/dev/null` (no trailing `;`).
Embedded as `"...\n   $(v4l2_neutral_set_default_cmd) \\\n   rm -f /tmp/cbox-burn-cam6.log; ..."`,
the stripped newline glued the two together into ONE command line:
`v4l2-ctl ... --get-ctrl=saturation,contrast 2>/dev/null rm -f /tmp/cbox-burn-cam6.log` — v4l2-ctl
errored `unknown arguments: rm`, and the intended `rm` never ran at all. This reproduced live on a
real gate run (29265311504) and was only caught because the log showed the exact "unknown
arguments: rm" text — a purely LOCAL `bash -n` syntax check on the reconstructed command string
does NOT catch this class of bug (gluing valid-looking tokens onto a command's argv is still
syntactically valid bash; it's a semantic error, not a parse error).

**A subtler variant, if what follows is ALSO a bare `VAR=value` assignment (no command name), not
an external command:** bash then treats the WHOLE glued sequence as a "prefix assignment before a
command" if a real command eventually follows on the same unterminated line — which sets the
variable ONLY in that ONE command's temporary environment, NOT persisting in the calling shell, so
a LATER reference to that variable reads as unset/empty. This is easy to miss because it doesn't
error at all; it just silently produces the wrong (empty/default) value downstream.

**Fix, and the rule going forward for any NEW `_cmd`-style helper meant for mid-string embedding:**
end the function's LAST printed statement with an explicit `;` (e.g.
`'v4l2-ctl -d "$V4L2_NEUTRAL_NODE" --get-ctrl=saturation,contrast 2>/dev/null;'` as the final
`printf` argument) — the literal `;` character survives the newline-strip and correctly terminates
the statement regardless of what the caller concatenates immediately after it, whether that's
another bare assignment, a real command, or nothing at all (a harmless trailing `;` at the very
end of a script is valid bash). **Test this class of bug functionally, not just with `bash -n`:**
reproduce the caller's EXACT embedding shape (a fake stand-in binary on `$PATH` logging its argv +
a marker file a "next" command must remove) and assert the following command actually ran as its
own statement — see `tests/harness_v4l2_neutral_744.rs`'s
`set_default_cmd_embedding_never_glues_the_following_command_746` /
`resolve_node_cmd_embedding_never_glues_the_following_command_746` for the pattern.
