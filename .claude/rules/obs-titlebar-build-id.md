---
paths:
  - "vendor/obs-studio/frontend/widgets/OBSBasic.cpp"
  - "vendor/obs-studio/frontend/widgets/NewlevelBuildSha.hpp"
  - "tests/obs_titlebar_newlevel.rs"
  - "tests/obs_titlebar_newlevel_sha_parse.rs"
---

# OBS window-title build identity (#152 / #313 / #1018)

The production OBS title (`OBSBasic::UpdateTitleBar()`) stamps
` - newlevel.media build <short-sha>` so an operator can tell at a glance WHICH build a
box runs (version-integrity epic #125). This is a genlock patch on the vendored OBS
FRONTEND (obs64.exe), guarded by a lock-step trio — keep all three in sync.

## The `/Brepro` trap — compile-time `__DATE__`/`__TIME__` are BLANK in this build (#1018)

OBS compiles with `/Brepro` (reproducible builds, `vendor/obs-studio/cmake/windows/
compilerconfig.cmake`). Under it MSVC blanks the compiler `__DATE__`/`__TIME__` macros to a
short placeholder. So ANY build-identity scheme based on `__DATE__`/`__TIME__` renders
useless: the original #152/#313 titlebar reformatted `__DATE__` and always showed
`newlevel.media build unknown` on the real boxes (the `newlevel_iso_date()` size-guard
fallback firing on a `< 11`-char input). **Never reach for `__DATE__`/`__TIME__` for a
build stamp in the vendored OBS frontend** — they cannot carry a real value here.

The source of truth for "what build is deployed" is **`GENLOCK_BUILD_SHA.txt`** at the OBS
install root — every deploy (full obs64.exe swap AND fast obs.dll hot-swap) writes it. The
title reads it and shows the short SHA (`newlevel_short_sha()` in `NewlevelBuildSha.hpp`, a
pure OBS/Qt-free formatter, unit-tested off-rig by `tests/obs_titlebar_newlevel_sha_parse.rs`).

## Resolve install-root files from the EXE dir, never the process cwd (#1018)

The Start-Menu shortcut launches with WorkingDir `bin\64bit`, and a relaunch may differ, so
a cwd-relative read is unreliable. Resolve relative to obs64.exe's OWN directory via libobs
`os_get_executable_path_ptr("../../GENLOCK_BUILD_SHA.txt")` (install root) then
`"GENLOCK_BUILD_SHA.txt"` (bin\64bit — both are deploy targets). It uses
`GetModuleFileNameW`, i.e. the exe path, never cwd. Free the returned `char*` with `bfree`.
(`os_get_executable_path_ptr`+`bfree`+`BPtr` come from `OBSBasic.hpp`'s `<util/platform.h>`
+ `<util/util.hpp>`; add `<fstream>`/`<iterator>` for the ifstream read.) A title helper
runs during OBSBasic construction — it must NEVER throw (#313); every path returns "unknown"
on failure.

## Lock-step guards — three copies of the anchor tokens, keep in sync

A `git subtree pull` upstream bump (#44 `/update-av-stack`) can silently restore the stock
title. Three guards defend it and must ALL be updated together when the mechanism changes:

1. `vendor/obs-studio/frontend/widgets/OBSBasic.cpp` — the call site + helper.
2. `tests/obs_titlebar_newlevel.rs` — the canonical Linux-CI source-anchor guard (this crate
   is Linux-only, cannot compile on the windows runner).
3. BOTH `.github/workflows/windows-genlock.yml` AND `windows-genlock-fast.yml` — pwsh
   source-anchor gates (`-replace '\s+',' '` then `[regex]::Escape` substring checks). The
   FULL workflow builds the frontend; the FAST one does not but still text-gates the tokens.

Both squish algorithms (Rust `split_whitespace().join(" ")` and pwsh `-replace '\s+',' '`)
must agree on the pinned substrings, or you self-inflict a CI failure.

## FRONTEND change ⇒ obs64.exe swap ⇒ needs the FULL bundle to deploy

The FAST `obs-genlock-fast-dll` artifact ships only obs.dll + a fresh `GENLOCK_BUILD_SHA.txt`
+ manifest — NOT obs64.exe. So any frontend (obs64.exe) change is invisible to a fast deploy;
the full `windows-genlock.yml` bundle (which builds + uploads obs64.exe) is required. This is
also why the deployed obs.dll/SHA can be newer than the running obs64.exe after a fast swap —
another reason the title must read the SHA file, not the exe's own compile identity.

## No local verification for frontend/vendored code — CI is first compile

`src/probe/*` AND `vendor/obs-studio/frontend/*` do not compile under a local Tier-0 cargo
check (camera-box disables the run-tests bypass entirely, airuleset #477). Get RED→GREEN on
the PURE header off-rig by compiling the standalone harness with plain `c++` directly (what
`tests/obs_titlebar_newlevel_sha_parse.rs` does at runtime) — that is not a cargo build and
is not hook-blocked. `cargo fmt --all --check` still parses the `.rs` harness; the vendored
C++ glue itself is first type-checked on the 150-min `windows-genlock.yml`.
