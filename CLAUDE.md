# Claude Code Guidelines for camera-box

Rust app for embedded NDI cameras (CAM1-4): multi-camera NDI streaming with software genlock + intercom/sidetone audio. Built locally, deployed to the camera devices over SSH.

<!-- Global rules inherited from ~/.claude/CLAUDE.md (managed by airuleset) -->
<!-- PR merge policy, CI monitoring, TDD, two-branch git workflow, test strictness, security, comprehensive logging apply automatically. This file holds ONLY camera-box-specific context — do not duplicate global rules here. -->

## Playbook router

- Rig ops (DanteSync clock, device deploy, recovery) → load `.claude/skills/ops`
- Provisioning / new cam box (build USB → setup-device.sh → verify-device.sh acceptance gate, #448-#454) → load `.claude/skills/provision`
- V4L2 capture controls (colour vs sharp sets, device-state persistence, NZXT CAM4 no-controls, grayscale/tint, the #299 colour-capture chroma metric) → load `.claude/skills/capture`
- Genlock OBS (deployed state, monorepo direction, NDI input mapping, timecode lag) → load `.claude/skills/genlock`
  - Genlock latency is ONE user knob in MS (#235): `OBS_GENLOCK_LATENCY_MS=N` (canonical; `OBS_GENLOCK_RESERVE_MS` is the back-compat alias; prod=3ms). Setting it implies ts-align on; preload is internal/auto-derived. Display: `latency = N ms (≈ M frames)`.
- OBS launch/recovery on strih/stream → load `.claude/skills/obs-ops`
- `--display` HDMI path (connector/phantom-fb detect, upscale cap, capture-dropped counter) → load `.claude/skills/display`
- CI artifacts, Discord notify, probe binary flow → load `.claude/skills/ci`
- E2E zero-loss testing (acceptance criteria, QR harness, reporting scope, active fleet size / `CAMERA_ACTIVE_SET` reactivation) → load `.claude/skills/e2e`
- Rig TEST/EVENT mode switch (#247 `scripts/rig-mode.sh`: pinned QR/burns/genlock per mode, the #246 burn-leak guard) → load `.claude/skills/e2e`
- Recording-verdict QR decode path (fast/robust gate, per-recording burn sets, #186 fixtures) → load `.claude/skills/recording-decode`
- A/V-sync offset measurement (cam2 QPSK marker, `--av-sync`, ring-bias + cluster-pairing gotchas) → load `.claude/skills/av-sync`
- imag-nb swap (install-imag-nb.sh → setup-imag.sh, derived CPU/GPU/IP) → `.claude/rules/imag-nb-provisioning.md` (auto-loads on its `paths:`)
- E2E gate preconditions (DanteSync servo, bundle-state-server) → `.claude/rules/rig-standing-services.md` (auto-loads on its `paths:`)
- CI/workflow concurrency-cancel risk, sourced-bash-test-harness `set -e` leak → `.claude/rules/ci-testing-gotchas.md` (auto-loads on its `paths:`)

## DO NOT DELETE These Files

**NEVER delete `targets.md`** — it contains IP addresses for all deployment targets (Windows and cameras). This file has been accidentally deleted multiple times during PR cleanup. DO NOT remove it.

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

## GOTCHA — two autopilot workers sharing this dev1 checkout WILL interleave on `dev`

`~/devel/camera-box` is a single shared clone with **no git worktree isolation** — every worker's
`git commit`/`git push` operates on the SAME local checkout, the SAME git index, and the SAME
local `dev` branch ref, not just the same remote branch. If the supervisor ever dispatches two
workers into this repo at once (violates `two-branch-workflow.md`'s "dispatch serially — one
active worker per repo", but has happened), their commits interleave on one linear history with
no isolation and no conflict warning.

**Incident (2026-07-04):** worker A (#499+#500, `setup-imag.sh`) and worker B (#505, a GL
PBO-orphan fix) both committed to `dev` concurrently. Worker A protected its pushes by pushing an
exact commit SHA (`git push origin <own-sha>:refs/heads/dev`, never a bare `git push origin dev`)
so B's not-yet-pushed commits weren't dragged to `origin` prematurely — but a `git commit` A ran ON
TOP of B's already-advanced local HEAD unavoidably included B's ancestry on the next push (a push
always carries a commit's full ancestor chain; excluding mid-branch commits needs a banned
force-push). Net result: A's PR ended up also shipping B's fully-complete #505 work, auto-closing
it via B's own `fix: #505 ...` commit title. Harmless here (B's work was genuinely finished +
TDD'd), but in a worse timing it could ship a STILL-IN-PROGRESS body of foreign work through the
wrong PR with no review of it.

**Consequences + mitigations, confirmed live:**

- **A stray untracked/modified file you didn't create can appear in `git status`.** NEVER
  `git add -A`/`git commit -a` — stage and commit ONLY the exact paths you touched, in the same
  breath: `git commit -m "..." -- <path>` (the pathspec form commits ONLY that path's
  working-tree content, ignoring anything else staged, and leaves the other worker's staged
  changes untouched). If a sweep still happens (`git show --stat HEAD` shows a file you never
  edited), `git rm --cached <path>` in a follow-up commit restores it to untracked — never `git
  rm`/delete it from disk, it's someone else's live work.
  - **GOTCHA (confirmed live, 2026-07-11, PR #692 / #684): `git commit -m "..." -- <path>`
    commits that path's FULL CURRENT working-tree content, not just what you `git add -p`'d for
    it.** This matters even OUTSIDE the two-worker scenario, any time you need TWO separate
    commits (e.g. a RED test commit then a GREEN fix commit, `regression-test-first.md`) touching
    the SAME file with only PART of your edits ready for the first commit. Selectively staging
    hunks via `git add -p <path>` (answering `y`/`n` per hunk) then running
    `git commit -m "..." -- <path>` does **NOT** commit only the staged hunks — it commits the
    path's CURRENT ON-DISK state, staged-or-not, silently pulling in the unstaged hunks too. Live
    incident: staging only #682's hunks in `scripts/recording-e2e.sh` via `git add -p`, then
    running `git commit -- scripts/lib/imag-scene-route.sh scripts/recording-e2e.sh` (intending
    to land ONLY the #682 fix), silently also committed the NOT-YET-STAGED #684 final-verify
    block still sitting in the working tree — collapsing two intended separate RED→GREEN pairs
    into one commit. **Never git-history-rewrite to fix this** (`commit-conventions.md`) — the
    clean recovery is a NEW commit pair: temporarily `Edit` the file to REMOVE the
    accidentally-early hunk (recreating the true pre-fix state), commit that as the RED test
    commit, then re-add the removed hunk as its own GREEN commit. To avoid it going forward: when
    you need ONLY the staged hunks of a partially-staged file, commit with **no pathspec at all**
    (`git commit -m "..."`, which commits exactly the INDEX) rather than repeating the file's path
    — the pathspec form is for "commit this path's CURRENT state", not "commit what I staged for
    this path".
- **Before every `git commit`, `git log --oneline -3`** to confirm HEAD is still what you expect
  — if it shows commits you didn't write, the other worker advanced the shared branch under you.
- **A push by EITHER worker pushes the local `dev` ref as it stands**, including the other
  worker's already-made local commits (both share one ref). There is no way to "un-push" someone
  else's commits without a banned force-push/history-rewrite; NEVER `git reset`/force-push to try
  — you'd be mutating a ref the other process may still be relying on mid-operation. If it
  happens, check `gh issue view <N>` for whether it changed which issue(s) a shared PR
  auto-closes (see the GOTCHA above) and adapt the PR body / commit wording rather than fighting
  the git state.
- **GitHub allows only ONE open PR per (head, base) branch pair.** If another worker's `dev`→`main`
  PR is already open when you're ready to push, you CANNOT open a second one — wait for theirs to
  reach a terminal state (poll `gh pr view <N> --json state,mergedAt`) before pushing, or your
  commits just fold into their PR's diff.
- **A later push cancels an in-flight `linux-genlock.yml` run via its
  `linux-genlock-${{ github.ref }}` concurrency group — even if the later push doesn't itself
  touch `vendor/**`** (the group keys on the ref, not the paths). A cancelled run is NOT a build
  proof — re-trigger manually once the ref is stable: `gh workflow run "Linux genlock build
  (vendored OBS + DistroAV, imag-nb parity)" --ref dev`.
- **`linux-genlock.yml` only triggers on push to `dev`, never `main`** (`on.push.branches` is
  `[dev]` only) — after a dev→main merge, main never automatically gets a genlock build. If you
  need proof the exact merged main state compiles, trigger it explicitly with `--ref main`.
- Note the collision plainly in your evidence block/autopilot-log entry, and if a foreign commit
  auto-closed an issue that wasn't yours, explain it via `gh issue comment <N>` for traceability.
  The supervisor should prefer serial dispatch or per-worker `git worktree` isolation for this
  repo going forward.

## GOTCHA — editing `scripts/recording-e2e.sh` (OR `scripts/rig-mode.sh`) can silently break OTHER test files' static anchors

Many separate `tests/harness_*.rs` / `tests/rig_mode.rs` files independently `.find()`/`.split()`
literal substrings/adjacency in `scripts/recording-e2e.sh` OR `scripts/rig-mode.sh` (a banner like
`"[5/8] StartRecord"`, a bare function-name anchor like `.split("do_event()")`, or structural
adjacency like `fi\ntrap cleanup EXIT`) to pin ordering/structure — the same static-string-assertion
model `tests/harness_recording_e2e_cleanup_resilient.rs` (#328) established, now reused across many
unrelated features in BOTH files (`#137` av-restart-sync, `#286` all-cambox burn targets, `#649`
StopRecord ordering, `#524`'s `event_mode_calls_stop_stray_recordings_guard`, etc.). A new comment
or code line you add to EITHER file CAN accidentally (1) duplicate a literal anchor another test's
`.find()`/`.split()` relies on — `.find()` returns the FIRST match in the WHOLE file, and
`.split(X).nth(1)` grabs the segment AFTER the SECOND occurrence of `X`, not the occurrence near
your own edit — or (2) break a textual adjacency two unrelated tests hard-code (e.g. inserting a
line between an `if` block's closing `fi` and the following `trap cleanup EXIT HUP INT TERM` line).

**Confirmed live (#649, 2026-07-10, recording-e2e.sh):** a new `cleanup()` comment containing the
literal text `[5/8] StartRecord` broke 3 tests in `tests/harness_av_restart_sync_gate.rs` (the
`#137` gate, which anchors on that exact string to slice "everything before the main record
step"); and adding new variable declarations directly before `trap cleanup EXIT HUP INT TERM` broke
`recording_e2e_all_cambox_extends_burn_targets_to_every_strih_input_286` in
`tests/harness_recording_e2e_paths.rs` (which hard-codes that the `#286` ALL_CAMBOX block's
closing `fi` is immediately followed by `trap cleanup EXIT`).

**Confirmed live (#722, 2026-07-13, rig-mode.sh — the SAME class, a DIFFERENT file):** a new
comment reading "...sent by `do_event()` AFTER this function returns..." — placed INSIDE
`event_mode_assert()`, BEFORE the real `do_event() {` definition — broke
`event_mode_calls_stop_stray_recordings_guard` in `tests/rig_mode.rs`, which extracts the function
body via `s.split("do_event()").nth(1)`. With TWO occurrences of the literal text `do_event()` (the
new comment, then the real definition), `.nth(1)` grabbed the segment BETWEEN them (a few lines of
comment) instead of the real function body — the test's failure message printed the WRONG slice
(the comment text), which is the tell: if a `.split()`/`.find()`-based test's failure output looks
like the wrong region of the file, suspect a duplicated anchor, not a logic regression. Fix: reword
the comment so it never contains the bare function-name-with-parens text (e.g. "the EVENT-mode
caller" instead of "`do_event()`") when that text sits BEFORE the real definition in the file.

**Confirmed live (#832, 2026-07-27) — the anchor you break can be a test YOU are writing IN THE
SAME PR, not just a pre-existing one.** Adding an explanatory comment right before a call site
(`# #832: recording-verdict-on-imag.sh has its OWN independent IMAG_BOX default...` right before
`"$HERE/recording-verdict-on-imag.sh"`) created a SECOND occurrence of the literal script name —
a NEW test's own `s.find("recording-verdict-on-imag.sh")` then latched onto the comment (the FIRST
occurrence) instead of the real invocation a few lines later, so its assertion window never
reached the actual call. Same failure shape hit a second, unrelated anchor in the same PR:
`rig-mode.sh`'s pre-existing explanatory comment already said `` `scripts/drift-guard.sh
--check-imag` `` (backticked, no `bash ` prefix) several lines above the REAL
`bash scripts/drift-guard.sh --check-imag ...` call — a naive `.find("drift-guard.sh
--check-imag")` grabbed the comment, not the call. Fix in both cases: anchor on a substring that
can ONLY appear at the real call site (the quoted `"$HERE/...").sh"` invocation form, or a
`bash ` / other prefix the comment never uses) — never a bare script/flag name that a nearby
comment could also contain. **The general rule: when you write a NEW static-anchor test against
one of these two files in the SAME commit/PR that adds explanatory prose near the call site,
verify your OWN anchor is unique too — you can self-collide, not just collide with someone else's
existing test.**

**Mitigation:** after ANY edit to `scripts/recording-e2e.sh` OR `scripts/rig-mode.sh`, run the FULL `cargo test` suite —
not just your own new/targeted test file — before pushing (`cargo test # airuleset:build-ok`
locally bypasses the Tier-0 build-block for this one-off check; Tier-0 policy is otherwise
`cargo test --no-run` only, see below). A failure elsewhere in the suite right after touching this
file is very likely a textual collision, not a real regression — grep the failing test's
`.find(...)` argument (or the surrounding slice logic) to see which literal string or adjacency
moved, then reword your new text (or relocate it) so it no longer matches/breaks that anchor.

**Prevention pattern (#675) — ADD new behavior via a sourced helper, never edit the literal
anchor line itself.** When the new logic needs to run right after an EXISTING pinned line (e.g.
"verify camera-box came back active after `systemctl restart camera-box`"), don't touch that
line's text at all — append a call to a NEW function in a NEW `scripts/lib/*.sh` file via command
substitution on the line(s) immediately after it (`$(my_new_helper_cmds "label")`). The static
anchor test suite reads ONLY `scripts/recording-e2e.sh`'s own text, never a sourced lib's — the
function CALL is invisible there (compile-time text), but its expanded OUTPUT still lands in the
final remote command at actual runtime. This adds a whole new capability with ZERO risk to any
existing `.find()`/adjacency assertion, and keeps the new logic in ONE sourced source of truth
(mirrors `rig_test_dropin_clear_cmds` in `scripts/lib/rig-test-dropin.sh`, #309) instead of
duplicating it inline at every call site. See `scripts/lib/camera-box-restart-verify.sh` for a
worked example — 3 call sites (cam1, the ALL_CAMBOX loop, cam2/painter) each gained a poll+retry
step with the ORIGINAL restart lines byte-for-byte unchanged, verified by the full `cargo test`
suite staying green (115/115 binaries, no anchor collisions).

**Variant (#712) — WRAPPING an anchored line's execution mode (not just appending after it) is
ALSO safe, PROVIDED you check every sibling test uses SUBSTRING `.find()`, never a full-line/exact
match.** #712 needed the cam3/4/5/6 ALL_CAMBOX restore loop's ssh call to run BACKGROUNDED
(`( timeout ... ssh ... ) &` instead of a bare foreground call) so 4 boxes restore concurrently
instead of sequentially — this touches the anchor line itself, not just text after it. Before
doing this: `grep -rn '\.find(' tests/*.rs` for every string that could live inside the region
being touched, and confirm each is a `body.contains(...)`/`region.find(...)` SUBSTRING check
(unaffected by a `(`/`) &` wrapper on the same logical command) rather than something that
requires the anchor to be the literal FIRST token on its line or hard-codes exact whitespace. The
new PID-collection + wait logic itself went into a new sourced lib
(`scripts/lib/cambox-parallel-restore.sh`), same as the #675 pattern — only the wrap-in-parens
touched the anchored region directly, and it was verified safe (grep first, then the full
`cargo test` suite green after) rather than assumed safe.

## GOTCHA — one failing test binary makes `cargo test` SKIP the remaining binaries (a second RED hides)

`cargo test` stops scheduling not-yet-started test binaries after a binary fails ("waiting for
other jobs to finish") — so a run that shows ONE failure is NOT a complete accounting: another
already-RED test file later in the schedule silently never ran. Live incident (2026-07-16, #792
session): the full suite showed only `obs_self_heal_install` failing; after fixing it, a SECOND
pre-existing failure surfaced (`setup_imag_guards`, stale since the #783 same-source pivot the
day before). Both had sat unnoticed because the event-mode hotfix sessions never ran the full
suite. Rules: (1) after fixing a failure, ALWAYS re-run the FULL suite — never conclude "now
green" from the first fix; (2) a hotfix session that skips the full suite leaves landmines for
the next session — run it before ending the session even when CI is deliberately not triggered
(count `test result: ok` lines and expect the full binary count, currently ~146).

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

## GOTCHA — `gh pr merge` falsely refuses a green PR as "not up to date"; the direct REST call works

This repo's `dev` branch is **structurally always "behind" `main`** by design: `main` only ever
gains 2-parent MERGE commits from past `dev`→`main` PRs (`Merge pull request #N from
zbynekdrlik/dev`); `dev` itself is a pure linear branch that NEVER pulls those merge commits back
in (confirmed by walking several consecutive `Merge pull request #N` commits' parents — each
merge's own dev-side parent is dev's OLD tip, never main's). So `git merge-base --is-ancestor
origin/main origin/dev` is **permanently false**, and every PR's `mergeable_state` reads `"behind"`
forever, even on a fully green PR with zero real conflicts (`mergeable: true`).

**Incident (2026-07-11, PR #697):** with every required check green, `gh pr merge 697 --merge`
(and `--auto`) both refused: `"the head branch is not up to date with the base branch"`. This is
`gh`'s own CLIENT-SIDE heuristic being overly cautious for this repo's workflow shape — it is NOT
what GitHub's server-side branch protection actually enforces here. The direct REST call — the
EXACT SAME operation the green "Merge pull request" web button performs, **not** an admin/bypass —
succeeded immediately with zero special flags:

```bash
gh api repos/OWNER/REPO/pulls/<N>/merge -X PUT -f merge_method=merge -f commit_title="Merge pull request #<N> from zbynekdrlik/dev"
```

**Never reach for `--admin`** just because `gh pr merge` complains about "not up to date" — that
IS a branch-protection bypass and is banned regardless (`autonomous-quality-discipline.md`). This
`behind` state is a known-harmless artifact of this repo's specific two-branch shape, not a real
staleness problem; the plain REST merge call is the correct, non-bypassing path when EVERY actual
required check is green and `gh pr merge` is merely being overcautious about it.

## GOTCHA — `gh pr edit --body-file` fails with a GraphQL "Projects (classic)" error; use the REST PATCH instead

`gh pr edit 704 --body-file <file>` (or `--body`) fails on this repo with `GraphQL: Projects
(classic) is being deprecated...(repository.pullRequest.projectCards)` and exit code 1 — `gh`'s
GraphQL mutation for editing a PR fetches the `projectCards` field in its response even when you
never touch project cards, and this repo (or org) still has that legacy field wired up. The body
is **silently NOT updated** when this happens (confirmed: re-reading the PR body afterward showed
the OLD text). The direct REST PATCH sidesteps the broken GraphQL response entirely and works
every time:

```bash
gh api repos/OWNER/REPO/pulls/<N> -X PATCH -F body=@/path/to/new-body.md
```

Same family as the `gh pr merge` GOTCHA above (a `gh` CLI convenience wrapper misbehaving on this
specific repo; the equivalent raw REST call is the reliable fallback) — check the PATCH response's
own `.body` (or re-`gh pr view --json body`) to confirm the write actually landed before trusting
it, since a `gh pr edit` failure here is easy to miss (it prints an error to stderr but the exit
code alone doesn't make the silent no-op obvious without a diff-back).

## GOTCHA — a live-triggered E2E gate run can race ahead of a mid-cycle fleet redeploy

If a PR's fix requires a fleet redeploy to actually take effect on the live rig BEFORE the gate
can pass (e.g. a WARN threshold recalibration — #685), the PR's own automatic `pull_request`-
triggered "Full-path E2E" run can start (and fail, against the STILL-stale rig) before the redeploy
finishes — pushing the fix and deploying it is NOT atomic with the CI trigger. Don't chase that
failed run; once the redeploy is verified live (journal/WS read-back), get a fresh REAL verdict.

**CORRECTED 2026-07-12/13 (#717, #719/#726 dispatch) — this section used to recommend
`gh workflow run "Full-path E2E ..." --ref dev` here. DO NOT DO THAT — it is DANGEROUSLY WRONG
for this purpose.** `full-path-e2e.yml` branches `E2E_EXECUTE_VERDICT`/`ALL_CAMBOX` on
`github.event_name == 'pull_request'` — a `workflow_dispatch` run (what `gh workflow run` always
creates) ALWAYS gets `E2E_EXECUTE_VERDICT=0`/`ALL_CAMBOX=0` and stays in the OLD plan-print-only
mode: it never decodes strih/stream, never computes a real verdict, and "succeeds" trivially —
**yet GitHub still posts a check-run with the SAME required-check NAME on the SAME commit SHA**,
which SATISFIES the PR's branch-protection requirement. Following this GOTCHA as originally
written could let a genuinely broken PR merge behind a MEANINGLESS green. The correct way to get
a fresh REAL verdict on an already-pushed commit (no new push, e.g. after a fleet-side fix or an
infra repair with no code diff) is:

```bash
gh run rerun <the-original-pull_request-run-id>
```

`gh run rerun` preserves the ORIGINAL trigger's event context (`github.event_name` stays
`pull_request`), so `E2E_EXECUTE_VERDICT`/`ALL_CAMBOX` are correctly `1` again. Find the run id via
`gh run list --branch dev --workflow "Full-path E2E (recording-based · hardware · self-hosted
dev1)" --json databaseId,event,headSha` and pick the one with `"event": "pull_request"` matching
your commit. Full detail + the "two same-commit `pull_request` runs can disagree for reasons
outside your own diff" corollary: `.claude/skills/e2e`'s own `gh workflow run` section (the
canonical source now — this CLAUDE.md section is kept only as a pointer + a loud warning against
the old advice; do not restore the `gh workflow run` snippet here). Same "manual re-trigger after a
stale/superseded run" IDEA the `linux-genlock.yml` GOTCHA above documents for a cancelled run — but
`gh workflow run` is the WRONG mechanism for this specific workflow; `gh run rerun` is correct.

## Local Build Policy

**Tier 0 (default) — CI builds the deployable binary; local checkouts run cheap checks only.**

CI builds the `camera-box` release binary AND the probe/verdict binaries (`--features probe`)
via two artifact uploads (`camera-box-linux-amd64`, `probe-tools-linux-amd64`). Download and run
the CI artifact — never build locally.

Run locally before every push (**DEFAULT FEATURES ONLY — never `--features probe` / `--all-features`**):
```bash
cargo fmt --all --check
cargo check
cargo clippy --all-targets -- -D warnings   # NO --all-features
cargo test --no-run
```

**Do NOT compile `--features probe` (or `--all-features`) locally — that is what balloons `target/`.**
The `probe` feature pulls heavy deps (`qrcode`, `rqrr`, `image`, `drm`, `lz4_flex`) and 5 extra
`required-features = ["probe"]` `[[bin]]` targets; with `--all-targets --all-features` every worker's
cheap check recompiled all of them into the single shared dev1 `target/`, which has no GC
(rust-lang/cargo#5026) — so it grew to 18 GB and filled the disk (#185). The probe code is
**compile-checked + built ON CI ONLY**: the C++/vendored gate runs on CI (#101) and the probe
binaries are built + uploaded as `probe-tools-linux-amd64` on CI (#192) — local probe compilation
is redundant. Default-feature checks compile only the small appliance crate (`target/` stays in the
**low hundreds of MB**, not GB); `cargo check`/`cargo tree` on default features pulls NONE of the
probe crates.

Heavy builds in CI only: `cargo build --release`, running `cargo test`, `cargo bench`, `--features probe`.

**Make probe logic Tier-0 testable — pure seam at the CRATE ROOT, not in `src/probe/`.**
The whole `probe` module is `#[cfg(feature = "probe")]` (lib.rs), so its tests run ONLY under
`--features probe` (CI only — banned locally). To get a locally-verifiable RED→GREEN on probe
work, extract the PURE logic (geometry, decisions, tables) into a crate-root module that compiles
on default features — the `src/reannounce.rs` / `src/colour_scale.rs` (#367) pattern — and have
the probe-gated code (`src/probe/…`) iterate/call it. The pure module's tests run on default
features; the probe-gated glue (framebuffer blit, ioctl) gets a thin probe-gated test CI runs.
To OBSERVE RED→GREEN on a cheap default-feature test (the Tier-0 hook blocks all `cargo test`
that RUNS), append the one-off bypass: `cargo test --lib <module> # airuleset:build-ok` (or
`--test <file>`).

**No bypass exists for `src/bin/recording-verdict.rs` or any `src/probe/*.rs` file itself** — the
bin has `required-features = ["probe"]` and every file under `src/probe/` is behind the SAME
feature gate, so `cargo check`/`clippy`/`test` on DEFAULT features doesn't even attempt to compile
them (confirmed live, #632/#638: `cargo test --lib probe::qr::` / `qr::tests::` / `grouped_gate`
all silently match "0 tests" — NOT a passing run, just nothing to run). The `# airuleset:build-ok`
bypass only helps a PURE module already extracted to the crate root (above); a change confined
entirely to `recording-verdict.rs`/`src/probe/` has **zero local verification path** — not even a
compile check — until CI runs. Treat every such change with extra manual review rigor (type/
signature checks, `cargo fmt --all -- --check`, diffing brace/paren balance against `origin/main`)
before pushing, and expect CI to be the FIRST place a mistake surfaces.

**Gotcha within that extra-review-rigor pass — adding an `f64` field to a probe-gated struct that
derives `Eq` breaks the build (#726).** `f64`/`f32` have no `Eq` impl (NaN has no total order), so
`#[derive(..., Eq, ...)]` on a struct that gains a field containing (or wrapping) a float no longer
compiles. Since this lives under `src/probe/` it is INVISIBLE locally (per above) — the break only
surfaces on CI. Before adding a float-carrying field to any `src/probe/*.rs` struct: `grep -n
"derive(" <file>` for that struct and drop `Eq` if present (keep `PartialEq`/`Debug` — `assert_eq!`
only needs those, never `Eq`), then `grep -rn "StructName" src/ tests/` to confirm nothing outside
the file relies on the dropped `Eq` bound (a HashSet/BTreeSet key, a generic `T: Eq` constraint) —
if something does, that's a real blocker to resolve, not just delete the derive. Example:
`probe::recording_segments::CamboxSegment`/`SegmentedContinuity` dropped `Eq` when
`presentation_cadence: Option<CadenceEvenness>` (which carries `f64` fractions) was added; verified
clean via the grep above before pushing.

**Bound the shared dev1 `target/` (backstop).** Even default-feature checks + rust-analyzer
accumulate over a day (incremental cache, never purged). Keep it under ~4 GB:
```bash
# Check size, then purge when stale / over budget (CI rebuilds it):
du -sh target 2>/dev/null
[ "$(du -sm target 2>/dev/null | cut -f1)" -gt 4096 ] && cargo clean   # >4 GB → reset
```
The repo's `scripts/purge-target.sh` (run by the `pre-push` git hook, installed by
`scripts/install-git-hooks.sh`) does this automatically before each push. **Never purge while an
E2E is live** (probe binaries executing) — the hook skips when `recording-verdict`/`frame-probe`
are running.
