---
paths:
  - "Cargo.toml"
  - ".github/workflows/ci.yml"
  - ".github/workflows/release.yml"
  - "scripts/lib/release-version.sh"
  - "scripts/camera-box-version-gate.sh"
  - "tests/release_versions_1476.rs"
---

# Real release versions: the tag + GitHub Release flow (issue 1476)

The scheme lives in the project CLAUDE.md, "Versions and releases": a clean X.Y.Z, dev = main + one
patch, a tag + Release per merge. This file holds the mechanics and the traps.

## The flow on a dev->main merge (ci.yml, main push only)

- `release-tag` needs lint/test/build/security/shellcheck, NOT deploy-fleet, so a rig that is away
  does not withhold the release.
  - It reads Cargo.toml and runs this run's `camera-box-linux-amd64 --version` (ubuntu-22.04 +
    `libasound2`: the binary links libasound only).
  - It looks the tag up with `git ls-remote` (`^{}` for an annotated tag, else the plain ref).
  - `release_tag_decision` returns CREATE (push a lightweight `vX.Y.Z` on `GITHUB_SHA`), EXISTS (a
    re-run, publish again), or FAIL. FAIL covers a pre-release on main, a binary that disagrees with
    Cargo.toml, and a tag that names another commit, which means dev was not bumped. A release is
    never moved.
- `release` calls `./.github/workflows/release.yml` with `workflow_call`. **A tag pushed with the
  default GITHUB_TOKEN starts NO other workflow** (GitHub's anti-recursion rule), so release.yml's
  `push: tags` trigger never fires for it. The calling job must grant `permissions: contents: write`:
  a called workflow cannot exceed its caller's permissions.
- On the call path, release.yml DOWNLOADS the caller run's `camera-box-linux-amd64` artifact (a
  called workflow shares the caller's run, so its artifacts are visible). The released sha256 is
  then the gated and deployed bytes. Only a hand-pushed tag builds from the tag. Both paths refuse a
  binary whose `--version` is not the tag. `make_latest` is true only on the call path, so a
  re-issued older tag never takes Latest.
- softprops v2 on an existing release updates its assets. A failed publish is finished by
  re-running the same main run (EXISTS).

## Concurrency: a main run is never cancelled, and never dropped

- A cancelled job does not trip `failure()`, so `notify-on-failure` stays silent. A main run
  cancelled after its tag push leaves a tag with no release, and nobody hears about it.
- `cancel-in-progress: false` is not enough. **GitHub keeps only ONE pending run per concurrency
  group**, so a third quick merge still cancels the waiting second run. Each main run therefore has
  its own group (`format('-{0}', github.run_id)` appended for `refs/heads/main`). The fleet deploy
  keeps its own hardware-queue group (`loopback-e2e-hardware`).

## dev > main before the merge

`version-check` (dev pushes) runs `release_version_dev_check`. It fails unless dev carries a clean
X.Y.Z strictly above main's version, read with `git fetch --depth=1 origin main` + `git show
FETCH_HEAD:Cargo.toml`, and it names the bump to make. A push made right after a release merge,
before the bump, is meant to be red: bump first.

## The order rule

`scripts/lib/release-version.sh` `release_version_cmp` is semver with pre-release. A historical
`1.7.0-dev.N` sorts below `1.7.0`, so below every `1.8.x`. Every character set is spelled out
(`local LC_ALL=C`; a bracket range widens under en_US.UTF-8, and a lint test refuses one). The hub's
verify grade has a python twin of this rule (`strih-intercom-hub-unit.sh` `key()`): change both
together. The [0/8] gate's PIN-DRIFT row names the direction (OLDER/NEWER than main). The auto-align
still decides by equality.
