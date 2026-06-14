# vendor/ — the genlock monorepo (one repo, everything needed)

Per the project decision (#41): the WHOLE genlocked AV stack lives in THIS repository —
fresh copies of the latest upstream releases, with our genlock patches applied on top —
so the final working state is reproducible from one place and production boxes never run
an unpinned/stock build.

| dir | upstream | version | imported as |
|---|---|---|---|
| `vendor/obs-studio` | github.com/obsproject/obs-studio | **32.1.2** (commit `fb4d98bf8`) | git subtree --squash |
| `vendor/distroav` | github.com/DistroAV/DistroAV | **6.2.1** (commit `038d9d6`) | git subtree --squash |
| NDI SDK headers | shipped inside DistroAV (`vendor/distroav/lib/ndi/`) | SDK v6 (plugin requires **NDI ≥ 6.3.0**) | part of the DistroAV tree |

The NDI **runtime** (`libndi.so` / `Processing.NDI.Lib.x64.dll`) is NOT committed —
licensing forbids redistribution (see the License Agreement PDF in `lib/ndi/`). Each
machine gets it via the NDI installer / `vendor/distroav/CI/libndi-get.sh`.
**Note:** dev1 + cam2 currently run NDI runtime 6.2.1 — DistroAV 6.2.1 needs ≥ 6.3.0, so
the runtime upgrade is part of the rollout. The production OBS boxes strih + stream already
run NDI runtime **6.3.2.0** (≥ 6.3.0 ✓, verified 2026-06-14).

## Pinned production settings — drift guard (#45)

`scripts/drift-guard.sh` (unit-tested in `tests/drift_guard.rs`) enforces that strih
(`10.77.9.202`) + stream (`10.77.9.204`) stay on the versions above **and** these critical
runtime settings — the known-good zero-loss state verified live on both boxes 2026-06-14. A
*deliberate* rollout (the 30→60 fps step #11, or activating genlock) re-pins the value here as
part of that change; an *unexpected* difference is drift and the guard fails loudly.

| setting | pinned value | live source (read-only) |
|---|---|---|
| `output_fps` | `30` | OBS log `video settings reset: … fps: <n>/1` (current zero-loss rate; re-pin to `60` on the #11 rollout) |
| `genlock_wall_clock` | `0` | env `OBS_GENLOCK_WALL_CLOCK` master gate — `0` = genlock dormant; set `1` (+ per-input FIFO) when genlock goes live |

The OBS/DistroAV **versions** come from the version table above (single source of truth); the NDI
runtime is checked `≥` the `NDI ≥ 6.3.0` minimum stated there. The two facets:

```bash
./scripts/drift-guard.sh --check-pins    # CI: validate the pin set + cross-check vs vendored source
./scripts/drift-guard.sh --compare host=strih obs_version=… distroav_version=… \
    ndi_runtime=… output_fps=… genlock_wall_clock=…   # live box (values read via win-* MCP)
```

The live read-only run is driven by `/drift-guard` (`.claude/commands/drift-guard.md`), which gathers
the observed values off strih/stream through the win-* MCP tools and feeds them to `--compare` —
CI runners can't reach the production LAN, so the live facet is operator/agent-driven, not in CI.

The OBS **auto-update dialog stays disabled** (#43) is a *build-time* property, not runtime-readable
off a running box, so it is guarded at its proper layer — `tests/obs_updater_disabled.rs` against the
vendored source — rather than by this runtime guard.

## Why subtree --squash

- One repo (the user's requirement) — no submodule indirection, the source is HERE.
- No upstream history bloat — each import is a single squash commit.
- Updates stay tractable: `git subtree pull --squash` MERGES a new upstream release with
  our local genlock patches instead of overwriting them. This is the mechanism the
  release-bump slash command (#44) builds on.

## Updating to a new upstream release — `/update-av-stack` (#44)

Use the slash command `/update-av-stack` (engine `scripts/update-av-stack.sh`, unit-tested in
`tests/av_stack_update.rs`). It parses the version table above, checks each subtree component
against the latest upstream **stable** tag, and — for anything behind — runs the catch-up pull,
re-applying our genlock patches through the subtree merge and reporting conflicts loudly:

```bash
./scripts/update-av-stack.sh --check    # read-only: report drift + the exact catch-up commands
./scripts/update-av-stack.sh --apply    # run the git subtree pulls (clean tree required)
```

Each pull is equivalent to:

```bash
git subtree pull --prefix=vendor/obs-studio https://github.com/obsproject/obs-studio.git <NEW_TAG> --squash
git subtree pull --prefix=vendor/distroav  https://github.com/DistroAV/DistroAV.git  <NEW_TAG> --squash
```

After applying: resolve conflicts patch-by-patch (each `genlock:` commit is one patch), rebuild
per `BUILD.md`, run the strict harness (#35), and update the table above with the new tag/commit.

## Our patches

Genlock changes (#42) are normal commits in THIS repo touching `vendor/` files — `git log
-- vendor/` after the two import commits IS the patch series. Keep each patch commit
focused and prefixed `genlock:` so the #44 update flow can review conflicts patch-by-patch.

## Build

Local prototyping happens on dev1 (Linux). The production target is a Windows build for
strih/stream against this exact tree. Build docs land with the first proven build (#41
acceptance); the OBS auto-update dialog is disabled in our build per #43 so a stock OBS
can never overwrite a genlocked install.
