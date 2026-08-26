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

- **pwsh mirror: NO for a #773-class NULL-guard, YES for a rig-critical BEHAVIORAL divergence
  (corrected #1195).** The original claim here — "the windows-genlock ymls do NOT reference
  `frontend/**`, so a frontend anchor is Rust-test-only" — is **FALSE**: both `windows-genlock.yml`
  and `windows-genlock-fast.yml` already carry frontend source-text anchors (OBSApp.cpp `#43`
  IsUpdaterDisabled in the full yml; OBSBasic.cpp `#152` titlebar + OBSProjector `#276` in BOTH).
  The real discriminator is the CHANGE CLASS, not the file: a #773-style defensive NULL-guard is
  Rust-test-only (a missing guard is caught by the full build's own compilation, and the ymls were
  never given one), but a rig-critical BEHAVIORAL divergence from upstream that STILL COMPILES
  either way and that a `git subtree pull` would silently revert (the #43/#152 class, and #1195's
  auto-normal unclean-shutdown launch — `tests/obs_unclean_shutdown_auto_normal_1195.rs`) gets a
  pwsh source-text anchor in BOTH ymls, mirroring the Rust guard — exactly as
  `vendored-libobs-change-safety.md` + `av-sync-dock-anchor-refactor-safety.md` mandate for any
  vendored guard. Decide by: "would a subtree pull silently revert a behavior we depend on, while
  still compiling?" → mirror to BOTH ymls; a pure crash/NULL safety guard → Rust anchor only.
- **A frontend change needs a FULL-BUNDLE deploy, never fast-dll** — it lands in `obs64.exe`, not
  `obs.dll` (`obs-titlebar-build-id.md` / `rig-state-inspection.md`). The supervisor/rig-ops owns
  that deploy.

## Sweeping the WHOLE frontend for the class (a shared helper, not #773's file-local guard) (#1106)

`#773` guarded ONE file with a file-local `static` helper + inline coalesce. When the SAME class
recurs across MANY files (#1106: 47 `std::string`-from-`obs_data_get_json()` construction sites
across 11 frontend `.cpp`), use ONE shared header-only helper instead of N inline coalesces or N
file-local copies:

- **`vendor/obs-studio/frontend/utility/obs-data-json-safe.hpp`** — `static inline std::string
  OBSDataGetJsonSafe(obs_data_t *data, const char *context)` coalescing NULL→`""` +
  `blog(LOG_WARNING, ...context)`. Self-contained (`#include <string>`, `<obs.h>`, `<util/base.h>`).
  **Header-only ⇒ NO CMakeLists edit** — existing TUs `#include <utility/obs-data-json-safe.hpp>`
  (the `frontend/` root is already an include dir, same as `<utility/item-widget-helpers.hpp>`);
  the compiler resolves it, no source-list entry, no new TU/moc. `static inline` in a header is
  ODR-safe across the many including TUs.
- Each of the 11 consumer files gains that one `#include` and replaces its crash-class sites with
  `OBSDataGetJsonSafe(arg, "tag")`.

**The crash class = a `std::string` BUILT from `obs_data_get_json()`, including IMPLICITLY.** Four
forms: direct-init `std::string x(obs_data_get_json(y))`, copy-init `std::string x =
obs_data_get_json(y)`, temporary `std::string(obs_data_get_json(y))`, AND — easy to miss — a bare
`obs_data_get_json(...)` passed to a `const std::string &` PARAMETER, which constructs
`std::string(NULL)` implicitly. The last one is `undo_stack::add_action(...)` (its undo/redo
payloads are `const std::string &`, `utility/undo_stack.hpp`), so `add_action(..., obs_data_get_json(d), ...)`
IS crash-class. Check every callee's parameter TYPES, not just literal `std::string(` text.

**NOT the crash class — leave byte-identical (churning them = scope creep + subtree-pull conflict):**
`obs_data_get_json()` fed to a NULL-tolerant C API (`obs_data_set_string`, `config_set_string`,
`obs_data_create_from_json`), Qt's documented-NULL-safe `QString(const char *)`, and a bare
discarded `obs_data_get_json(data);`. `std::string(obs_source_get_name(...))` is a DIFFERENT getter,
out of scope.

**Facet B for a C++ helper needs `c++`, not `cc`.** `#773`'s helper was pure C (`int`/`const char*`,
compiled with `cc`). A helper returning `std::string` must lift-compile with `c++ -std=c++17
-Wall -Wextra -Werror` against tiny stubs (`typedef struct obs_data obs_data_t;` + a
`obs_data_get_json` stub that round-trips the pointer to `const char*` so a NULL models a
`json_dumps` failure + a no-op `blog` + `enum { LOG_WARNING = ... }`) driving a NULL/non-NULL truth
table. Write the helper brace-free under the `if` (single-statement `blog`, one `return
std::string(json ? json : "")`) so the `sig → first "\n}\n"` lift extraction is unambiguous.

**The completeness test is a COUNT invariant, and count OCCURRENCES not LINES.** Per file assert
`OBSDataGetJsonSafe(` occurrences == crash-class count AND raw `obs_data_get_json(` occurrences ==
the legit NULL-tolerant remainder (0 for most; e.g. Scenes keeps 1 for its bare discard, Filters
keeps 2 for its `obs_data_set_string`). Before/after the fix these two counts SWAP, giving a clean
RED→GREEN. GOTCHA: `grep -c` counts LINES — an `add_action(..., obs_data_get_json(a),
obs_data_get_json(b))` line has TWO constructions on ONE line (Clipboard), so use occurrence-count
(`grep -o ... | wc -l`, or Rust `.matches().count()`); a line-based expected count undercounts it.
