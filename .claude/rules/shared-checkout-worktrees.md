---
paths:
  - "docs/autopilot-log.md"
  - "Cargo.toml"
  - ".github/**"
---

<!-- Moved verbatim from CLAUDE.md (issue 1552). -->

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
- **Worktree FLEET rounds (the #317 default) — proven 2026-08-11.**
  The `EnterWorktree` TOOL refuses inside a worker here (`worktree.bgIsolation: "none"`), but
  plain git CLI worktrees work: `git -C <repo> worktree add <repo>/.claude/worktrees/<name> -b
  <branch> dev`, then the worker does ALL file edits by ABSOLUTE path under the worktree and all
  git via `git -C <worktree>` — its process cwd never changes, and it must NEVER `git checkout`/
  `switch` in the shared root. `git worktree remove` + branch-delete once the round PR merges.
  Expect trivially-resolvable append-append conflicts in `docs/autopilot-log.md` (keep
  both entries) and identical version-bump commits across workers (merge clean).
  **A worktree branch that survived from an EARLIER round and gets re-dispatched fresh for a NEW
  ticket can be MANY commits behind current `origin/dev`** (confirmed live, issue 1245, 2026-08-31:
  a worktree still sitting at `1.7.0-dev.561` while `origin/dev` was at `.594`, a 33-commit gap —
  the file the dispatched issue named, `scripts/lib/frame-probe-parity-align.sh`, genuinely did NOT
  EXIST on that stale branch, even though the issue was fully valid and the file existed on current
  `dev`). Before concluding a dispatched ticket references a nonexistent file/pattern (an
  "obsolete ticket" verdict per `verify-issue-still-valid.md`), first confirm `git log HEAD --not
  origin/dev --oneline` is EMPTY (no unique commits of your own yet) and, if so, `git merge --ff-only
  origin/dev` — a missing file on a stale worktree branch is NOT evidence the ticket is invalid.
  **A lane's worktree can also be based on `origin/main` instead of `dev`** (confirmed 2026-09-02:
  the issue-1130 lane returned 3 commits on top of main's `Merge pull request #1257` commit while
  three sibling lanes dispatched the same way sat on `origin/dev`). Before EVERY integration merge
  run `git merge-base <lane-branch> dev` / `git log --oneline dev..<lane-branch>`: if main-side merge
  commits show up in that range, `git cherry-pick` the lane's own commits onto `dev` instead of
  `git merge` — a merge would drag main's merge commits into dev's history. **Lane side (issues
  1377 + 1379, 27.9.2026):** such a lane's startup `git merge --ff-only origin/dev` FAILS (main's
  merge commits are never dev ancestors). With NO own commits yet, re-point the lane's OWN branch
  with `git switch -C <own worktree branch> origin/dev` (clean tree, no history rewrite) — or make
  `worktree-issue-<N>` from origin/dev and report THAT name — then `git branch --unset-upstream`
  so no bare `git push` can ever target `dev`.
- **A lane deletes every scratch dir it created (downloaded bundles, stage copies, experiment
  trees) before its LANE-RETURN; the supervisor deletes a merged lane's leftovers at integration;
  and ANY session (supervisor included) deletes every scratch dir it created, downloaded CI
  bundles first, once the deploy or measurement that needed it is verified, keeping at most the
  one in use** (issue 1395:
  9 copies of a 2.2 GB strih bundle piled up to 25 GB in one session scratchpad on dev1's one
  shared disk; the E2E run dirs are bounded by `scripts/lib/e2e-rundir-retention.sh`).
- **A worker's OWN local `origin/dev` tracking ref can drift AHEAD mid-session too** (confirmed
  live, issue 1278, 2026-09-02: a lane's local `origin/dev` moved from its own fork point
  `85ee75632` to `ddf43a3a8` — a SIBLING lane's merge — sometime between the lane's initial fetch
  and a mid-session `git diff`, with no `git fetch` run by the lane itself in between). A naive
  `git diff origin/dev..HEAD` at that point is NOT "my branch's diff" — it also reverses whatever
  the sibling lane added/removed since the fork point (deleted test files, changed `Cargo.toml`,
  etc. all show up as noise), which is actively dangerous when handing that diff to a fresh-context
  review dispatch (it would review changes that aren't yours). **Always diff against the actual
  fork point, not a bare `origin/dev`:** `git merge-base HEAD origin/dev` then
  `git diff <that-sha>..HEAD` — this is the SAME fix the integration-merge bullet above already
  recommends for a different symptom (main-side merge commits polluting a cherry-pick range); here
  it applies to a plain review/audit diff a worker prepares for itself mid-lane.
- **A red "CI" run on the OTHER worker's push can be YOUR OWN not-yet-fixed RED commit riding
  along, not a real regression in their code.** Confirmed live (2026-07-30, #854/#881 vs #878):
  worker A committed a TDD RED commit (failing-on-purpose, per `regression-test-first.md`) while
  worker B's own commit landed on top of it in the shared local history; when B pushed next, the
  push carried A's still-broken RED commit too (a push always carries full ancestry — see the
  bullet above), and B's CI run failed on TESTS A HAD NOT YET FIXED. Before treating a scary CI
  failure on a push you didn't make as evidence the other worker's code is broken, check the
  failing test names against YOUR OWN recent RED commit (`git log --oneline` around the failing
  SHA) — if they're yours, it's a superseded interleaving artifact, not a real bug; your own next
  push (carrying the GREEN fix) supersedes it and should be judged on its OWN CI run instead.
