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
