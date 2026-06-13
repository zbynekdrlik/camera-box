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
