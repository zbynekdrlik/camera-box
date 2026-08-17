---
paths:
  - "vendor/obs-studio/frontend/**"
  - "tests/prop_dialog_*.rs"
---

# Vendored OBS FRONTEND crash-safety (`vendor/obs-studio/frontend/**`) — the `obs_data_get_json()` NULL class (#773)

The `#793`/`#1026` WS-enum crash class in `vendored-libobs-change-safety.md` is about **libobs**
borrowed pointers. This is its **frontend** sibling: a Qt dialog consuming a libobs getter result
without a NULL guard.

## The crash class: `obs_data_get_json()` / `obs_source_get_settings()` can return NULL

- `libobs/obs-data.c::obs_data_get_json()` returns NULL not only for a NULL `obs_data_t` but also
  when `json_dumps(root, flags)` (jansson) fails — an intermittent serialization/alloc failure on a
  perfectly valid `obs_data_t`.
- `libobs/obs-source.c::obs_source_get_settings()` returns NULL for an invalid source (→
  `obs_data_get_json(NULL)` = NULL too).

So ANY frontend code that feeds an `obs_data_get_json(...)` result straight into `strcmp()` (crash
c0000005 in `ucrtbase!strcmp`) or into `std::string(...)` (UB — `std::string(NULL)` crashes) is a
latent NULL-deref. `#773` was `OBSBasicProperties::CheckSettings()` (unguarded `strcmp`) + the
sibling `on_buttonBox_clicked` AcceptRole path (`std::string(obs_data_get_json(...))`). Fix =
**guard at the CONSUMER**: a NULL-safe helper that treats an unreadable current/old JSON as "no
detectable change" (return 0), so the dialog closes cleanly instead of dereferencing NULL (and
without popping a Save/Discard prompt on settings that cannot even be serialised). When touching any
frontend file, grep it for `strcmp(` and `std::string(obs_data_get_json(` before trusting it.

## Reading a live OBS crash log on strih/stream — win-* MCP, NOT ssh

The Windows crash reports live at `C:\Users\<user>\AppData\Roaming\obs-studio\crashes\Crash
<date>.txt` and PERSIST for weeks (the `#773` log was still there a month later). Read them
read-only through the box's **win-* MCP** (`FileSearch` to locate, `FileRead`/`Shell Get-Content`
to read) — a session-agnostic file read, so it is allowed even in an agent session (the
`win-ssh-vs-mcp.md` rule bans only ssh GUI atoms, not an MCP file read). What to read:

- `Fault address: <addr> (…\ucrtbase.dll)` — `ucrtbase.dll` is the UCRT C runtime where `strcmp`
  and friends live; a fault there = a bad pointer into a CRT string op.
- The crashed thread block (`Thread <id>: (Crashed)`): the top frame's `Arg0` column is the first
  argument. **`Arg0 = 0000000000000000` = a NULL first argument** — the smoking gun for
  `strcmp(NULL, …)` / a NULL-deref, and it tells you WHICH pointer was NULL.
- The frame chain gives the exact call path (`… → CheckSettings → strcmp`).

## Testing a frontend fix — same std-only Facet A + Facet B, but NO pwsh mirror

The vendored frontend `.cpp` compiles ONLY on CI (`# airuleset:build-ok` disabled). Reuse the
`vendored-libobs-change-safety.md` #793/#1026 pattern exactly (see `tests/video_io_null_guard.rs` /
`tests/prop_dialog_checksettings_null_guard_773.rs`):

- **Facet A** — std-only `fs::read_to_string` source anchors (helper present, guard-before-deref,
  old unguarded form absent), runnable offline via `CARGO_MANIFEST_DIR=<worktree> rustc --test
  --edition 2021 tests/<file>.rs -o /tmp/x && /tmp/x`.
- **Facet B** — lift the pure helper VERBATIM by signature → first `\n}\n` and `cc -Werror
  -Wconversion -Wformat=2` compile it over a truth table. A frontend `static int helper(const char
  *, const char *)` is valid C, so `cc` compiles it fine even though it lives in a `.cpp`; call it
  from `main` so `-Werror`'s unused-static-function does not fire. This proves the shipped bytes
  COMPILE + COMPUTE before CI.

Two frontend-specific differences from the libobs rule:

- **No `windows-genlock*.yml` pwsh mirror.** Those workflows anchor libobs genlock tokens; they do
  NOT reference `frontend/**` (grep confirms), so a frontend anchor is Rust-test-only.
- **A frontend change needs a FULL-BUNDLE deploy, never fast-dll** — it lands in `obs64.exe`, not
  `obs.dll` (`obs-titlebar-build-id.md` / `rig-state-inspection.md`). The supervisor/rig-ops owns
  that deploy.
