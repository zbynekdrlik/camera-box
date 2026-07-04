# Claude Code Guidelines for camera-box

Rust app for embedded NDI cameras (CAM1-4): multi-camera NDI streaming with software genlock + intercom/sidetone audio. Built locally, deployed to the camera devices over SSH.

<!-- Global rules inherited from ~/.claude/CLAUDE.md (managed by airuleset) -->
<!-- PR merge policy, CI monitoring, TDD, two-branch git workflow, test strictness, security, comprehensive logging apply automatically. This file holds ONLY camera-box-specific context — do not duplicate global rules here. -->

## Playbook router

- Rig ops (DanteSync clock, device deploy, recovery) → load `.claude/skills/ops`
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

## GOTCHA — two autopilot workers sharing this dev1 checkout WILL interleave on `dev`

This repo's autopilot workers run directly in `~/devel/camera-box` on dev1 with **no git
worktree isolation** — every worker's `git commit`/`git push` operates on the SAME local
checkout and the SAME local `dev` branch ref. If the supervisor ever dispatches two workers into
this repo at once (violates `two-branch-workflow.md`'s "dispatch serially — one active worker per
repo", but has happened), their commits land on the SAME linear `dev` history, interleaved by
whichever process commits first — there is no isolation and no conflict warning.

**Incident (2026-07-04):** worker A (#499+#500, `setup-imag.sh`) and worker B (#505, a GL PBO-orphan
fix) both committed to `dev` concurrently. Worker A protected its own pushes by pushing an exact
commit SHA (`git push origin <own-sha>:refs/heads/dev`, never a bare `git push origin dev`) so
worker B's not-yet-pushed commits weren't dragged to `origin` prematurely — but a `git commit` run
by A ON TOP of B's already-advanced local HEAD unavoidably included B's ancestry on the next push
(a git push always carries a commit's full ancestor chain; there is no way to exclude mid-branch
commits without a force-push, which is banned). Net result: worker A's PR ended up also shipping
worker B's fully-complete #505 work, auto-closing it via B's own `fix: #505 ...` commit title.
Harmless here (B's work was genuinely finished + TDD'd), but in a worse timing it could ship a
STILL-IN-PROGRESS body of foreign work through the wrong PR with no review of it.

**Mitigation for a worker that detects this mid-flight** (`git log --oneline -5` shows commits you
didn't write, or `git status` shows files you never touched): (1) NEVER `git push origin dev`
bare — always push your own exact last commit SHA (`git push origin <sha>:refs/heads/dev`) so you
never ship more than you intend; (2) before every `git commit`, `git log --oneline -3` to confirm
HEAD is still what you expect; (3) if a stray untracked/modified file you didn't create shows up in
`git status`, NEVER `git add -A` — stage only the exact paths you touched; if one still gets swept
in by a shared-index race, `git rm --cached <path>` in a follow-up commit (never delete the file
from disk — it's someone else's live work); (4) NEVER `git reset`/force-push to "undo" another
session's commits from the shared local branch — you'd be mutating a ref the other process may
still be relying on mid-operation; (5) note the collision plainly in your evidence block/autopilot
log and, if a foreign commit auto-closed an issue that wasn't yours, explain it via
`gh issue comment <N>` for traceability. The supervisor should prefer serial dispatch or
per-worker `git worktree` isolation for this repo going forward.

## GOTCHA — two autopilot workers in the SAME checkout share one git index/branch ref

`/home/newlevel/devel/camera-box` is a single shared clone. If two autopilot workers are ever
dispatched concurrently (violates the intended "one active worker per repo" but has happened in
practice — 2026-07-04, issues #499/#500 vs #505), they share the SAME working tree, index, and
local `dev` ref, not just the same remote branch. Consequences + mitigations, confirmed live:

- **`git add` + a later separate `git commit` leaves a race window** — the other worker's own
  `git commit -a`-style commit can sweep up your staged-but-uncommitted file. **Stage and commit
  in the same breath, and always pass an explicit pathspec:** `git commit -m "..." -- <path>`
  (pathspec form defaults to `--only` — commits ONLY that path's working-tree content,
  disregarding anything else staged, and leaves the other worker's staged changes untouched). If
  a sweep still happens, `git rm --cached <path>` in a new commit restores the file to untracked
  (don't `git rm` the file itself — the other worker's session may still need it on disk).
- **A push by EITHER worker pushes the local `dev` ref as it stands** — including the OTHER
  worker's already-made local commits, since both share one ref. There is no way to "un-push"
  someone else's commits without a banned force-push/history-rewrite. If this happens, verify
  with `gh issue view <N>` whether it changes which issue(s) a shared PR will auto-close (see the
  GOTCHA above) and adapt the PR body / commit wording rather than fighting the git state.
- **GitHub allows only ONE open PR per (head, base) branch pair.** If another worker's PR from
  `dev`→`main` is already open when you're ready to push, you CANNOT open a second one — wait for
  theirs to reach a terminal state (poll `gh pr view <N> --json state,mergedAt`, e.g. via
  `Monitor`) before pushing, or your commits will just get folded into their PR's diff.
- **A later push cancels an in-flight `linux-genlock.yml` run via its
  `linux-genlock-${{ github.ref }}` concurrency group — even if the later push doesn't itself
  touch `vendor/**`** (the group is keyed on the ref, not the specific paths). A cancelled run is
  NOT a build proof. Re-trigger manually once the ref is stable:
  `gh workflow run "Linux genlock build (vendored OBS + DistroAV, imag-nb parity)" --ref dev`.
- **`linux-genlock.yml` only triggers on push to `dev`, never `main`** (its `on.push.branches` is
  `[dev]` only). After a dev→main merge, main never automatically gets a genlock build — if you
  need proof the exact merged main state compiles, `gh workflow run "Linux genlock build
  (vendored OBS + DistroAV, imag-nb parity)" --ref main` explicitly.

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
