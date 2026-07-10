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
- E2E zero-loss testing (acceptance criteria, QR harness, reporting scope) → load `.claude/skills/e2e`
- Rig TEST/EVENT mode switch (#247 `scripts/rig-mode.sh`: pinned QR/burns/genlock per mode, the #246 burn-leak guard) → load `.claude/skills/e2e`
- Recording-verdict QR decode path (fast/robust gate, per-recording burn sets, #186 fixtures) → load `.claude/skills/recording-decode`
- A/V-sync offset measurement (cam2 QPSK marker, `--av-sync`, ring-bias + cluster-pairing gotchas) → load `.claude/skills/av-sync`

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

## GOTCHA — editing `scripts/recording-e2e.sh` can silently break OTHER test files' static anchors

Many separate `tests/harness_*.rs` files independently `.find()` literal substrings/adjacency in
`scripts/recording-e2e.sh` (a banner like `"[5/8] StartRecord"`, or structural adjacency like
`fi\ntrap cleanup EXIT`) to pin ordering/structure — the same static-string-assertion model
`tests/harness_recording_e2e_cleanup_resilient.rs` (#328) established, now reused across many
unrelated features (`#137` av-restart-sync, `#286` all-cambox burn targets, `#649` StopRecord
ordering, etc.). A new comment or code line you add to this file CAN accidentally (1) duplicate a
literal anchor another test's `.find()` relies on — `.find()` returns the FIRST match in the WHOLE
file, not the occurrence near your own edit — or (2) break a textual adjacency two unrelated tests
hard-code (e.g. inserting a line between an `if` block's closing `fi` and the following `trap
cleanup EXIT HUP INT TERM` line).

**Confirmed live (#649, 2026-07-10):** a new `cleanup()` comment containing the literal text
`[5/8] StartRecord` broke 3 tests in `tests/harness_av_restart_sync_gate.rs` (the `#137` gate,
which anchors on that exact string to slice "everything before the main record step"); and adding
new variable declarations directly before `trap cleanup EXIT HUP INT TERM` broke
`recording_e2e_all_cambox_extends_burn_targets_to_every_strih_input_286` in
`tests/harness_recording_e2e_paths.rs` (which hard-codes that the `#286` ALL_CAMBOX block's
closing `fi` is immediately followed by `trap cleanup EXIT`).

**Mitigation:** after ANY edit to `scripts/recording-e2e.sh`, run the FULL `cargo test` suite —
not just your own new/targeted test file — before pushing (`cargo test # airuleset:build-ok`
locally bypasses the Tier-0 build-block for this one-off check; Tier-0 policy is otherwise
`cargo test --no-run` only, see below). A failure elsewhere in the suite right after touching this
file is very likely a textual collision, not a real regression — grep the failing test's
`.find(...)` argument (or the surrounding slice logic) to see which literal string or adjacency
moved, then reword your new text (or relocate it) so it no longer matches/breaks that anchor.

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
