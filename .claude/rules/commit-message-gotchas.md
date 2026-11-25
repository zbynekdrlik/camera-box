---
paths:
  - "docs/autopilot-log.md"
  - "Cargo.toml"
  - ".github/**"
---

<!-- Moved verbatim from CLAUDE.md (issue 1552). -->

## GOTCHA — `fix: #N ...` commit prefixes auto-close #N on ANY merge it rides along in

This repo's convention tags commits with `fix: #N <description>` / `feat: #N <description>` as a
plain topic reference. A **regular (non-squash) merge** makes GitHub scan **every individual
commit** in the merged range for closing keywords — not just the merging PR's own body. GitHub's
keyword matcher accepts `fix`/`close`/`resolve` immediately followed by `#N` **even across a bare
colon** (`fix: #458` matches), so a `fix: #458 ...` commit that has been sitting on `dev`,
UNMERGED, for a prior ticket will silently auto-close `#458` the moment it finally rides along in
ANY later PR's merge — even one whose own body only says `Closes #459`/`Closes #461` for
completely different issues. **Incident (2026-07-03):** PR #468 (bundling #459+#461) merged and
GitHub auto-closed **#458** too, even though #458 carried an explicit "stays OPEN until the rework
lands — do not let a PR merge auto-close it early" comment; three earlier `fix: #458 ...` commits
from a prior session's WIP were still unmerged on `dev` and rode along. Reopened + explained in
`gh issue comment 458`.

**Mitigation:** when a `fix:`/`feat:` commit message must NOT auto-close its referenced issue on a
future merge (the work is genuinely partial / multi-PR), phrase it so the keyword and `#N` are NOT
adjacent — `fix(#458): ...`, `fix — #458: ...`, or drop the leading verb entirely (`#458:
description`). Before merging any PR, `git log origin/main..HEAD --oneline` and grep for
`^(fix|close|resolve)[a-z]*:\s*#` to catch a stray reference-only commit that would trigger an
unwanted auto-close.

**Extension — the SAME trap fires from a PR title/body, and NEGATION DOES NOT PROTECT YOU
(incident 2026-07-05, #504/PR #539):** GitHub's closing-keyword matcher scans the merging PR's
OWN title and body too, not just commits — and it is a bare substring match with **no negation
parsing**. A PR body written to explicitly scope a partial/code-only PR — *"...it does NOT close
#504"* — still auto-closed **#504** on merge, because the literal substring `close #504` is
present regardless of the preceding "does NOT". Every commit message that session had already been
checked clean (`git log origin/main..HEAD | grep -iE '(fix|close|resolve)...#[0-9]'` → none), so
the commit-message mitigation above is NOT sufficient by itself — the PR title AND body need the
same check. **Before opening/editing a PR that must NOT close an issue it merely references, grep
the PR title+body text itself** (not just commits) for `(close|closes|closed|fix|fixes|fixed|
resolve|resolves|resolved)\s*#[0-9]` and rephrase any hit — including a NEGATED one — so the verb
and `#N` are not adjacent (e.g. "the live purge for #504 is separate" instead of "does not close
#504"). Recovery: `gh issue reopen <N>` + a `gh issue comment <N>` explaining the accidental
auto-close (see issuecomment-4887235757 on #504 for the template).

## GOTCHA — `git commit -m "..."` with literal backticks in a DOUBLE-quoted message is mangled

This repo's commit messages routinely reference code with backtick-quoted spans
(`` `function_name()` ``, `` `field_name` ``, `` `4/2-1=1` `` style arithmetic) — exactly the style
`gh-cli-recipes.md` already warns about for `gh issue create --body`, but the SAME shell mangling
hits **any** `git commit -m "..."` when the message is a double-quoted string: bash treats each
backtick pair inside double quotes as command substitution and silently replaces it with that
"command"'s (usually empty) output, deleting the quoted text. **Incident (2026-07-07, PR #587):** a
commit message written as `git commit -m "...dropped the now-unused \`step\` param..."` (plain
double quotes, no heredoc) landed with `` `step` `` silently deleted (`Bash completed with no
output` plus a stray `step: command not found` on the terminal) — the word vanished from the
committed message, and every OTHER backtick-quoted span in that same message lost its backticks
too, even where the "command" happened to fail silently instead of printing an error.

**Mitigation:** for ANY commit message containing a backtick, `$`, or `%`, use the same
quoted-heredoc pattern the global commit-conventions template already shows:
```bash
git commit -m "$(cat <<'EOF'
fix(#N): message with `backticks`, $VARS, and 100% safe symbols

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)" -- <exact paths>
```
The single-quoted `'EOF'` delimiter disables ALL shell expansion inside the heredoc body — backticks,
`$(...)`, and `%` all pass through literally. A plain `git commit -m "..."` is safe ONLY when the
message contains none of those three characters; once you write a single backtick, switch to the
heredoc form for the WHOLE message, not just the backtick-containing line. **Never amend/rewrite a
commit that shipped with a mangled message** (`commit-conventions.md` — no history rewrites); the
next commit's message is where you note the correction if it matters.

## GOTCHA — mentioning ANY OTHER `#N` in a commit message (even just as prose context) can BLOCK the commit on that ticket's own design gate

The autopilot design-before-code hook (`block-commit-without-design.sh`) scans the WHOLE `git
commit` command text for issue references — its regex is `(?:^|[\s(/])#([0-9]+)\b`, i.e. ANY
`#N` preceded by whitespace, `(`, `/`, or the string start, not just the commit's OWN `fix(#N):`
prefix. **Every distinct `#N` it finds must already have a design comment posted, or the commit
is BLOCKED** — including a number you only mentioned in passing to explain WHY a rule exists
(e.g. writing "the standing #836 rule: never widen the tolerance" while fixing `#855`). Live
incident (2026-07-28, `#855`): a GREEN commit's body referenced `(#836: never widen the
tolerance...)` — a completely different, already-resolved ticket, cited only for context — and
the hook blocked the commit demanding a design comment on `#836` too.

**The scan covers the WHOLE Bash call, not just the `-m` message — a `cat >> file <<EOF`
heredoc in the SAME call as a `git commit` trips the gate on `#N` refs the heredoc merely
writes into a FILE** (live: appending a docs/autopilot-log.md entry naming other tickets +
committing it in one call, 2026-08-07; and per the rig-state-inspection rule, the BLOCKED call
runs nothing — the append never happened either). Split file-writing and `git commit` into
separate Bash calls when the written content names other tickets.

**Fix: never write a bare `#N` for an issue OTHER than the one(s) this commit's own design
comment already covers.** Reference the RULE, not the ticket number, in commit-message prose
("the standing gate-strictness rule: never widen the tolerance" instead of "the `#836` rule") —
or, if the number must appear, keep it out of the `(?:^|[\s(/])#\d+` shape (e.g. spelled as
`issue 836` with no `#`, which the regex does not match). This does NOT apply to the commit's OWN
ticket number(s) in the conventional `fix(#N):` / `test(#N):` prefix — those already have (or are
about to get) their design comment via the normal flow; it is specifically OTHER tickets casually
named in the body that trip the gate.
