---
description: Re-apply our genlock patches onto the latest upstream OBS/DistroAV releases in vendor/ (the genlock monorepo). Reports conflicts loudly.
argument-hint: "[--check | --apply]"
allowed-tools: Bash, Read, Edit
---

# /update-av-stack — bump the vendored AV stack to the latest upstream release

The `vendor/` tree is the genlock monorepo (#41): fresh upstream **OBS** + **DistroAV** releases
with our `genlock:`-prefixed patches applied on top, imported via `git subtree --squash`
(see `vendor/README.md`). This command catches the stack up to the latest upstream **stable**
release so production boxes never linger on an old build, re-applying our patches through the
subtree merge and **reporting every conflict loudly** — never auto-resolving or skipping.

The deterministic engine is `scripts/update-av-stack.sh` (unit-tested in
`tests/av_stack_update.rs`). This command drives it through the full human-in-the-loop bump.

## Steps

1. **Pre-flight.** Confirm you are on `dev` with a clean tree (`git status --porcelain` empty).
   A subtree pull needs a clean tree; if dirty, stop and surface why — do not stash silently.

2. **Check drift (read-only).** Run:
   ```bash
   ./scripts/update-av-stack.sh --check
   ```
   - Exit `0` → every component is up to date with upstream stable. **Report that and STOP** —
     nothing to do.
   - Exit `10` → at least one component is BEHIND. The output lists each behind component and
     the exact `git subtree pull … --squash` catch-up command. Continue.

3. **Apply, one component at a time.** For each BEHIND component, run its catch-up command
   (or `./scripts/update-av-stack.sh --apply` to run them in sequence). The subtree pull does a
   3-way merge of the new upstream release against our local `genlock:` patches — that merge
   IS the patch re-application.

4. **On conflict — STOP and report loudly. Never auto-skip.**
   - A conflict means upstream changed the same lines one of our genlock patches touched.
   - List the conflicting files (`git diff --name-only --diff-filter=U`).
   - Map each to the owning patch: `git log --oneline -- vendor/` — every `genlock:` commit is
     one patch in the series.
   - Resolve patch-by-patch preserving our genlock intent (wall-clock-slaved render tick + FIFO
     source consumption — #42), `git add`, then commit the merge. If a resolution is genuinely
     ambiguous, surface it to the user rather than guessing.

5. **Rebuild** the vendored stack per `vendor/BUILD.md` (OBS first, then DistroAV against it).
   Build dirs stay in `/tmp`, never inside the repo. A build failure after a bump is a
   conflict you missed — go back to step 4.

6. **Run the strict on-device harness** `scripts/loopback-e2e.sh` — **zero** frame loss is
   required (#35). Anything less than a clean PASS blocks the bump.

7. **Update the manifest + commit.** Edit the version table in `vendor/README.md` to the new
   tag + squash commit for each bumped component. Commit the manifest update together with the
   merge using a `vendor:` / `genlock:` prefix, referencing the upstream tag.

8. **Report** what bumped (old → new tag per component), every conflict and how it was resolved,
   the build result, and the harness verdict. If anything is unverified (e.g. the harness needs
   a device that is off-air-only), say so explicitly — never claim a clean bump you did not prove.

## Notes

- **NDI** has no standalone subtree — its SDK headers ride inside `vendor/distroav/lib/ndi/`,
  so bumping DistroAV bumps the NDI headers. The runtime (`libndi.so` / the Windows DLL) is
  licensed per-machine and never committed (`vendor/README.md`).
- This command only touches `vendor/`. It never deploys. Deploying a bumped stack to
  strih/stream is a separate, off-air, user-approved step (`deploy-from-clean-tree`).
- `--check` is safe to run any time (read-only, network only). `--apply` mutates the repo and
  requires a clean tree.

## Gotchas learned live (#825, the 32.1.2 → 32.2.0 OBS bump) — READ before the next bump

The `--check`/`--apply` engine only knows about the two TOP-LEVEL subtree rows (obs-studio,
distroav). A real OBS bump hits five things the engine does NOT handle — a "clean" subtree pull
is not a complete bump:

1. **The 3 nested submodule-subtrees CONFLICT every time, and the conflict RECURS forever.**
   `plugins/obs-websocket`, `plugins/obs-browser`, `deps/libdshowcapture/src` are git SUBMODULES
   upstream (gitlink, mode 160000) but REAL CONTENT trees in our monorepo. The subtree merge
   always presents a dir-vs-gitlink conflict there (upstream bumps the gitlink SHA each OBS
   release; the `--squash` base never absorbs our resolution). **Resolve by KEEPING our content:**
   `git rm -rf 'vendor/obs-studio/plugins/obs-websocket~<squashsha>'` (and obs-browser), then
   `git ls-files -s vendor/obs-studio | awk '$1=="160000"'` MUST be empty. Do NOT adopt the
   incoming gitlink. Holding these (+ DistroAV) is CORRECT when the libobs public API is additive
   (see 5) — they source-compile against the new libobs and declare the right `LIBOBS_API` on
   rebuild; it also freezes the WS protocol our Rust `obs-ops` client depends on.

2. **The version string is NOT in the vendored tree — the merge does NOT change `obs --version`.**
   `versionconfig.cmake` falls to `git describe` / `OBS_VERSION_OVERRIDE`, and our `.git`-less
   subtree can't git-describe OBS (`_obs_default_version` is upstream's placeholder `0.0.1`). The
   REAL lever is **`-DOBS_VERSION_OVERRIDE=<X.Y.Z>` in the 5 genlock-workflow steps**
   (`linux-genlock.yml` ×2, `windows-genlock.yml` ×1, `windows-genlock-fast.yml` ×2). Forget these
   and an otherwise-perfect merged tree still ships the OLD version.

3. **The `32.1.2` PIN LATTICE is broad — bump it all, prove it with the tests.** Beyond `vendor/README.md`
   (the source-of-truth the scripts READ) + `BUILD.md`: the 5 workflow overrides;
   `tests/{drift_guard,version_integrity_gate,genlock_manifest}.rs` live-pin assertions;
   `scripts/setup-imag.sh` + `verify-imag.sh` `IMAG_OBS_BASE_VERSION` default (the #824
   version-matched-base — must equal the PPA base for the target series) AND setup-imag's
   `LastVersion` first-run-wizard sentinel (`(major<<24)|(minor<<16)|patch`). Method that CANNOT
   over/under-reach: bump README + workflows + the imag defaults FIRST, then run the affected
   std-only tests via the #1026 rustc recipe (`CARGO_MANIFEST_DIR=<wt> rustc --test --edition 2021
   tests/<f>.rs -o /tmp/x && CARGO_MANIFEST_DIR=<wt> /tmp/x`) and fix ONLY the assertions that go
   RED. Self-consistent FIXTURES (a fake README, an OBS-log parse fixture, a drift/logic-input
   example, the `av_stack_update.rs` version-compare) stay at the old version — they are green and
   deliberate. Completeness gate: `grep -rn 32.1.2` outside `vendor/` + worktrees returns only
   comments/parse-doc/historical, never a functional pin.

4. **Which upstream tag: match the PPA base, not the highest tag.** Acceptance is usually "obs
   --version matches the PPA base on every box". Confirm the box series' Published version via the
   Launchpad API (`getPublishedBinaries ... status=Published`, filter the series e.g. `~noble`) —
   it can lag the newest upstream source tag (e.g. PPA noble = 32.2.0 while upstream also has
   32.2.1/32.2.2). Target the PPA base.

5. **Additive-API check decides bump-vs-hold for the nested plugins + DistroAV.** `git diff
   <oldtag> <newtag> -- 'libobs/*.h' 'libobs/**/*.h' | grep '^-'` — if there are ZERO real
   signature/`EXPORT` removals (only `LIBOBS_API_*_VER` macro bumps + line re-wraps) and the major
   is unchanged, the held plugins + DistroAV compile + load fine; only a REBUILD is needed, no
   source bump. A minor OBS bump is usually additive.

6. **Frontend conflict resolution when upstream heavily restructured a file:** our patch there is
   small (an `#include` + a few `obs_data_get_json`→`OBSDataGetJsonSafe` swaps / the #773
   NULL-guards). Take upstream's WHOLE new version (`git show <newtag>:<path>`) and surgically
   re-apply our hunks, then PROVE it: `diff <(git show <newtag>:<path>) <resolved>` must show
   EXACTLY our patch hunks and nothing else. The `frontend_obs_data_json_null_guard_1106.rs` anchor
   test is the completeness proof the guards survived.

7. **The genlock C auto-merges clean on a minor bump** (its regions don't overlap upstream's
   additive delta) — but PROVE no patch was lost: run every std-only genlock/distroav anchor test
   via the rustc recipe (genlock_release_cadence, gl_pbo_orphan, distroav_*, aux_sender_teardown,
   obs_updater_disabled, linux_genlock_workflow_gate, windows_manifest_gate) + grep the markers in
   obs-source.c / gl-texture2d.c / distroav/src. `cargo test --no-run` builds a ~4.5 GB `target/`
   — skip it when the shared disk is tight + sibling worktree workers are active (the rustc recipe
   compiles each test standalone to /tmp instead).
