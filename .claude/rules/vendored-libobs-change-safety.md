---
paths:
  - "vendor/obs-studio/libobs/obs-source.c"
  - "vendor/obs-studio/libobs/obs-internal.h"
  - "tests/genlock_release_cadence.rs"
  - "tests/genlock_relock_selection_parity.rs"
  - "vendor/obs-studio/libobs/obs.c"
  - "vendor/obs-studio/libobs/obs.h"
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
