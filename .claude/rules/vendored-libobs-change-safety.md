---
paths:
  - "vendor/obs-studio/libobs/obs-source.c"
  - "vendor/obs-studio/libobs/obs-internal.h"
  - "vendor/obs-studio/libobs/obs-display.c"
  - "vendor/obs-studio/libobs/graphics/graphics.c"
  - "vendor/obs-studio/libobs/graphics/graphics-internal.h"
  - "vendor/obs-studio/libobs/graphics/graphics-imports.c"
  - "vendor/obs-studio/libobs-opengl/gl-x11-egl.c"
  - "vendor/obs-studio/libobs-opengl/gl-wayland-egl.c"
  - "vendor/obs-studio/libobs-opengl/gl-subsystem.h"
  - "vendor/obs-studio/libobs-opengl/gl-subsystem.c"
  - "vendor/obs-studio/frontend/widgets/OBSProjector.cpp"
  - "tests/gl_egl_present_vsync_1107.rs"
  - "tests/genlock_release_cadence.rs"
  - "tests/genlock_relock_selection_parity.rs"
  - "vendor/obs-studio/libobs/obs.c"
  - "vendor/obs-studio/libobs/obs.h"
  - "vendor/obs-studio/libobs/obs-canvas.c"
  - "vendor/obs-studio/libobs/obs-output.c"
  - "vendor/obs-studio/libobs/media-io/video-io.c"
  - "vendor/obs-studio/libobs/obs-display-budget.h"
  - "vendor/distroav/src/ndi-filter.cpp"
  - "tests/aux_sender_budget_879.rs"
  - "tests/aux_sender_teardown_ordering_877.rs"
  - ".github/workflows/windows-genlock.yml"
  - ".github/workflows/windows-genlock-fast.yml"
---

# Changing the genlock C in `libobs` — CI is its first compile, so buy verification back (#1003)

The project CLAUDE.md is blunt about the constraint: a change confined to the vendored tree has
"zero local verification path — not even a compile check", and CI is the first place a mistake
surfaces. That is true of the *whole* file. It is NOT true of the pieces you actually add, and
treating it as if it were is how a type error or a `printf` mismatch burns a CI cycle.

## Lift the new helpers into a standalone harness and really compile them

Any `static inline` helper you add here is self-contained — it needs `<stdint.h>`, `<stddef.h>`
and the two or three `obs_source_t` fields it touches. So splice it VERBATIM out of
`obs-source.c` (never retype it — a retyped copy verifies your typing, not the shipped code)
into a file with a minimal stub, and compile it for real:

```c
struct obs_source_frame { uint64_t timestamp; };
typedef struct obs_source {
    struct { struct obs_source_frame **array; size_t num; } async_frames;
    uint64_t genlock_phase_anchor_ns;
} obs_source_t;
/* ...the lifted helpers, byte-for-byte... */
```

```bash
gcc -std=gnu99 -Wall -Wextra -Wformat=2 -Wconversion -c harness.c -o /dev/null
```

**Do the same for a `blog()` call you add or edit** — splice the format string and its exact
argument list into a `printf` in the harness. `-Wformat=2` then checks the specifiers against
the real argument types, which is the single easiest thing to get wrong in this file
(`%zu` vs `%llu` vs `%lld`, and the casts around `size_t`/`uint64_t`).

This is a *different* technique from the one in `audio-quality-measurement.md`, which links the
box's own system libraries to mimic vendored library behaviour. Here the vendored code itself is
what gets compiled — no substitution, so a pass is evidence about the shipped bytes.

Cheap structural check to run alongside it: compare `{}`/`()`/`[]` deltas against `git show
HEAD:<file>`. A non-zero difference means an unbalanced edit, in one second.

## Promote the harness to a committed gate when it checks a MIRROR

`src/genlock_backlog.rs` is the Tier-0 authority and the C is the port; they are required to be
numerically identical, and every other guard in this repo asserts that only by static text
anchor — which proves the C still *says* the right thing, never that it *computes* it.
`tests/genlock_relock_selection_parity.rs` closes that: it lifts the helpers by name, compiles
them under `-Werror` against the stub, runs them over a spread of vectors, and requires
byte-identical results from the Rust authority. It **fails loudly rather than skipping** when no
compiler is present (a parity test that silently passes without running is worse than none).

Two properties to preserve if you touch it: the helpers it lifts must stay **contiguous** in
`obs-source.c`, and the lift is by function name.

### When the lifted helper has NO Rust consumer — make the gate STD-ONLY so it runs offline (issue 767)

`genlock_relock_selection_parity.rs` compares the C to a `use camera_box::...` Rust authority, so it
needs the crate + `CARGO_TARGET_TMPDIR` and runs ONLY under `cargo test` (CI — camera-box's
`# airuleset:build-ok` is disabled, so you can't `cargo test` it locally at all). A vendored-C
DECISION helper that NOTHING in the Rust appliance calls (e.g. `genlock_reconnect_decision` in
`vendor/distroav/src/ndi-source.cpp`, a DistroAV-receiver-loop-only choice) has no such authority to
compare against — so DON'T invent a crate-root module just to have one. Instead make the whole gate
**self-contained / std-only** (`tests/distroav_ndi_reconnect_767.rs`): it `fs::read_to_string`s the
vendored file, lifts the `static inline` helper by signature → first `\n}\n`, compiles it with `cc`
against a tiny `<stdint.h>`/`<stdbool.h>` stub + a `main()` driving a hand-written **truth table**,
and asserts each hardcoded expected bool. The truth table IS the spec (no Rust reference needed), so
this runs BOTH under `cargo test` AND standalone via the issue-1026 recipe
(`CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/<file>.rs -o /tmp/x && /tmp/x`) — a real
local RED→GREEN with no cargo/OOM contention, and the `cc` invocation still `-Werror -Wconversion
-Wformat=2` compile-checks the shipped bytes and panics loud (never skips) when no compiler.

Two gotchas proven live here: (1) put ONE hand-picked vector at EVERY guard boundary
(exact-threshold, just-under, each early-return guard) AND at least one vector with a DIFFERENT
value for any parameter the helper takes (else a helper that hardcodes the constant instead of using
the parameter passes every vector) — then mutate the C on a scratch copy and watch the specific
boundary vector go RED, per the section below. (2) a bare `s->config.reset_ndi_receiver = true;`-style
source anchor is easily ALIASED (the same statement text recurs at other sites with `= false` /
`= <var>`); anchor the UNIQUE multi-line squished adjacency (e.g. the mutex-lock + flag-set + unlock
+ timestamp-refresh trio) instead, and confirm `.count()==1` in the squished file before trusting it.

## A parity or mutation gate is a LIE until you watch it go red

Live, in this ticket: the parity gate passed 129 vectors, then mutating the C tie-break from
`<` to `<=` **still passed all 129** — not one vector produced an exact tie, so the gate was
blind to the exact contract it claimed to guard. Four deliberate exact-tie vectors were added
and the same mutation now diverges on 4 of 133.

So: after writing any such gate, **mutate the thing under test and confirm the gate fails.** For
nearest-neighbour selection specifically, an exact tie is reachable rather than theoretical —
both rig sender grids are EVEN (33,333,300 ns and 16,666,600 ns), so `i*grid + grid/2` is an
exact integer nanosecond a wall instant can land on. Construct the tie directly:
`wall = BASE + i*grid + grid/2 + age`.

## Adding REMEMBERED STATE: enumerate the invalidation seams before writing the tests

A per-source field that survives across ticks (`genlock_phase_anchor_ns`,
`genlock_locked_next_boundary_ns`, `genlock_last_known_n`) is only correct if every event that
invalidates it clears it. Write that list FIRST — the tests you would otherwise write all pass
while a seam is missing, because each one exercises the happy path.

The seam list for this file, learned the hard way:

- **all three** `free_async_cache(source);` call sites (the explicit flush, the overrun
  force-drain, AND the `async_texture_changed()` re-alloc) — each destroys the whole delay line;
- `genlock_backward_regime_end()` — the wall clock moved, so every `wall - ts` age sampled
  before the correction is wrong by exactly the step;
- `obs_source_set_genlock_latency_ms()` when the value actually changes — the remembered state
  describes a hold that no longer exists.

That last one was the critical review finding on #1003: a latency **decrease** left a stale
anchor, so the relock selection shed nothing while the lowered threshold qualified the backlog
branch every tick — and because that branch pre-empts STEADY, `drain_eligible` never got set and
the settle-back drain never ran either. 923 → 400 ms parked at the old hold with 800 relocks in
800 ticks. Guard the *shape*, not just the instance: `tests/genlock_release_cadence.rs` counts
anchor-clears against `free_async_cache()` call sites, so a fourth seam cannot be added silently.

**Adding a NEW remembered-state field that clears at the SAME seams — put its clear AFTER
`free_async_cache(source);`, never between it and `genlock_phase_anchor_ns = 0;` (#1161).** The
#1003 seam guard above counts the exact squished adjacency
`source->genlock_phase_anchor_ns = 0; free_async_cache(source);` and asserts it equals the
`free_async_cache(source);` count. A second field (e.g. `genlock_acquire_bracket_ticks`, #1161)
must clear at the identical three sites, but inserting `source->genlock_new_field = 0;` BETWEEN the
anchor-clear and the `free_async_cache` call SPLITS that adjacency → the #1003 count drops to 0 and
its `seams == frees` test fails (cost one live break here). Place the new clear on the line
*after* `free_async_cache(source);` — functionally identical (both run under `async_mutex`, no tick
in between, and `free_async_cache` reads neither field) — and guard the new field RELATIONALLY the
same way: count `free_async_cache(source); source->genlock_new_field = 0;` == the `free_async_cache`
count, plus a scoped `.contains` check inside `genlock_backward_regime_end`. Never guard it by the
`phase_anchor = 0; new_field = 0;` adjacency — that adjacency only survives at the regime-end /
setter sites, not the `free_async_cache` ones, once you (correctly) place the new clear after the
free.

## Static anchors slice on expressions you are about to change

Several guards in `tests/genlock_release_cadence.rs` locate the relock branch by `.find()` on a
literal statement (it was `"release = due;"`, now `"release = sel_1003 + 1;"`). Change that
statement and the `.expect()` fires *before* any assertion runs, so the failure message points
at the wrong thing entirely. Same family as the `recording-e2e.sh` static-anchor gotchas in the
project CLAUDE.md, and as `av-sync-dock-anchor-refactor-safety.md` for the dock C++.

Two habits that keep the anchors honest:

- **Scope to the enclosing function, never a fixed byte window.** A `[..600]` window here failed
  the moment a comment grew — the exact "a byte cap is a PROXY that rots" lesson the #940 anchor
  already records. Slice to the next top-level `static ` instead.
- **Keep each anchor SHORT and wrap-independent.** A long literal encodes where clang-format
  chose to wrap and what a loop variable is called; a formatter bump then fails the gate with a
  misleading message. Prefer two short anchors over one long one.

And whatever you anchor in `tests/genlock_release_cadence.rs`, mirror it into **both**
`windows-genlock.yml` and `windows-genlock-fast.yml` (the issue-912 lesson). Their pwsh squishes
whitespace the same way (`-replace '\s+', ' '` vs Rust's `split_whitespace().join(" ")`), so the
same literal works in both — verify each one against the real squished C before committing,
since `pwsh` is not installed on dev1 and a wrong literal fails only on a Windows runner.

## The source-rate multiple is measured from the STAMP GRID — and that can lie about arrival rate (#1042)

`genlock_measure_source_multiple()` derives `n = round(canvas_interval / source_interval)` where
`source_interval` comes from the front queued frame STAMPS. Since #1042 it is the **MINIMUM**
strictly-increasing adjacent delta over the first `GENLOCK_MEASURE_SCAN_DEPTH` entries, not the
FIRST — every rig source stamps on the monotonic, evenly-spaced DanteSync grid, so the true frame
interval is the SMALLEST gap; a duplicate or a dropped/decimated frame only ever ENLARGES a gap.
Taking the first increasing pair let a pair that straddled a dropped frame read 2 source intervals
(33.3 ms → n=1 on a 60fps source), collapsing `genlock_backlog_relock_qdepth()`'s threshold below
the source's real held depth and firing the backlog branch spuriously (~1/sec — the #796
health-signal complaint). The min-delta authority is the pure Tier-0 seam
`src/genlock_backlog.rs::source_interval_from_stamps`; the probe `measure_source_multiple` calls
it and the C `genlock_measure_source_multiple` mirrors the same min-loop. **If you touch this
derivation: min-delta is byte-identical to first-pair on any clean grid-stamped window, so a
change that "simplifies" it back to `break`-on-first silently reintroduces #1042 on any source
whose front window carries a gap.** It also feeds `effective_source_multiple` (the LIVE release
cadence), so a wrong `n` here is not just a threshold bug.

**Diagnosing a source's TRUE buffer-arrival rate when its stamps look coarse (the #1042 method):**
the stamp grid the measure reads can DIVERGE from how fast buffers actually arrive. Read the
`genlock-fifo audit '<src>'` line's COUNTER deltas across two consecutive lines instead — they are
emitted ~every 5 s and each carries a real `ts_present=<epoch_ns>`, so
`received_delta / (ts_present_delta / 1e9)` is the real buffer-arrival rate, and `consumed_delta`
/ `dropped_due_delta` are the presented / decimated rates. Live #1042: `Zaloha kamera` showed
`received` +300 / 5.000 s = **60 buffers/sec** (a genuine 60fps source) while `consumed` +150 =
30/sec and `dropped_due` +150 = 30/sec (correct 60→30 decimation) — that `received` delta is what
proved the source was 60fps and the n=1 threshold was the bug, not the source. (Same "never divide
a counter delta by a wall-clock sleep" caution as the #797 phantom-50fps post-mortem in the
genlock skill — use the audit line's OWN `ts_present` delta, not a `sleep`.) The audit line prints
this read-only over the win-* MCP `Shell` on the stream/strih box (session-agnostic file read of
the newest `%APPDATA%\obs-studio\logs\*.txt`).

## Extracting a large inline block into its own static function (#1038)

`ready_async_frame()` grew to ~832 lines; the issue-401 cadence was lifted into
`static bool genlock_release_tick(...)`. A pure "cut, dedent, wrap, call" move, but two traps cost
real time — both invisible to local Tier-0 (the file compiles only on CI):

- **A function that is ALSO forward-declared has TWO matching lines.** `ready_async_frame` has a
  forward declaration (`...sys_time);`) near the top of the file AND its definition (`...sys_time)`
  + `{`) far below. Inserting the new helper "before the line that startswith the signature"
  matches the FORWARD DECLARATION first and drops the helper ahead of every function it calls →
  implicit-declaration `-Werror` on CI. Match the DEFINITION line EXACTLY (no trailing `;`, or the
  `)\n{` pair) when placing a helper before such a function.
- **A moved block that only ASSIGNS an enclosing-scope local leaves it undeclared.** The cadence
  did `next_frame = source->async_frames.array[0];` — `next_frame` was DECLARED at the top of
  `ready_async_frame`. In the extracted function that assignment has no declaration → `next_frame
  undeclared`. The lift-and-compile harness (above) caught it in one second; a text-only "verbatim
  move looks right" review would not, and CI would be the first to know. Add the type at the first
  assignment site (`struct obs_source_frame *next_frame = ...`) — it is assigned-before-read in the
  block, so a fresh local is exact.

**Anchor safety for a whole-block move here was FREE:** every gate in
`tests/genlock_release_cadence.rs` and both `windows-genlock*.yml` is either a whole-file
`contains`/`-match` substring or a statement-level `.find` with no leading indentation (immune to a
uniform dedent), and the two enclosing-function slices (the `genlock_relocks++` → next `\nstatic `
window in the #741 / #940 tests) still resolve *inside* the new function BECAUSE it is placed
before `ready_async_frame` — so its relock branch's next `\nstatic ` is `ready_async_frame` itself,
and `release = sel_1003 + 1;` stays within the window. Moving the whole cadence as ONE contiguous
function (not per-branch helpers) is what keeps those slices intact — a split would put a `\nstatic`
between `genlock_relocks++` and its terminal `release =`. Prove the move byte-for-byte: re-indent
the new body back to the original depth and `diff` it against `git show HEAD:<file>` over the old
line range; the ONLY intended delta is the added declaration token.


## Bringing an AUX ndi_filter path under a libobs decision, and lift-compiling a NEW EXPORT (#879)

The strih aux NDI senders (interkom / MULTIVIEW / Grading) are `ndi_filter` republishes in the
DistroAV plugin, NOT the program (the program is `ndi_output`, a separate source type rendered in
`output_frames()` BEFORE `render_displays()` each graphics tick). `ndi_filter_render_video()` did a
full texrender + stagesurface readback + send every tick with no budget/cadence term — it never
entered `render_display()`, so the adaptive render budget (`obs_display_should_skip`,
`obs-display-budget.h`) could not see it. Pattern to bring such a path under the SAME budget without
inventing a second mechanism: add ONE EXPORTed libobs seam (`obs_aux_sender_should_skip()` in
`obs.c`, declared in `obs.h`) that reads `obs->video.graphics_frame_start_ns` +
`video_frame_interval_ns` (the two globals `render_display()` already budgets against) and delegates
to the pure `obs_display_should_skip()`; keep per-instance EWMA + counters on the plugin struct.
Program priority stays STRUCTURAL (different source type, never routed through the gate).

- **Budget-model render-order caveat — RESOLVED by issue 1063.** `elapsed = now -
  graphics_frame_start_ns` only reflects the program cost if the aux decision falls AFTER
  `output_frames()`. Aux scenes shown via their own surfaces render in `render_displays()` (after
  the program) so it held on strih; an aux scene embedded in the PROGRAM scene would decide early
  and see false headroom. Issue 1063 closed this: `obs_graphics_thread_loop()` now publishes the
  PREVIOUS tick's completed `frame_time_ns` into `obs->video.last_tick_total_ns` (obs-internal.h),
  and `obs_aux_sender_should_skip()` gates on `max(elapsed, last_tick_total_ns)` — order-independent,
  fail-open (the field is 0 before the first completed tick, so it is byte-identical to `elapsed`
  at startup). Two constraints when touching this seam again: (a) keep the seam BRACE-FREE
  (single-statement `if`s + ternaries only) — `tests/aux_sender_budget_879.rs` finds its closing
  brace via the first `"\n}"` after the signature, and the new max-term uses a `? :` ternary
  precisely to avoid a nested block; (b) BOTH lift-compile stubs (the 879 invariant test AND the
  1063 order-independence test) carry the `last_tick_total_ns` field in their `video_stub`, and the
  field + publish line + max-term are anchored in `genlock_preload.rs` and BOTH windows-genlock ymls.
- **Audio is a separate filter callback:** gate ONLY `ndi_filter_render_video`; leave
  `ndi_filter_asyncaudio` untouched so interkom talkback audio stays full-rate.

**Lift-and-compile a NEW libobs EXPORT seam, not just a `static inline` helper.** A seam that reads
the `obs` global is still liftable: stub the global (`struct obs_stub { struct video_stub video; };
static struct obs_stub _obs; static struct obs_stub *obs = &_obs;`) + `static uint64_t
os_gettime_ns(void)`, `#include` the REAL header for the pure helpers, and splice the seam VERBATIM
from `obs.c`. `tests/aux_sender_budget_879.rs` does this (parity vs the Rust authority + the
never-freeze/program-priority invariants) — the same "compile the shipped bytes" evidence the
genlock parity gate gives.

- **GCC-14 TRAP in such a stub:** `static struct { .. } _obs; static struct { .. } *obs = &_obs;`
  declares TWO distinct anonymous struct types — an incompatible-pointer-types *warning* on GCC 13
  but a hard *error* on GCC 14 (a `compile()` that only checks exit status passes today, breaks the
  day a runner moves to GCC 14). Name the struct ONCE (`struct obs_stub`). Compile the stub under
  `-Werror` to catch it now.

**Deploy coupling:** adding a NEW libobs EXPORT means the DistroAV plugin DLL now imports a symbol
that only the rebuilt `obs64.dll` provides — a partial deploy of just the plugin against an old
`obs64.dll` fails plugin LOAD (unresolved symbol). Always FULL-BUNDLE deploy after a libobs export
change (see `rig-state-inspection.md`).

**Additive header helpers keep existing anchors safe.** `obs_effective_render_divisor()` was APPENDED
to `obs-display-budget.h` rather than refactoring `render_display()`'s inline #776 derivation, because
both windows-genlock ymls + `genlock_preload.rs` pin `uint32_t effective_divisor =
display->render_divisor;`. When you leave a second inline copy of a derivation, ANCHOR it too
(`genlock_preload.rs` now pins the obs-display.c `derived = ...` line) so the two cannot silently
diverge.

## Observing RED→GREEN on a source-anchor test when the vendored change can't run cargo (#1026)

The `# airuleset:build-ok` bypass is DISABLED for camera-box (CLAUDE.md Local Build Policy), so you
CANNOT `cargo test` a source-anchor guard locally at all, and the vendored C compiles only on CI.
But a `tests/*.rs` guard that only reads a vendored file's TEXT (`fs::read_to_string` +
`.contains()`/`.find()` anchors — the whole class this rule's committed-gate section produces) uses
NOTHING but `std`, so you can compile AND run it standalone with plain rustc, no cargo, no crate,
no OOM contention with sibling workers' `cargo` builds on the shared box:

```bash
CARGO_MANIFEST_DIR=<worktree-abs-path> rustc --test --edition 2021 tests/<file>.rs -o /tmp/anchortest
/tmp/anchortest        # runs the #[test] fns; exit 101 = RED, 0 = GREEN
```

`env!("CARGO_MANIFEST_DIR")` (used by the `repo()` path helper) must be set on the compile command;
the test re-reads the vendored file at RUNTIME, so after applying the fix you re-run the SAME binary
(or recompile) to watch RED→GREEN for real — a genuine observed transition, not just "committed a
[red] then a [green]". This is the Rust-anchor-test counterpart to the C lift-and-compile harness
above; use both on a vendored change (harness proves the C compiles, standalone rustc proves the
guard bites). Confirmed live #1026 (obs-canvas.c UAF fix): 2 failed → 2 passed via `rustc --test`.

### The SAME standalone-rustc recipe runs a PURE-STD crate-root `src/<mod>.rs` too — the Tier-0 mirror's own RED→GREEN, no cargo (#771)

The `# airuleset:build-ok` bypass is genuinely DISABLED for camera-box (the tier0 hook blocks
`cargo test --lib <mod> # airuleset:build-ok` outright — do NOT trust any older rule note, e.g.
`jitter-audit-parser.md`, that claims it works here; it does not). So a vendored change's Tier-0
MIRROR (`src/render_budget.rs`, `src/mv_audit.rs`, `src/genlock_backlog.rs`, `src/jitter_audit.rs`
…) can't be `cargo test`-run locally either. But if that module uses ONLY `std` (no `use
camera_box::…`, no external crate), `rustc --test` compiles it AS ITS OWN crate and runs its
`#[cfg(test)] mod tests` — the identical #1026 recipe, just pointed at a `src/` file instead of a
`tests/` one:

```bash
rustc --test --edition 2021 src/<mod>.rs -o /tmp/modtest && /tmp/modtest   # exit 101 = RED, 0 = GREEN
```

No `CARGO_MANIFEST_DIR` is needed (a pure-std module doesn't call the `env!` path helper). Confirmed
live #771: `src/mv_audit.rs`'s 7 unit tests (floor/parse/classify/gate) ran GREEN this way while
`cargo test` was Tier-0-banned. Pair it with the `tests/<file>.rs` anchor recipe above: the anchor
proves the vendored C still carries the change, the pure-std module proves the mirror LOGIC is
right — both with plain rustc, zero cargo/OOM contention with sibling workers.

**STALE-CLAIM CORRECTION (#1110, 2026-08-27): `src/mv_audit.rs` is NO LONGER purely std** — its
`floor_tracks_the_effective_target_not_canvas_over_two` test gained a `use
crate::render_budget::effective_render_divisor;` import, so a plain `rustc --test src/mv_audit.rs`
now FAILS with `unresolved import crate::render_budget` (that module doesn't exist in a single-file
compile). To still get the local RED→GREEN, run a HARNESS that substitutes a byte-identical LOCAL
copy of the one imported fn for the `use crate::…` line before compiling (a ~15-line python/sed
splice, then `rustc --test` the result) — this verifies the mirror LOGIC while CI compiles the real
file. `tests/mv_audit_emit.rs` (the vendored-source ANCHOR guard) stays genuinely std-only and runs
via the plain recipe above unchanged. General rule: before trusting "run `src/<mod>.rs` standalone",
grep the module for any `use crate::` (incl. inside `#[cfg(test)] mod tests`) — one such import in a
test makes the plain recipe fail, and the substitute-the-import harness is the fix.

### Adding a NEW observability audit line to the render loop (#771)

The `multiview-audit:` line pattern is the reusable shape for surfacing a render-loop signal to the
OBS log (the drift-guard / rig-health-audit / E2E-preflight facet — same three consumers as the
`genlock-fifo audit` lines): (1) emit a `blog(LOG_INFO, "<marker>: k=v k=v …")` from the render
loop with a MUTUALLY-NON-SUBSTRING marker (so the `jitter_audit`-family parsers stay independent);
(2) any threshold/floor is a pure `static inline` helper in `obs-display-budget.h`, MIRRORED
byte-identically in a pure-std `src/<mod>.rs` (Tier-0, run it via the recipe above); (3) a
`key=value` token-scan parser + a `*-gate` bin as the consumer; (4) lock-step anchors in
`tests/genlock_preload.rs` (probe-gated, CI-only) AND BOTH `windows-genlock*.yml` pwsh steps —
verify every pwsh anchor OFFLINE against the real `re.sub(r'\s+',' ',text)`-squished file with a
throwaway python script (pwsh is not on dev1), plus a std-only `tests/<mod>_emit.rs` anchor for
local RED→GREEN. Lift-compile the new `blog()` format string into a `printf` harness under
`-Wformat=2` before pushing.

## The obs-websocket enum crash class: a borrowed `output->video`/state pointer outliving its owner (#793, #1026)

Two live imag-nb SIGSEGVs now trace to the SAME shape: an obs-websocket enum request (on a Qt WS
worker thread) reads a libobs pointer that a video/mix reset freed underneath it. #793 was
`GetStats` → `obs_get_video()` reading a freed `canvas->mix`; #1026 was `GetOutputList` →
`obs_output_get_width` → `get_const_root` walking a freed `output->video` (the inactive
`virtualcam_output` kept a create-time `obs_get_video()` copy across the startup video reset). The
enum's `outputs_mutex`/list lock protects the LIST, never the borrowed `video_t` each node points
at. Fix invariant (both tickets): DETACH the borrowed pointer to NULL BEFORE the owner is freed
(`obs_canvas_clear_mix` clears `canvas->mix` for #793 and now `output->video` for #1026), so the
already-NULL-safe `media-io/video-io.c` getters return 0 instead of dereferencing freed memory. If
a THIRD WS-enum reader crashes on some other borrowed pointer, look for the same "set once, never
cleared on stop, freed by a reset the reader doesn't lock against" lifecycle before anything else.
## Aux ndi_filter TEARDOWN ordering + why "disable" never destroys a sender (#877)

Reasoning about an aux-sender wedge ("disabling all three aux NDI senders wedged PROGRAM to 0 fps")
needs two non-obvious facts about the DistroAV `ndi_filter` (interkom / MULTIVIEW / Grading):

- **A DISABLED filter's `video_render` is NEVER called, so disable triggers NO teardown.**
  `ndi_filter` is `OBS_SOURCE_TYPE_FILTER` with `OBS_SOURCE_VIDEO` (see `create_ndi_filter_info`).
  In `vendor/obs-studio/libobs/obs-source.c` `render_video()`: `if (!source->context.data ||
  !source->enabled) { obs_source_skip_video_filter(source); return; }` — a disabled filter is
  skipped BEFORE `ndi_filter_render_video`. So on disable the DistroAV code runs nothing for that
  filter (no texrender, no send, no `send_destroy`); the sender stays alive+idle. The destroy paths
  (`ndi_filter_destroy` / `ndi_filter_remove` / `ndi_sender_destroy`) hang on OBS remove/destroy
  callbacks, NEVER on enable/disable. Any "three senders destroyed at once on disable" premise is
  therefore wrong against current code — check `render_video()`'s enabled-gate before believing it.
- **The teardown ORDERING is upstream-original and correct — and now locked.** `ndi_filter_destroy`
  calls `video_output_close(f->video_output)` (stops + JOINs the raw_video send worker) BEFORE
  `ndi_sender_destroy(f)`, which holds BOTH `ndi_sender_video_mutex` + `ndi_sender_audio_mutex`
  before `send_destroy` — so no synchronous `send_send_video_v2` is in flight under the mutex when
  the sender is destroyed. `tests/aux_sender_teardown_ordering_877.rs` is a Tier-0 static gate that
  locks both orderings (with a baked-in mutation proof: reordered + missing-lock fixtures must be
  rejected). Sig anchors match the DEFINITION line `...)\n{` to dodge the forward-decl trap
  (`ndi_sender_destroy` has a `;`-terminated forward decl at the top of the file).
- **The observed 0-fps root cause is NDI-SDK internal, not in readable code.** The PROGRAM output
  (`ndi-output.cpp`) uses async `send_send_video_async_v2` (blocks the next call until the SDK frees
  the previous async buffer); three aux senders going silent/renegotiating at once churns the shared
  per-process NDI transmit/connection threads and can block the program's next async send. Not
  fixable/confirmable from the vendored source; confirming it needs a live rig repro (a rig write).

## Adding a per-display / per-present GL behavior WITHOUT touching the D3D11 (Windows) path (#1107)

imag-nb runs the ONLY Linux OBS (EGL/X11); strih+stream are libobs-d3d11. A change to the GL
winsys present (`gl-x11-egl.c` / `gl-wayland-egl.c`) is byte-identical for strih/stream by
construction — but a NEW per-present/per-display PROPERTY that must plumb from the frontend down to
the GL present can still force a cross-backend vtable touch. The pattern that keeps D3D11
byte-identical (used for #1107's tear-free vsync — vsync the fullscreen program projector only):

- Store the property on the GL `struct gs_device` (gl-subsystem.h), defaulting the SAFE/old value:
  `device_create` uses `bzalloc`, so a `bool` starts false = old behavior. Read it in the EGL
  present (`eglSwapInterval(edisplay, device->present_vsync ? 1 : 0)`) — x11 AND the wayland twin.
- Expose a setter as a NEW backend export `device_present_set_vsync` in gl-subsystem.c (default
  visibility, like `device_create`). Add it to the `gs_exports` vtable (graphics-internal.h) but
  import it with **`GRAPHICS_IMPORT_OPTIONAL`** (graphics-imports.c), NOT `GRAPHICS_IMPORT`.
  OPTIONAL = a bare `os_dlsym` with no failure flag, so libobs-d3d11/metal — which never define the
  symbol — leave the pointer NULL; a mandatory `GRAPHICS_IMPORT` sets `success=false` and BREAKS
  their module load. This is the whole trick: the Windows D3D path is untouched and cannot even see
  the feature. The public `gs_present_vsync(bool)` wrapper (graphics.c/.h) NULL-guards the optional
  export, so it is a no-op on D3D11.
- libobs side: a `bool` on `struct obs_display` + `obs_display_set_vsync()` (obs-display.c/obs.h);
  `render_display()` arms it each tick IMMEDIATELY before `gs_present()`. A DEVICE-level present
  flag re-armed per-display each tick is correct BECAUSE render_display runs sequentially on the ONE
  graphics thread. Do NOT put it on the swapchain / `gl_windowinfo` (winsys-private, and the wayland
  windowinfo is bmalloc'd → no safe zero default).

**Identifying "the program projector" — `render_divisor` is NOT the discriminator.** imag's OBS
display list has THREE displays: the OBS main window + preview (render_divisor 0, on eDP-1,
occluded but STILL rendered+presented — no compositor), the Multiview projector (render_divisor 2,
eDP-1), and the Program projector (render_divisor 0, HDMI-1). So `render_divisor <= 1` matches BOTH
the main window AND the program — vsyncing both stacks two DIFFERENT-monitor blocking presents per
tick and can drop the genlock 60fps. The program is identified in the FRONTEND (`OBSProjector.cpp`)
by `savedMonitor > -1 && !isMultiview` (fullscreen, non-multiview), mirroring the #276
`isMultiview → obs_display_set_render_divisor` mark. `savedMonitor` (a member set by `SetMonitor()`
in the ctor synchronously, BEFORE the async DisplayCreated lambda) is the fullscreen signal; re-arm
it on the runtime `OpenFullScreenProjector`/`OpenWindowedProjector` toggles too (the DisplayCreated
mark runs only ONCE — the #1107 review 🟡).

**Two live pre-deploy checks for any imag EGL vsync change:** (1) confirm imag's OBS launch env has
NO `vblank_mode`/MESA/driconf clamp (a `vblank_mode=0` FORCES interval 0 and silently defeats
`eglSwapInterval(1)`) — imag currently launches plain `obs --disable-shutdown-check`, clean. (2) A
blocking vsync present on the program couples the genlock render thread to the panel clock
(video_sleep→genlock_next_deadline targets ABSOLUTE DanteSync boundaries; a late arrival →
`count>=2` → a duplicate encode frame) — keep it to ONE present (program only) and rig-verify render
stays 60.00fps / 0 renderSkip / 0 lagged_frames growth; fallback is a +50–100 ppm-fast HDMI-1
modeline.

**Testing + deploy:** a `tests/*.rs` text-anchor guard (pure std, the #756 gl-x11-egl precedent,
run via the #1026 standalone-rustc recipe) goes RED if the patch regresses — the vendored C
compiles only on `linux-genlock.yml`, and this change spans the FRONTEND (obs binary) too, so the
imag deploy is FULL-BUNDLE (frontend + libobs + libobs-opengl.so via setup-imag.sh), never a
libobs-opengl-only hot-swap.
