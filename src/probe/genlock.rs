//! Genlock FIFO consume / preload decision logic (camera-box #42 → #70 → #97 → #102).
//!
//! This is the camera-box-side, pure, unit-tested MIRROR of the C decision logic
//! baked into the vendored OBS genlock FIFO
//! (`vendor/obs-studio/libobs/obs-source.c`, the `genlock_fifo` branch of
//! `ready_async_frame()` + `genlock_parse_preload()` + `genlock_decide()`). Keeping
//! the contract here lets CI prove the parse/clamp + consume rules without an OBS
//! build, and the `tests/genlock_preload.rs` vendored-source guard keeps the C side
//! in lock-step.
//!
//! ## The preload is a video-delay; the consume gate must not drop distinct frames
//!
//! The genlock FIFO (#42) consumes queued frames against a wall-clock render tick.
//! `preload` is the depth of a deliberate jitter buffer / **video delay** (#97):
//! `preload` frames buffered = `preload` frames of genlock-disciplined delay, used to
//! push the program video back ~1 s to match late audio on stream.lan.
//!
//! The #70 attempt gated consumption on `queue_depth > preload` on EVERY tick, so any
//! NDI arrival-*jitter* dip below the reserve REPEATED the last frame and lost one
//! DISTINCT frame; a deep #97 preload (≈1 s) made this catastrophic (11.6 % @
//! preload=1 → 34.3 % @ preload=30 on the live rig, underrun-dominated). #102 fixes
//! it: BUILD the delay once at startup ([`genlock_decide`] with the `filled` latch),
//! then consume a distinct frame on EVERY tick a frame is queued, repeating ONLY on a
//! true empty. A deep preload is then a CLEAN delay line — it holds the delay but
//! never drops a distinct frame ⇒ ~0 distinct-frame loss at any depth.

/// Default reserve when `OBS_GENLOCK_PRELOAD_FRAMES` is unset/invalid: one frame
/// (= one frame of latency per hop, the "1 frame per hop" the task calls for).
pub const GENLOCK_PRELOAD_DEFAULT: u32 = 1;

/// Hard cap on the reserve.
///
/// History (#70): originally 28, because the FIFO drop-cap was a single global
/// `MAX_ASYNC_FRAMES = 30` and the steady-state queue parks at `preload + 1`, so
/// any `preload` whose `preload + 1` reached 30 force-drained every refill and
/// FROZE the source. 28 ⇒ steady depth 29 < 30 was the highest safe reserve.
///
/// (#97) The preload is now a per-source, runtime-settable **video-delay** control
/// (one preload frame = one frame of genlock-disciplined delay), used to push the
/// program video back ~1 s to line up with late audio on stream.lan. At 30 fps,
/// ~1 s is 30 frames — already above the old cap — so the ceiling is raised to
/// **128** (≈ 4.3 s @ 30 fps / ≈ 2.1 s @ 60 fps), enough headroom for any realistic
/// audio offset. The old "must stay below MAX_ASYNC_FRAMES" invariant is replaced
/// by the per-source drop-cap ([`genlock_drop_cap`]): a delayed source's FIFO is
/// allowed to hold `preload + RESERVE` frames, so a deep preload no longer
/// force-drains. See [`genlock_drop_cap`] / [`GENLOCK_DROP_CAP_RESERVE`].
pub const GENLOCK_PRELOAD_MAX: u32 = 128;

/// Headroom above a genlock source's `preload` for the per-source FIFO drop-cap.
///
/// The drop-cap must sit a few frames ABOVE the steady-state depth (#102: the
/// consume-when-queued gate parks at `preload`) so normal producer/consumer jitter
/// never reaches it and force-drains the buffer (an overrun that resets the FIFO and
/// re-introduces a glitch). `+4` leaves slack above steady state. See
/// [`genlock_drop_cap`].
pub const GENLOCK_DROP_CAP_RESERVE: u32 = 4;

/// libobs' fixed async FIFO drop-cap for NON-genlock sources (`MAX_ASYNC_FRAMES`).
/// Mirrored here so the per-source drop-cap logic ([`genlock_drop_cap`]) can be
/// unit-tested without an OBS build; the vendored-source guard
/// (`tests/genlock_preload.rs`) keeps this in lock-step with the C `#define`.
pub const MAX_ASYNC_FRAMES: u32 = 30;

/// (#184) Default sub-frame jitter reserve in milliseconds: `0` = DISABLED, so the
/// timestamp-aligned release falls back to the whole-frame `preload` delay exactly
/// as before (full back-compat). Any value > 0 switches the release deadline to a
/// MS-GRANULAR reserve (see [`genlock_present_ts_reserve`]) — the held latency
/// becomes ≈ `reserve_ms` (single-digit ms = just the measured arrival jitter)
/// instead of a whole 33 ms frame. Mirrored & guarded in the C side
/// (`#define GENLOCK_RESERVE_MS_DEFAULT 0`).
pub const GENLOCK_RESERVE_MS_DEFAULT: u32 = 0;

/// (#184) Hard cap on the sub-frame jitter reserve (ms). A reserve is a *sub-frame*
/// jitter budget — it only ever needs to cover the few-ms inter-arrival spread
/// (measured 1.6 ms strih→stream, 8.1 ms cam1→strih on the live rig). 100 ms (= 3
/// whole frames @ 30 fps) is a generous ceiling that still keeps the knob in its
/// sub-frame / low-single-frame regime; anything larger means the operator wants a
/// whole-frame *video delay*, which is the `preload` knob's job, not this one.
/// Mirrored & guarded in the C side (`#define GENLOCK_RESERVE_MS_MAX 100`).
pub const GENLOCK_RESERVE_MS_MAX: u32 = 100;

// ---- #235: ONE user-facing genlock latency knob (ms), frames in parens ------
//
// History: genlock latency used to be set via TWO confusing env knobs —
// `OBS_GENLOCK_PRELOAD_FRAMES` (whole frames, #70/#97) AND `OBS_GENLOCK_RESERVE_MS`
// (ms, #184) — where the reserve OVERRODE the preload delay ONLY when
// `OBS_GENLOCK_TS_ALIGN` was on. That precedence ("when reserve is set, preload
// doesn't apply, but only under TS_ALIGN") confused everyone. #235 consolidates them
// into ONE canonical ms knob: the held latency is a single value in milliseconds.
//
// * Canonical knob = `OBS_GENLOCK_LATENCY_MS` — THE latency, in ms.
// * `OBS_GENLOCK_RESERVE_MS` is kept as a BACK-COMPAT ALIAS (so existing deploys,
//   scripts, and the #128 relaunch wrapper that set RESERVE_MS keep working; the
//   validated prod `reserve=3` maps cleanly to `latency_ms=3`). The canonical knob
//   WINS when both are set.
// * Setting the ms knob > 0 implies timestamp-aligned release ON (no separate
//   `OBS_GENLOCK_TS_ALIGN` user gate) and a release deadline of `wall_now - latency_ms`
//   (the validated #184 path).
// * `preload` (the FIFO jitter/dropout buffer depth) becomes INTERNAL / auto-derived
//   ([`genlock_auto_preload`]) — NOT a competing latency knob. It is latency-free
//   under the ms deadline and holds >= [`GENLOCK_AUTO_PRELOAD_MIN`] frame so the #110
//   0-loss floor holds.
// * Display ([`format_latency_label`]) is "N ms (≈ M frames @ Ffps)" — ms primary,
//   the whole-frame equivalent ([`ms_to_frames`]) in parentheses (the user's ask).

/// (#257) The genlock latency is now a BUILD CONST — no `OBS_GENLOCK_LATENCY_MS` /
/// `OBS_GENLOCK_RESERVE_MS` env any more. Default AND floor are 3 ms (the validated
/// zero-loss held latency). Mirrors the C `#define GENLOCK_LATENCY_MS_DEFAULT 3`. The
/// legacy [`resolve_latency_ms`] / [`parse_reserve_ms`] strtol parsers are kept as pure
/// helpers (still unit-tested) but the C side no longer feeds them from env.
pub const GENLOCK_LATENCY_MS_DEFAULT: u32 = 3;

/// (#257) Hard FLOOR for the per-source genlock latency (ms) — the OBS UI min and the
/// `obs_source_set_genlock_latency_ms` setter clamp both pin it (1 → 3, 0 → 3). Mirrors
/// the C `#define GENLOCK_LATENCY_MS_MIN 3`. There is no "0 = follow global" any more;
/// 3 ms is the minimum held latency.
pub const GENLOCK_LATENCY_MS_MIN: u32 = 3;

/// (#235) Hard cap on the canonical GLOBAL genlock latency (ms) — the SAME ceiling as
/// the aliased reserve ([`GENLOCK_RESERVE_MS_MAX`] = 100 ms ≈ 3 frames @ 30 fps). This
/// is the jitter-reserve scale (the global env default each source falls back to). The
/// PER-SOURCE override ([`GENLOCK_SOURCE_LATENCY_MS_MAX`]) has a much higher ceiling —
/// it is a deliberate per-source VIDEO DELAY, not a sub-frame reserve.
pub const GENLOCK_LATENCY_MS_MAX: u32 = GENLOCK_RESERVE_MS_MAX;

/// (#245) Hard cap on the PER-SOURCE genlock latency override (ms), set in the OBS
/// source UI. Unlike the global env knob ([`GENLOCK_LATENCY_MS_MAX`] = 100 ms — a
/// sub-frame jitter reserve), a per-source override is a deliberate VIDEO DELAY: the
/// operator delays ONE source by up to ~2 s to align it against another (the #245
/// live-event need was 1000 ms on a single stream source while the others stayed low).
/// 2000 ms ≈ 60 frames @ 30 fps, comfortably inside the FIFO drop-cap's absolute
/// maximum ([`GENLOCK_PRELOAD_MAX`] + [`GENLOCK_DROP_CAP_RESERVE`] = 132 frames) at the
/// pinned 30 fps output, so a source at the cap can still buffer its full delay without
/// an overrun force-drain. Mirrored in the C side (`#define
/// GENLOCK_SOURCE_LATENCY_MS_MAX 2000`) and the DistroAV UI int range.
pub const GENLOCK_SOURCE_LATENCY_MS_MAX: u32 = 2000;

/// (#235) The minimum auto-derived internal FIFO depth a genlock source holds for
/// jitter/dropout resilience, now that `preload` is internal (no longer a user latency
/// knob). At least 1 frame is buffered so a single-tick arrival dip never empties the
/// queue (the #110 sweep showed depth >= 1 is needed to hold 0-loss). This depth is
/// LATENCY-FREE: under the ms deadline ([`genlock_present_ts_reserve`]) the held delay
/// is governed by `latency_ms`, not by how many frames sit in the FIFO — the buffer
/// only smooths arrival jitter, it does not add latency. Equals the historical default
/// preload ([`GENLOCK_PRELOAD_DEFAULT`] = 1), so the validated prod behavior is preserved.
pub const GENLOCK_AUTO_PRELOAD_MIN: u32 = GENLOCK_PRELOAD_DEFAULT;

/// (#292) The maximum frame rate any genlock SOURCE feeds at on this rig — the cameras
/// and strih render 60 fps. The genlock ts-align deadline ([`genlock_present_ts_reserve`])
/// holds every queued frame younger than `latency_ms`, so the FIFO fills at the SOURCE's
/// ARRIVAL rate, which can EXCEED the canvas OUTPUT rate (the stream box receives a 60 fps
/// NDI feed from strih into a 30 fps canvas — the "60→30 strih→stream" topology). Budgeting
/// the FIFO drop-cap at the canvas rate undercounted the held depth ~2x, so a deep
/// per-source latency force-drained at ~450 ms on the 30 fps stream box (#292). The
/// drop-cap depth ([`genlock_latency_depth_frames`]) is budgeted at this worst-case arrival
/// rate so the configured latency is DELIVERED regardless of canvas fps.
pub const GENLOCK_MAX_SOURCE_FPS: u32 = 60;

/// (#235) Resolve the canonical genlock latency (ms) from the new knob + the
/// back-compat alias.
///
/// `latency_env` = `OBS_GENLOCK_LATENCY_MS` (the canonical knob); `reserve_env` =
/// `OBS_GENLOCK_RESERVE_MS` (the deprecated alias). Resolution:
///
/// 1. If `OBS_GENLOCK_LATENCY_MS` is SET (a valid `strtol` integer, incl. an explicit
///    `0`), it is THE latency — it wins over the alias.
/// 2. Otherwise fall through to `OBS_GENLOCK_RESERVE_MS` (the alias) — so existing
///    deploys/scripts/the #128 wrapper keep working and prod `reserve=3` ⇒ `3`.
/// 3. Otherwise [`GENLOCK_LATENCY_MS_DEFAULT`] (`0` = disabled, whole-frame fallback).
///
/// "Set" for the canonical knob means it PARSES to a value (the same `strtol` contract
/// as [`parse_reserve_ms`]): an empty/whitespace/junk/negative `OBS_GENLOCK_LATENCY_MS`
/// is treated as UNSET and falls through to the alias, NOT silently as `0` — otherwise a
/// typo in the new knob would surprise-disable a working aliased deploy. A canonical
/// value out of range is clamped to [`GENLOCK_LATENCY_MS_MAX`]; the alias is clamped on
/// the same scale by [`parse_reserve_ms`].
pub fn resolve_latency_ms(latency_env: Option<&str>, reserve_env: Option<&str>) -> u32 {
    if let Some(ms) = parse_latency_ms_set(latency_env) {
        // The canonical knob is set (incl. explicit 0) — it owns the value.
        return ms;
    }
    // Fall through to the back-compat alias OBS_GENLOCK_RESERVE_MS.
    parse_reserve_ms(reserve_env)
}

/// (#245) The EFFECTIVE genlock latency (ms) for a single source: the source's OWN
/// per-source override when set (`> 0`), else the global default (from
/// [`resolve_latency_ms`]).
///
/// Mirrors the C release-deadline gate in `obs-source.c` `ready_async_frame`:
/// ```c
/// reserve_ms = source->genlock_latency_ms > 0 ? source->genlock_latency_ms
///                                             : genlock_reserve_ms();
/// ```
/// so each NDI source can hold a DIFFERENT latency (the #245 per-source ask) while a
/// source left at `0` follows the single global default — which may itself be `0` (the
/// whole-frame preload path). #235 collapsed latency to ONE global env knob and lost
/// per-source control (the live-event regression); #245 restores it WITHOUT the
/// confusing dual env knobs: the override lives on the source, set from the OBS UI.
pub fn effective_latency_ms(source_latency_ms: u32, global_latency_ms: u32) -> u32 {
    if source_latency_ms > 0 {
        source_latency_ms
    } else {
        global_latency_ms
    }
}

/// (#235) Parse `OBS_GENLOCK_LATENCY_MS` into `Some(ms)` only when it is genuinely SET
/// to a valid value (the `strtol` contract of [`parse_reserve_ms`]), else `None` so
/// [`resolve_latency_ms`] can fall through to the alias.
///
/// Distinct from [`parse_reserve_ms`]'s "invalid ⇒ default 0": here invalid ⇒ `None`
/// (unset), because the canonical knob must NOT mask a working alias on a typo. A valid
/// non-negative integer (incl. `0`) ⇒ `Some(clamped)`.
fn parse_latency_ms_set(env: Option<&str>) -> Option<u32> {
    let raw = env?;
    let body = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = body.strip_prefix('+').unwrap_or(body);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None; // unset / junk / negative ⇒ fall through to the alias
    }
    Some(match digits.parse::<i64>() {
        Ok(v) => v.min(GENLOCK_LATENCY_MS_MAX as i64) as u32,
        // Positive overflow only (non-empty all-digit body, no sign) ⇒ strtol saturates
        // to LONG_MAX ⇒ the `> MAX` clamp ⇒ MAX.
        Err(_) => GENLOCK_LATENCY_MS_MAX,
    })
}

/// (#235) The auto-derived INTERNAL FIFO depth for a genlock source, given the resolved
/// latency in ms.
///
/// `preload` is no longer a user latency knob — the ms deadline holds the latency, the
/// FIFO is just a jitter/dropout buffer. So the depth is held at a fixed minimum
/// ([`GENLOCK_AUTO_PRELOAD_MIN`], >= 1 frame) regardless of `latency_ms`: a deeper FIFO
/// would NOT add latency under the ms deadline (the deadline picks the frame aged
/// `latency_ms`, independent of how many frames are queued), but it WOULD waste memory
/// (stream.lan is RAM-tight, #89). One frame of buffer is enough to absorb a single-tick
/// arrival dip and hold the #110 0-loss floor. `latency_ms` is accepted (and ignored)
/// so the signature can grow a latency-dependent depth later without a call-site change.
pub fn genlock_auto_preload(latency_ms: u32) -> u32 {
    let _ = latency_ms;
    GENLOCK_AUTO_PRELOAD_MIN
}

/// (#235) Convert a latency in milliseconds into the equivalent WHOLE-FRAME count at a
/// given output frame rate — the inverse of [`preload_to_ms`], for the "N ms (≈ M
/// frames @ Ffps)" display.
///
/// `frames = round(ms * fps_num / (1000 * fps_den))`. Rounds to nearest (so a sub-frame
/// ms like 3 ms @ 30 fps shows ≈ 0 frames — the headline that the operator no longer has
/// to count whole frames), and a whole-frame ms round-trips back to the same frame count
/// via [`preload_to_ms`]. Returns 0 when `fps_num` is 0 (no valid video info yet) — the
/// caller shows an "fps unknown" label rather than dividing by zero. Uses `u64`
/// intermediates so `ms * fps_num` cannot overflow at the ms cap.
pub fn ms_to_frames(ms: u32, fps_num: u32, fps_den: u32) -> u32 {
    if fps_num == 0 || fps_den == 0 {
        return 0;
    }
    let num = (ms as u64) * (fps_num as u64);
    let den = 1000u64 * (fps_den as u64);
    // round-to-nearest: (num + den/2) / den.
    ((num + den / 2) / den) as u32
}

/// (#235) Format the single-knob genlock latency label "genlock latency = N ms (≈ M
/// frames @ Ffps)" — MS PRIMARY, the whole-frame equivalent in PARENTHESES (the #235
/// display ask). Mirrored by the C audit-log line and the DistroAV slider/label.
///
/// `fps_num == 0` (no valid video info yet) ⇒ "N ms (≈ ? frames — fps unknown)" so the
/// ms is still shown and the caller never divides by zero. The exact wording is unit-
/// tested; the C/cpp sides produce the same shape (ms first, frames parenthesized).
pub fn format_latency_label(ms: u32, fps_num: u32, fps_den: u32) -> String {
    if fps_num == 0 || fps_den == 0 {
        return format!("genlock latency = {ms} ms (≈ ? frames — fps unknown)");
    }
    let frames = ms_to_frames(ms, fps_num, fps_den);
    let fps = fps_num as f64 / fps_den as f64;
    format!("genlock latency = {ms} ms (≈ {frames} frames @ {fps:.3}fps)")
}

/// Parse the `OBS_GENLOCK_PRELOAD_FRAMES` env value into a reserve depth.
///
/// This is a FAITHFUL mirror of the C `genlock_parse_preload()`, which uses
/// `strtol(env, &end, 10)` and then `if (end == env || *end != '\0' || v < 0)
/// return default; if (v > MAX) return MAX;`. To match it exactly (the test crate
/// exists to prove the C contract), it replicates `strtol`'s quirks rather than
/// using Rust's `parse`, which differs on two pathological inputs:
/// * `strtol` skips only *leading* whitespace; a trailing non-digit (e.g. `"5 "`)
///   leaves `*end != '\0'` ⇒ default. (Rust `trim()` would have accepted `"5 "`.)
/// * `strtol` *saturates* an out-of-range magnitude to `LONG_MAX`, which then
///   passes the `v >= 0` guard and hits the `v > MAX` clamp ⇒ MAX. (Rust
///   `parse::<i64>()` would `Err` on overflow and fall to default.)
///
/// Net contract: `None`/empty/leading-junk/trailing-junk/negative ⇒ default;
/// any in-range or overflowing non-negative integer ⇒ clamped to
/// [`GENLOCK_PRELOAD_MAX`]; `0` is valid (reproduces the old zero-slack FIFO).
pub fn parse_preload(env: Option<&str>) -> u32 {
    let Some(raw) = env else {
        return GENLOCK_PRELOAD_DEFAULT;
    };
    // strtol skips leading ASCII whitespace, then reads an optional sign + digits;
    // a trailing non-digit leaves `*end != '\0'`. `trim_start` + an all-ASCII-digit
    // body (after an optional leading '+') reproduces that without a hand-rolled
    // arithmetic loop. `i64::from_str` does the accumulation, and its
    // `PosOverflow` error is the strtol LONG_MAX-saturation case ⇒ clamp to MAX.
    let body = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = body.strip_prefix('+').unwrap_or(body);
    // Reject empty / leading-sign-only / any non-digit char (incl. trailing junk
    // and a leading '-', so every negative falls to default like the C `v < 0`).
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return GENLOCK_PRELOAD_DEFAULT;
    }
    match digits.parse::<i64>() {
        Ok(v) => v.min(GENLOCK_PRELOAD_MAX as i64) as u32,
        // `digits` is non-empty and all-ASCII-digit (no sign), so the ONLY
        // reachable parse error is positive overflow — strtol saturates that to
        // LONG_MAX, which then hits the `> MAX` clamp ⇒ MAX. No other error kind
        // can occur, so there is no separate default-on-error arm (a guarded arm
        // here would leave an equivalent, untestable mutant).
        Err(_) => GENLOCK_PRELOAD_MAX,
    }
}

/// (#184) Parse the `OBS_GENLOCK_RESERVE_MS` env value into a sub-frame jitter
/// reserve in milliseconds.
///
/// A FAITHFUL mirror of the C `genlock_parse_reserve_ms()` (same `strtol` quirks as
/// [`parse_preload`]): `None`/empty/leading-junk/trailing-junk/negative ⇒
/// [`GENLOCK_RESERVE_MS_DEFAULT`] (`0` = disabled, whole-frame `preload` path);
/// any in-range or overflowing non-negative integer ⇒ clamped to
/// [`GENLOCK_RESERVE_MS_MAX`]. `0` is a valid explicit value (disabled), distinct
/// only in intent from the unset default — both yield `0`.
pub fn parse_reserve_ms(env: Option<&str>) -> u32 {
    let Some(raw) = env else {
        return GENLOCK_RESERVE_MS_DEFAULT;
    };
    let body = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = body.strip_prefix('+').unwrap_or(body);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return GENLOCK_RESERVE_MS_DEFAULT;
    }
    match digits.parse::<i64>() {
        Ok(v) => v.min(GENLOCK_RESERVE_MS_MAX as i64) as u32,
        // Only positive overflow is reachable here (non-empty all-digit body, no
        // sign) — strtol saturates to LONG_MAX ⇒ the `> MAX` clamp ⇒ MAX.
        Err(_) => GENLOCK_RESERVE_MS_MAX,
    }
}

/// The genlock consume decision for one render tick (#102).
///
/// `consume` — hand a distinct queued frame to the compositor this tick.
/// `filled`  — the new value of the source's one-time startup-fill latch (the
///             delay line has reached its `preload` depth at least once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenlockDecision {
    pub consume: bool,
    pub filled: bool,
}

/// At a render tick, decide whether the genlock FIFO consumes a distinct frame
/// (#102 — the frame-loss fix).
///
/// The #70 gate held consumption until `queue_depth > preload` on **every** tick,
/// so any NDI arrival-jitter dip below the reserve REPEATED the last frame and lost
/// one DISTINCT frame. At a deep #97 preload (≈1 s) it was catastrophic: after any
/// drain the FIFO had to refill PAST the whole reserve before a single new frame
/// escaped (11.6 % @ preload=1 → 34.3 % @ preload=30, underrun-dominated on the live
/// stream box).
///
/// The fix splits the FIFO's life into two phases via a one-time `filled` latch:
///
/// 1. **Build (`filled == false`)** — establish the delay line. Hold (no consume)
///    until `queue_depth > preload`, i.e. `preload` frames are buffered plus one to
///    emit; then **latch `filled` and consume** the first (now `preload`-frames-late)
///    frame. This startup fill is the ONLY place repeats are emitted, and only once.
///
/// 2. **Steady (`filled == true`)** — **consume a distinct frame on EVERY tick a
///    frame is queued** (`queue_depth >= 1`). A jitter dip below the reserve still
///    delivers a distinct frame (the reserve shrinks and refills naturally) — it is
///    never repeated. The ONLY hold is a TRUE empty (`queue_depth == 0`): a genuine
///    underrun with no frame to deliver. `filled` stays `true` across a transient
///    empty, so a momentary dip does NOT re-trigger the whole startup refill run.
///
/// Result: a deep preload is a CLEAN delay line — it holds the delay but emits a
/// distinct frame every tick one is queued ⇒ ~0 distinct-frame loss at ANY depth.
/// Mirrors the C `genlock_decide()` in the vendored OBS genlock branch. The latch is
/// reset (back to `filled == false`) only on an overrun force-drain (`cache_video`
/// empties the FIFO), so the delay rebuilds after a drain.
pub fn genlock_decide(queue_depth: usize, preload: u32, filled: bool) -> GenlockDecision {
    if !filled {
        // Build phase: fill to the preload delay depth before emitting anything.
        if queue_depth > preload as usize {
            GenlockDecision {
                consume: true,
                filled: true,
            }
        } else {
            GenlockDecision {
                consume: false,
                filled: false,
            }
        }
    } else {
        // Steady phase: consume whenever a distinct frame is queued; hold only on
        // a true empty (an unavoidable underrun, never a repeat-while-queued).
        GenlockDecision {
            consume: queue_depth >= 1,
            filled: true,
        }
    }
}

/// The peak steady-state queue depth seen at the consume DECISION instant when
/// producer and consumer run at the same rate.
///
/// #102: the FIFO builds to `preload + 1` (the latch fires when depth first exceeds
/// `preload`), then each tick the producer adds one (depth → `preload + 1`) and the
/// gate consumes one (depth → `preload`). So the depth oscillates `preload + 1` (at the
/// decision, before consuming) ↔ `preload` (after) — leaving `preload` frames of reserve
/// at the instant of consumption, the SAME single-tick jitter tolerance the #70 gate
/// gave (it takes `preload + 1` consecutive missed deliveries to reach a true empty).
/// The drop-cap must clear this `preload + 1` peak, which is what
/// [`genlock_drop_cap`]'s `+ RESERVE` guarantees.
pub fn steady_state_depth(preload: u32) -> u32 {
    preload + 1
}

/// How many OLDEST frames to ERASE at the build-latch instant so the FIFO settles
/// at exactly the target depth, regardless of the NDI startup burst (#116).
///
/// ## The bug this fixes
///
/// The #102 build latch ([`genlock_decide`]) latches `filled = true` the instant
/// `queue_depth > preload`, at WHATEVER depth the NDI startup burst left in the
/// queue, and consumes exactly one frame — it never trims down. So:
///
/// 1. Two inputs with the SAME preload but DIFFERENT startup bursts freeze at
///    different depths ⇒ different per-camera latency ⇒ a time-jump when switching
///    cameras (live: NDI cam5(=CAM1) latched depth 6 vs cam1/cam3 depth 2 at an
///    identical preload=1, a ~133 ms spread).
/// 2. A preload DECREASE re-arms the latch ([`obs_source_set_genlock_preload`]),
///    but the deep queue re-latches `filled` immediately at the OLD deep depth, so
///    the lower delay never takes effect ("preload only goes up").
/// 3. Each restart's random NDI arrival phase gives a different frozen depth ⇒
///    non-deterministic latency after an OBS restart.
///
/// ## The fix
///
/// When the build latch is about to fire (`queue_depth > preload`), erase the
/// `queue_depth - target` OLDEST frames so the FIFO holds exactly `target =
/// steady_state_depth(preload) = preload + 1` frames, then the same-tick consume
/// leaves `preload` (the steady-state reserve). Every input — and every restart —
/// then settles at the IDENTICAL deterministic depth, and a preload change takes
/// effect immediately in BOTH directions (a decrease drains to the lower target on
/// the next build latch; an increase rebuilds up to the higher target). The C
/// `ready_async_frame` calls this at the build latch and erases that many oldest
/// frames via the `da_erase(async_frames, 0)` + `remove_async_frame()` idiom (the
/// same per-frame free path the `async_unbuffered` drain uses — no leak, no
/// double-free).
///
/// The drain fires ONLY at the build latch (and on a preload-change re-arm) — NEVER
/// in steady state. The #102 consume-when-queued 0-loss gate ([`genlock_decide`]'s
/// steady branch) is untouched. Below the latch (`queue_depth <= preload`, still
/// building) and at/under the target there is nothing to trim ⇒ 0 (saturating, so
/// it can never underflow/wrap).
pub fn genlock_build_drain(queue_depth: usize, preload: u32) -> usize {
    let target = steady_state_depth(preload) as usize;
    queue_depth.saturating_sub(target)
}

/// Re-arm threshold: the number of CONSECUTIVE true-empty (underrun) render ticks a
/// genlock FIFO must see in steady state before a resume counts as a reconnect and
/// re-arms the build latch (#126).
///
/// Chosen at **30 ticks ≈ 1 s @ 30 fps** — deliberately FAR above any realistic
/// arrival-jitter dip. The #102 reserve already makes a single-tick (or even a few
/// consecutive) empties impossible without a real outage: in steady state the queue
/// parks at `preload` after each consume, so it takes `preload + 1` consecutive missed
/// deliveries even to REACH a true empty, and only a genuine upstream disconnect (the
/// strih OBS restart this fixes) sustains true-empties for ~1 s. A LOWER threshold
/// would risk a spurious re-arm at the shallow cam `preload = 1`, where a 1–2 tick
/// transient empty must NOT re-arm — a spurious re-arm forces a ~`preload`-frame
/// rebuild HOLD on every blip. The cost of the chosen value is bounded and acceptable:
/// recovery after a real reconnect is ~1 s (the detection window) plus one rebuild hold
/// (~`preload + 1` frames) — versus the silent video-delay collapse (A/V drift) the bug
/// causes until a manual nudge. Mirrors the C `#define GENLOCK_REARM_EMPTY_TICKS`.
pub const GENLOCK_REARM_EMPTY_TICKS: u32 = 30;

/// Advance the per-source consecutive-true-empty counter for one render tick (#126).
///
/// `consumed` — whether a frame was QUEUED (`num >= 1`) this tick. The counter is only
/// ever nonzero in steady state, where the #102 invariant guarantees a queued frame is
/// always consumed (`queue_depth >= 1` ⇒ consume) — so "a frame was queued again" and
/// "a frame was consumed" coincide for every tick that can reset a nonzero counter, and
/// modelling the reset as `consumed=true` is exact. (The C side resets at the same
/// instant: on entry to the genlock branch of `ready_async_frame`, which is reached only
/// with `num >= 1`.)
/// * a queued/consumed tick **resets** the run to 0 (so a flickering empty/non-empty
///   queue — normal jitter — can NEVER accumulate to [`GENLOCK_REARM_EMPTY_TICKS`]; only
///   a genuine sustained disconnect can);
/// * a true empty **increments** the run (saturating, so a very long outage never wraps).
///
/// Mirrors the C `source->genlock_empty_run` bookkeeping in `obs-source.c`
/// (`++` at the `get_closest_frame` num==0 underrun site, `= 0` on the next tick a frame
/// is queued — i.e. on entry to the `ready_async_frame` genlock branch).
pub fn genlock_empty_run_next(empty_run: u32, consumed: bool) -> u32 {
    if consumed {
        0
    } else {
        empty_run.saturating_add(1)
    }
}

/// Decide whether a resuming genlock FIFO should RE-ARM its build latch (#126).
///
/// On an upstream OBS (strih) restart the downstream NDI source underruns to EMPTY, but
/// DistroAV's default `KEEP_CONTENT` blocks the only NULL-emit reset, and an underrun
/// (not an overrun) never fires the `cache_video` force-drain reset — so `genlock_filled`
/// stays **true**. The #102 steady branch then consumes 1/tick the instant the queue
/// refills WITHOUT rebuilding the preload reserve, so the deliberate ~26-frame video
/// delay silently collapses to ~0 (A/V drift) until a manual preload nudge.
///
/// The fix: when frames RESUME after a SUSTAINED true-empty run, re-arm `genlock_filled`
/// to `false` so the EXISTING #102 build path + #116 drain rebuild the reserve to exactly
/// `preload + 1` — no manual nudge, NO new draining logic.
///
/// Returns `true` (re-arm) only when BOTH:
/// * the source is in STEADY state (`filled == true`) — while building, the reserve is
///   already being rebuilt, so re-arming is meaningless; AND
/// * the consecutive-empty run is at/above [`GENLOCK_REARM_EMPTY_TICKS`] — the
///   jitter-safety guard that keeps a brief (sub-threshold) dip from spuriously
///   re-arming (which would force a ~`preload`-frame hold on every blip, catastrophic at
///   the shallow cam `preload = 1`).
///
/// Mirrors the C `genlock_empty_run >= GENLOCK_REARM_EMPTY_TICKS && source->genlock_filled`
/// guard taken on the resume tick in `ready_async_frame`.
pub fn genlock_rearm_on_resume(empty_run: u32, filled: bool) -> bool {
    filled && empty_run >= GENLOCK_REARM_EMPTY_TICKS
}

/// Convert a preload depth (frames of genlock-disciplined delay) into the
/// equivalent **delay in milliseconds** at a given output frame rate.
///
/// `ms = frames * 1000 * fps_den / fps_num` (e.g. 30 frames @ 30000/1001 ≈ 1001 ms;
/// 30 frames @ 30/1 = 1000 ms). This is the EXACT integer arithmetic the C/C++ side
/// performs (`obs_get_video_info()` → `fps_num`/`fps_den`), mirrored here so the GUI
/// info-text label and the audit-log ms are unit-tested. Uses `u64` intermediates so
/// `frames * 1000 * fps_den` cannot overflow at the cap (128 * 1000 * fps_den).
///
/// Returns 0 if `fps_num` is 0 (no valid video info yet) — the caller shows a
/// "fps unknown" label rather than dividing by zero.
pub fn preload_to_ms(frames: u32, fps_num: u32, fps_den: u32) -> u64 {
    if fps_num == 0 {
        return 0;
    }
    (frames as u64) * 1000 * (fps_den as u64) / (fps_num as u64)
}

/// The per-source async-FIFO drop-cap (#97).
///
/// libobs force-drains a source's async FIFO when it reaches the drop-cap (an
/// overrun). For a NON-genlock source the cap stays the fixed [`MAX_ASYNC_FRAMES`]
/// (30) — those sources never deliberately buffer, so a deep cap would only mask a
/// runaway producer. For a **genlock** source the cap =
/// `max(MAX_ASYNC_FRAMES, preload + RESERVE)`, clamped to an absolute maximum of
/// `GENLOCK_PRELOAD_MAX + RESERVE` (= 132).
///
/// The [`MAX_ASYNC_FRAMES`] floor is load-bearing: before #97 every source (genlock
/// included) had the fixed 30-frame cap, which absorbed NDI catch-up bursts after a
/// LAN hiccup. Scaling the cap to `preload + RESERVE` *alone* would, at the
/// production default `preload = 1`, drop the cap to 5 — a 6× cut in burst tolerance
/// on exactly the jittery sources the genlock FIFO exists to protect, so a momentary
/// stall delivering a 5-frame catch-up burst would force-drain the whole buffer.
/// Keeping the 30-frame floor preserves the pre-#97 burst tolerance; the cap only
/// GROWS above it once the operator dials in a deep delay (memory stays bounded —
/// only a deliberately-delayed source holds a big buffer, stream.lan is RAM-tight #89).
pub fn genlock_drop_cap(genlock_fifo: bool, preload: u32) -> u32 {
    if !genlock_fifo {
        return MAX_ASYNC_FRAMES;
    }
    let want = preload.saturating_add(GENLOCK_DROP_CAP_RESERVE);
    let abs_max = GENLOCK_PRELOAD_MAX + GENLOCK_DROP_CAP_RESERVE;
    want.min(abs_max).max(MAX_ASYNC_FRAMES)
}

/// (#292) The FIFO depth (frames) a genlock source must be able to hold to DELIVER
/// `latency_ms` of video delay — the value fed into [`genlock_drop_cap`] as the source's
/// effective `preload` budget.
///
/// The ts-align release deadline ([`genlock_present_ts_reserve`]) holds every queued frame
/// younger than `latency_ms`, so the FIFO parks `latency_ms`-worth of frames AT THE SOURCE
/// ARRIVAL RATE. That rate can EXCEED the canvas OUTPUT rate: the stream box receives a
/// 60 fps NDI feed from strih into a 30 fps canvas (the "60→30 strih→stream" topology), so
/// 1000 ms of delay parks ≈ 60 frames, NOT the 30 the canvas rate implies. Budgeting the
/// drop-cap at the canvas fps undercounted the held depth ~2x, so the overrun force-drain
/// capped a deep latency at ~450 ms — the operator could not delay the stream the ~1 s
/// needed to align to the late mastered audio (#292).
///
/// The depth is therefore budgeted at the WORST-CASE arrival rate
/// ([`GENLOCK_MAX_SOURCE_FPS`]) — and the canvas rate too, should a future canvas ever run
/// faster — flooring at [`GENLOCK_AUTO_PRELOAD_MIN`]. Mirror of the C
/// `genlock_source_drop_cap` depth budget in `vendor/obs-studio/libobs/obs-source.c`.
pub fn genlock_latency_depth_frames(
    latency_ms: u32,
    canvas_fps_num: u32,
    canvas_fps_den: u32,
) -> u32 {
    // The buffer fills at the SOURCE arrival rate (#292): budget at the worst-case
    // GENLOCK_MAX_SOURCE_FPS so a 60 fps feed into a 30 fps canvas still holds the full
    // delay. Honour the canvas rate too, should a future canvas ever run faster than the
    // source. Floor at the resilience minimum so a sub-frame (3 ms) latency keeps >= 1.
    let arrival_frames = ms_to_frames(latency_ms, GENLOCK_MAX_SOURCE_FPS, 1);
    let canvas_frames = ms_to_frames(latency_ms, canvas_fps_num, canvas_fps_den);
    arrival_frames
        .max(canvas_frames)
        .max(GENLOCK_AUTO_PRELOAD_MIN)
}

// ---- #136: timestamp-aligned release (multi-source IN-SYNC) ----------------
//
// The count-based gate above ([`genlock_decide`]) keeps a per-source jitter buffer of
// a fixed DEPTH and consumes one frame per render tick. That cannot hold MULTIPLE
// sources in sync: each source's depth drifts independently (the render pass consumes
// slightly slower than the cameras produce, and any per-source dropout/reconnect/
// preload-change leaves that source at a different depth that never re-converges), so
// camera A ends up N frames behind camera B — visible desync (measured ~300 ms / 9
// frames spread live, #136). A depth/count buffer fundamentally only chooses WHERE the
// rate difference accumulates; it cannot eliminate it.
//
// Timestamp-aligned release fixes it. Every camera-box frame carries its real
// DanteSync wall-clock CAPTURE instant (src/ndi.rs stamps the NDI timecode; DistroAV
// passes it into `obs_source_frame->timestamp` in Source-Timecode mode). The strih
// render tick is ALSO on the shared DanteSync wall clock. So at each tick we present,
// from every source, the frame captured at the SAME instant `present_ts = tick_wall -
// COMMON_DELAY`. Identical capture instant on every source ⇒ **in-sync by
// construction**, regardless of buffered depth, delivery jitter, or a transient.
// Latency is exactly `COMMON_DELAY` (bounded + uniform, never drifting), and a slow/
// lagged render pass just drops the stale past-due frames uniformly instead of filling
// the buffer toward the overrun cap. `preload` (a frame COUNT) is reinterpreted as a
// TIME delay via the wall clock — same operator knob, sync-correct semantics.

/// A timestamp-aligned genlock RELEASE decision for one source at one render tick (#136).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenlockRelease {
    /// Free this many OLDEST queued frames UNSHOWN — they are already past the
    /// presentation deadline (stale). A lagged render pass makes this > 0; at matched
    /// rates it is 0. This replaces the unbounded FIFO fill of the count gate.
    pub drop_oldest: usize,
    /// Present a newly-due frame this tick (the head after `drop_oldest`). If `false`,
    /// no queued frame has reached its presentation deadline yet (source early or
    /// stalled) → repeat the current frame, drop nothing.
    pub present: bool,
}

/// The presentation deadline (capture instant due NOW) for a render tick.
///
/// `present_ts = tick_wall_ns - delay_frames * interval_ns` (saturating, so an early
/// boot wall clock can never wrap below 0). `delay_frames` is the genlock `preload`
/// reinterpreted as a COMMON delay shared by every source — the single knob that sets
/// the uniform end-to-end latency and the jitter headroom (it must cover worst-case
/// capture→strih delivery, like `preload` did, but now as a true time budget).
pub fn genlock_present_ts(tick_wall_ns: u64, delay_frames: u32, interval_ns: u64) -> u64 {
    // Half-interval tolerance (#136 boundary-churn fix). A frame whose capture
    // timestamp lands exactly on the nominal deadline (wall - delay*interval) jitters
    // in/out of the `ts <= present_ts` test from tick to tick — the wall-clock render
    // tick has ±slew, so present_ts hovers around the frame's timestamp and the frame
    // alternates due/not-due, producing hold-then-catch-up churn (measured ~3 fps on
    // the deep-preload chained strih->stream PGM feed: every boundary-aligned frame
    // landed on the deadline). Biasing the deadline FORWARD by half a frame makes a
    // boundary-landing frame robustly due (the ±2 ms slew is far below interval/2 ≈
    // 16 ms @ 30 fps), giving a clean one-frame-per-tick release. Every source shares
    // the same bias, so multi-source alignment is preserved; effective latency just
    // drops by interval/2 (~16 ms, negligible).
    tick_wall_ns
        .saturating_sub((delay_frames as u64) * interval_ns)
        .saturating_add(interval_ns / 2)
}

/// (#184) The presentation deadline for a render tick under a MS-GRANULAR jitter
/// reserve — the sub-frame lowest-latency lever for zero-latency IMAG on LED walls.
///
/// `present_ts = tick_wall_ns - reserve_ms*1_000_000` (saturating, so an early-boot
/// wall clock can never wrap below 0). A frame is presented once its capture
/// timestamp is at/before this deadline — i.e. once it has aged at least
/// `reserve_ms` — so the held latency is EXACTLY `reserve_ms`, a pure time delay
/// that is NOT quantized to a whole frame.
///
/// ## Why this replaces the whole-frame preload (the #184 thesis)
///
/// The frame-based [`genlock_present_ts`] subtracts `preload * interval` — at
/// `preload = 1`, that is a fixed **33.3 ms** floor @ 30 fps. But the buffer only
/// has to absorb the per-input ARRIVAL JITTER, which the live rig measures at
/// **1.6 ms** (strih→stream) and **8.1 ms** (cam1→strih) — a few ms, NOT a whole
/// frame. Setting `reserve_ms ≈ measured_jitter + a small margin` keeps overruns/
/// underruns at 0 while cutting the held delay from 33 ms to single-digit ms.
///
/// ## No +interval/2 churn bias (deliberate)
///
/// The frame-based path adds `+interval/2` (the #136 boundary-churn guard) because a
/// frame can land EXACTLY on a frame-quantized deadline and jitter in/out under
/// render-tick slew. The reserve deadline is an ABSOLUTE wall-clock instant, not a
/// frame multiple, so frames do not cluster on it — there is no boundary to churn
/// across, and `reserve_ms` is itself the slew tolerance (chosen ≥ jitter + margin).
/// Adding `+interval/2` here would silently re-inflate the latency by ~16 ms,
/// defeating the whole point. Every source shares the SAME `tick_wall_ns - reserve`,
/// so multi-source in-sync (the #136 invariant) is preserved by construction.
///
/// Mirror of the C `genlock_present_ts_reserve()` (guarded in
/// `tests/genlock_preload.rs`).
pub fn genlock_present_ts_reserve(tick_wall_ns: u64, reserve_ms: u32) -> u64 {
    tick_wall_ns.saturating_sub((reserve_ms as u64) * 1_000_000)
}

/// Decide the timestamp-aligned release for one source at one render tick (#136).
///
/// `present_ts_ns` — the shared presentation deadline ([`genlock_present_ts`]).
/// `queued_ts_ascending` — the source's queued frames' CAPTURE timestamps, oldest
/// first (a single NDI source delivers in monotonic capture order, so the due frames
/// are a prefix).
///
/// Rule: let `due` = the queued frames whose capture ts is at/before the deadline. If
/// none are due, HOLD (`present = false`, drop nothing) — the source is early or
/// stalled, repeat the current frame. Otherwise present the NEWEST due frame (the head
/// after dropping the `due - 1` stale older ones). Because every genlock source shares
/// `present_ts` and a common wall-clock capture cadence, the presented frame's
/// timestamp is identical across sources ⇒ in-sync.
pub fn genlock_release(present_ts_ns: u64, queued_ts_ascending: &[u64]) -> GenlockRelease {
    let due = queued_ts_ascending
        .iter()
        .take_while(|&&ts| ts <= present_ts_ns)
        .count();
    if due == 0 {
        GenlockRelease {
            drop_oldest: 0,
            present: false,
        }
    } else {
        GenlockRelease {
            drop_oldest: due - 1,
            present: true,
        }
    }
}

/// A timestamp-aligned genlock RELEASE decision WITH backward-wall-clock-step recovery
/// (#147) — the [`genlock_release`] decision plus a flag for whether THIS tick recovered
/// from a backward clock step (so the C audit can count `genlock_backward_steps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenlockReleaseGuarded {
    /// The release decision to apply (drop the stale oldest, present or hold).
    pub release: GenlockRelease,
    /// A backward DanteSync wall-clock step (NTP/PTP sawtooth correction) was DETECTED
    /// and RECOVERED this tick: the head frame was stamped in the impossible FUTURE
    /// relative to the real wall clock (captured before the step), so the unguarded path
    /// would have HELD (frozen) indefinitely; this tick re-anchored and presented a frame
    /// instead. `false` on every normal tick (a due frame, or a benign source-early hold).
    pub backward_step: bool,
}

/// Decide the timestamp-aligned release WITH backward-wall-clock-step recovery (#147).
///
/// The SINK ts-align deadline is `present_ts = wall_now - reserve` ([`genlock_present_ts`]
/// / [`genlock_present_ts_reserve`]). If the shared DanteSync wall clock steps BACKWARD (a
/// real NTP/PTP sawtooth correction on this rig — see the genlock ops notes), `wall_now`
/// regresses, so `present_ts` drops BELOW every already-queued frame's (pre-step, higher)
/// capture timestamp → [`genlock_release`] finds `due == 0` and HOLDs (repeats the last
/// frame) every tick. Because the post-step frames keep arriving stamped at the rewound
/// (lower) clock, the queue head stays a stale FUTURE-stamped frame and the hold is
/// INDEFINITE — the live program feed FREEZES until the wall clock naturally climbs back
/// (potentially seconds–minutes). This is the SINK analogue of the cam-EMIT freeze the
/// #131/#134 guard fixed (`src/genlock_pacing.rs genlock_emit_gate`: a boundary latched impossibly
/// far in the future re-latched to the rewound clock).
///
/// Recovery (mirror of #131's future-state detection, applied to the queued frames): a
/// queued frame whose capture timestamp is MORE THAN one `interval` AHEAD of the real
/// `wall_now` is impossible for a live capture (you cannot capture in the future) — it was
/// stamped BEFORE a backward clock step. When the unguarded release would HOLD (`due == 0`)
/// yet such a future-stamped frame sits at the HEAD (blocking the ascending-prefix scan),
/// RE-ANCHOR: present the NEWEST queued frame and drop the older (now-stale) ones — exactly
/// as a normal "present newest due" tick. Over the next ticks each re-anchor drops the
/// frames behind the newest, so the stale pre-step frames drain; once the post-step
/// (rewound-clock) frames are the newest, the normal ts-align prefix resumes. The result
/// is a one-/few-tick blip, NEVER an indefinite freeze.
///
/// Why a one-tick `wall_now < last_wall_now` step detector is NOT enough (and is not used):
/// the backward step is a SINGLE event, but the stale FUTURE-stamped head persists across
/// the following ticks (until it drains), so a one-shot "the clock just stepped" trigger
/// would re-freeze on the very next tick. Detecting the stale future-stamped head directly
/// — the condition that actually causes the freeze — self-heals over the seam and needs no
/// per-source state. A legitimate large per-source latency override (up to 2000 ms) buffers
/// frames that are PAST-stamped (aging toward the deadline, `ts <= wall_now`), never
/// FUTURE-stamped, so this guard never touches it.
///
/// Mirror of the C `ready_async_frame` ts-align release block (#147) — guarded in
/// `tests/genlock_preload.rs`.
pub fn genlock_release_guarded(
    wall_now_ns: u64,
    interval_ns: u64,
    present_ts_ns: u64,
    queued_ts_ascending: &[u64],
) -> GenlockReleaseGuarded {
    let release = genlock_release(present_ts_ns, queued_ts_ascending);
    if release.present {
        // A queued frame is due — normal operation, no freeze, nothing to recover.
        return GenlockReleaseGuarded {
            release,
            backward_step: false,
        };
    }
    // due == 0: the unguarded path would HOLD (repeat the last frame). Distinguish a
    // BENIGN source-early hold (frames queued, none aged to the deadline yet) from a
    // backward-clock-step FREEZE.
    //
    // #269 [3]: detect the step on the NEWEST (max-ts) queued frame, NOT the oldest. The
    // newest captured frame is ~wall_now in normal operation; one stamped MORE THAN an
    // interval AHEAD of the real wall clock is impossible for a live capture — the shared
    // DanteSync clock stepped backward. Testing the OLDEST frame instead would make the
    // trigger depend on each source's queue depth (a backward step smaller than a deep
    // source's buffer leaves its oldest frame NOT-future, so it stays frozen while a shallow
    // source jumps to live — the exact cross-source DESYNC genlock prevents). The MAX is
    // depth-independent, so all genlock sources re-anchor UNIFORMLY once the step exceeds the
    // margin. A legitimate large per-source latency override (≤2000 ms) buffers PAST-stamped
    // frames (ts ≤ wall_now, aging toward the deadline), whose max never exceeds the margin,
    // so the guard never touches it.
    //
    // #1009: the margin is the RE-QUALIFIED max(3×interval, 250 ms) from the Tier-0 authority
    // (src/genlock_backlog.rs backward_step_margin_ns) — the old ONE-interval margin sat only
    // network-delay away from the sender's deliberate ceil-to-boundary future bias and fired
    // on a few ms of inter-box skew (the 2026-08-07 overnight −900 ms hold collapse). The
    // STATEFUL parts of #1009 (sustained-tick qualification + regime self-heal + bounded
    // regime warns) live in the C and in src/genlock_backlog.rs BackwardStepGuard ONLY —
    // this stateless per-tick mirror keeps just the raw condition in lock-step (same
    // minimal-glue shape as the #940 phase-pin, which is also C+Tier-0-mirror only).
    let max_ts = queued_ts_ascending.iter().copied().max();
    let backward_step = max_ts.is_some_and(|m| {
        m > wall_now_ns.saturating_add(crate::genlock_backlog::backward_step_margin_ns(interval_ns))
    });
    if backward_step {
        // #269 [0]: RE-ANCHOR by presenting the OLDEST queued frame and dropping NOTHING
        // extra (drop_oldest = 0). The pre-step frames are real captures; the caller erases
        // the presented head each tick, so the buffer drains one frame per tick (matching the
        // consume rate) and the configured latency-depth buffer is PRESERVED — recovery is a
        // smooth few-frame blip at ANY latency. The old "present newest, drop num-1" drained
        // the queue to empty, so for a deep latency override the feed then FROZE for
        // ~latency_ms while the buffer refilled.
        GenlockReleaseGuarded {
            release: GenlockRelease {
                drop_oldest: 0,
                present: true,
            },
            backward_step: true,
        }
    } else {
        // Genuine source-early / stalled hold (queue empty, or frames recent but not yet
        // due — including a large deliberate per-source latency buffer fill) — unchanged.
        GenlockReleaseGuarded {
            release,
            backward_step: false,
        }
    }
}

/// The per-EVENT latch decision for the `genlock_backward_steps` audit counter + warning
/// log (#269 [2]). A single backward clock step recovers over MANY render ticks (the buffer
/// drains one frame/tick), so the raw [`genlock_release_guarded`] `backward_step` flag is
/// `true` for the WHOLE recovery. Counting/logging on every such tick reports one step as N
/// and spams `LOG_WARNING` at frame rate (breaking the 5 s audit-log gating). This latch
/// fires the count + log ONCE, on the transition INTO the re-anchor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenlockBackwardStepLatch {
    /// Increment `genlock_backward_steps` THIS tick (once per event, on the entry edge).
    pub count_event: bool,
    /// Emit the backward-step `LOG_WARNING` THIS tick (once per event, on the entry edge).
    pub log_event: bool,
    /// The new "currently inside a backward-step recovery" state to carry to the next tick
    /// (mirror of the C `source->genlock_in_backward_step` field).
    pub in_step: bool,
}

/// Latch the backward-step audit counter + warning log to ONCE per EVENT (#269 [2]).
///
/// `prev_in_step` — were we already inside a backward-step recovery on the previous tick
/// (the carried `genlock_in_backward_step` state)? `backward_step_this_tick` — did THIS tick
/// re-anchor ([`genlock_release_guarded`]`.backward_step`)? The count + log fire only on the
/// rising edge (`backward_step_this_tick && !prev_in_step`), so one EVENT recovered over N
/// ticks counts ONCE, not N times, and the `LOG_WARNING` fires once per event instead of at
/// frame rate. The carried `in_step` simply tracks the current re-anchor state, so the next
/// distinct event (after a normal/benign tick resets it) counts again.
///
/// Mirror of the C `ready_async_frame` ts-align re-anchor counter latch (#147 / #269 [2]) —
/// guarded in `tests/genlock_preload.rs`.
pub fn genlock_backward_step_latch(
    prev_in_step: bool,
    backward_step_this_tick: bool,
) -> GenlockBackwardStepLatch {
    // #269 [2]: fire the count + log only on the RISING edge of the re-anchor state, so one
    // event recovered over N ticks counts ONCE (not N) and the LOG_WARNING fires once per
    // event instead of at frame rate. The carried `in_step` tracks the current state; a
    // normal/benign tick resets it, so the next distinct event counts again.
    let transition_in = backward_step_this_tick && !prev_in_step;
    GenlockBackwardStepLatch {
        count_event: transition_in,
        log_event: transition_in,
        in_step: backward_step_this_tick,
    }
}

/// Lower plausible-wall-clock bound (Unix epoch ns): 2020-01-01T00:00:00Z.
pub const WALLCLOCK_TS_MIN_NS: u64 = 1_577_836_800_000_000_000;
/// Upper plausible-wall-clock bound (exclusive, Unix epoch ns): 2100-01-01T00:00:00Z.
pub const WALLCLOCK_TS_MAX_NS: u64 = 4_102_444_800_000_000_000;

/// Is a frame timestamp a plausible DanteSync wall-clock instant (Unix-epoch ns)?
///
/// Timestamp-aligned release ([`genlock_release`]) is correct ONLY when frames carry a
/// real shared wall-clock capture instant — the camera-box genlock inputs in
/// Source-Timecode mode. A `0` (no timecode), a small monotonic-style value, or any
/// out-of-range garbage (a non-camera source: CG, preview, lyrics) fails this, and the
/// C side then falls back to the count-based [`genlock_decide`] gate so non-broadcast
/// sources are never broken by the new path.
pub fn is_wallclock_ts(ts_ns: u64) -> bool {
    (WALLCLOCK_TS_MIN_NS..WALLCLOCK_TS_MAX_NS).contains(&ts_ns)
}

/// The new high-water mark of the genlock FIFO depth, given the previous peak and a depth
/// OBSERVED right now (#99 point 2).
///
/// ## Why this exists
///
/// `genlock_peak_depth` is the audit log's high-water mark of the jitter buffer — its whole
/// purpose is to tell the operator how close the queue got to the drop-cap so the preload
/// depth can be tuned. The original #70 instrumentation updated it ONLY on the CONSUMER side
/// (inside `ready_async_frame`, at the render-tick consume decision). But the PRODUCER (NDI
/// arrival → `obs_source_output_video_internal` → `da_push_back(async_frames)`) can push the
/// queue to a momentary high depth BETWEEN two render ticks and have it drained back down
/// before the next tick observes it — so the consumer-side-only peak UNDER-reports the true
/// high-water mark. The fix (#99) is to also fold the depth the producer reaches into the
/// peak, at the push site, under the same `async_mutex`.
///
/// This is the camera-box-side REFERENCE for that "peak = max so far" rule — the same mirror
/// pattern as [`genlock_decide`] / [`steady_state_depth`]: the C does the update INLINE at both
/// sites (`if (depth > genlock_peak_depth) genlock_peak_depth = depth;` — the render path can't
/// call into Rust), and the `tests/genlock_preload.rs` vendored-source guard asserts that inline
/// update exists on BOTH the producer and consumer paths so an upstream subtree merge can't drop
/// it. This pure fn pins the rule itself (a monotone non-decreasing max) under unit test, so the
/// reference the C is checked against is itself provably correct.
///
/// Pure `max`; saturating is unnecessary (a `u32` max of two `u32`s cannot overflow). The
/// invariant: the return is never below `current_peak`.
pub fn genlock_peak_update(current_peak: u32, observed_depth: u32) -> u32 {
    current_peak.max(observed_depth)
}

/// (#200) Accept an output-fps snapshot only when two back-to-back reads AGREE — the
/// value-seqlock the C `genlock_video_fps()` helper uses to avoid a TORN
/// `(fps_num, fps_den)` pair on the unlocked genlock audit/preload path.
///
/// `obs_get_video_info()` (obs.c) copies the global output `ovi` struct WITHOUT a
/// lock, so a concurrent `obs_reset_video()` (a resolution/fps change) can interleave
/// between the num and den field copies and return a mismatched pair, formatting a
/// wrong ms in the audit log. Taking the OBS video graphics lock on the render/audit
/// path risks a lock-ordering deadlock vs `obs_reset_video` (which holds the video lock
/// while it can touch source state), for a log-only gain — so the C side instead reads
/// the pair twice and accepts it only when both reads match (`obs_reset_video` is rare
/// ⇒ the steady state matches on the first try). Returns `None` on disagreement (a tear
/// in flight); the caller then takes the fps-unknown branch (frames 0 / fps 0.0), never
/// a divide-by-zero. This pure fn pins that acceptance rule so the C helper is checked
/// against a provably-correct reference.
pub fn genlock_fps_pair_consistent(a: (u32, u32), b: (u32, u32)) -> Option<(u32, u32)> {
    if a == b {
        Some(a)
    } else {
        None
    }
}

/// (#148) The audit counter a single timestamp-aligned render tick increments.
///
/// On the ts-align release path the FIFO is only entered with at least one queued
/// frame (`ready_async_frame` dereferences the head before this decision), so a
/// non-presented tick there is ALWAYS a SOURCE-EARLY HOLD — frames ARE queued, none
/// has yet reached its presentation deadline ([`genlock_release`] returned
/// `present == false` with a non-empty queue) — NOT a true-empty FIFO underrun. The
/// pre-#148 C code folded that hold into `genlock_underruns`, conflating a benign
/// source-early hold (the #136 boundary churn) with real starvation and making the
/// live ~3 fps churn undebuggable from the counters alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenlockTick {
    /// A due frame was presented this tick.
    Present,
    /// Frames queued, none due yet — repeat the current frame. Counted as
    /// `genlock_holds` (#148), NOT `genlock_underruns`.
    SourceEarlyHold,
    /// The FIFO is genuinely empty — a real underrun (counted as `genlock_underruns`).
    /// NOT reachable on the ts-align path (entered with >=1 queued frame); modelled so
    /// the hold-vs-underrun split is provable.
    Underrun,
}

/// Classify one ts-align tick from the queue depth and the due-frame count, so the C
/// counter choice (`genlock_holds` vs `genlock_underruns`) is unit-tested. `due` is the
/// [`genlock_release`] prefix count (queued frames at/before the deadline).
pub fn classify_ts_align_tick(queue_depth: usize, due: usize) -> GenlockTick {
    if queue_depth == 0 {
        GenlockTick::Underrun
    } else if due == 0 {
        GenlockTick::SourceEarlyHold
    } else {
        GenlockTick::Present
    }
}

/// An output frame-rate pair `(fps_num, fps_den)` — the canvas `ovi` fps the genlock
/// audit/preload path reads.
pub type FpsPair = (u32, u32);

/// The result of [`genlock_fps_cached`]: `(new_cache, result)` — the cache state after
/// the call, and the pair the caller uses (`None` ⇒ the fps-unknown fallback).
pub type FpsCacheUpdate = (Option<FpsPair>, Option<FpsPair>);

/// (#200 follow-up, #269 review) The LAST-GOOD-cached extension of
/// [`genlock_fps_pair_consistent`]. The C `genlock_video_fps()` keeps a file-scope
/// last-good `(fps_num, fps_den)` cache (the output fps is the GLOBAL canvas `ovi`, one
/// value shared by every genlock source, so a single cached pair is correct). On
/// AGREEMENT it accepts the fresh pair AND refreshes the cache; on a persistent TEAR it
/// returns the cached last-good pair instead of failing.
///
/// This is the #269 fix for two false-return regressions the bare
/// [`genlock_fps_pair_consistent`] introduced: a tear made
/// [`genlock_drop_cap`]'s caller skip the latency-frames bump → the per-source FIFO
/// drop-cap collapsed to the 30-frame floor → a deep-latency override momentarily
/// force-drained the FIFO (an A/V phase jump); and it made the C
/// `genlock_frame_interval_ns()` return 0 → the ts-align block was skipped for that tick
/// → the source briefly presented off the shared wall-clock deadline (a one-tick break of
/// the #136 multi-source in-sync invariant). Returning the cached pair on a transient
/// tear eliminates both while still never logging a torn pair (#200's goal).
///
/// Only a tear with NO good pair ever cached rejects (`None` ⇒ the first-ever call
/// mid-tear → the fps-unknown fallback). A degenerate `(0, _)` agreement is accepted but
/// NOT cached (it is not a good fps), mirroring the C `a.fps_num != 0` publish guard.
///
/// Returns `(new_cache, result)`: `new_cache` is the cache state after this call;
/// `result` is the pair the caller uses (`None` ⇒ fps-unknown fallback).
pub fn genlock_fps_cached(cache: Option<FpsPair>, a: FpsPair, b: FpsPair) -> FpsCacheUpdate {
    match genlock_fps_pair_consistent(a, b) {
        // Agree on a GOOD pair: accept it AND refresh the cache.
        Some(pair) if pair.0 != 0 => (Some(pair), Some(pair)),
        // Agree on a degenerate (0, _) pair: return it but do NOT cache it.
        Some(pair) => (cache, Some(pair)),
        // Tear: return the cached last-good pair; only a never-initialized cache rejects.
        None => (cache, cache),
    }
}

/// (#148 follow-up, #269 finding [4]) Classify one COUNT-GATE render tick into the audit
/// counter it increments. The count gate is entered (via `ready_async_frame`) only with
/// `queue_depth >= 1`, so a NON-consume tick there is ALWAYS a build-fill HOLD (still
/// establishing the preload delay; the startup-fill latch is unset) — a BENIGN repeat
/// that RECURS on every #126 reconnect re-arm — NOT a true-empty FIFO starvation. The
/// true-empty underrun is counted separately at the `queue_depth == 0` guard
/// (`get_closest_frame`), modelled here as `Underrun` for completeness. The pre-#269 C
/// folded the build-fill hold into `genlock_underruns`; this pins the corrected split
/// (build-fill HOLD → `genlock_holds`, true-empty → `genlock_underruns`).
pub fn classify_count_gate_tick(queue_depth: usize, consume: bool) -> GenlockTick {
    if queue_depth == 0 {
        GenlockTick::Underrun // true empty (counted at the num==0 guard, get_closest_frame)
    } else if !consume {
        GenlockTick::SourceEarlyHold // build-fill HOLD → genlock_holds (#269 [4])
    } else {
        GenlockTick::Present
    }
}

/// (#148 follow-up, #269 finding [5]) The ts-align decision sample (present_ts / due /
/// head-skew) the 5s audit line publishes. It is meaningful ONLY on a tick the ts-align
/// path actually sampled; a count-gate or true-empty tick has NO sample and MUST publish
/// the all-zero sentinel, so the audit never prints a STALE sample left over from an
/// earlier ts-align tick (the pre-#269 C wrote these fields ONLY in the ts-align branch
/// but `genlock_audit_log` printed them unconditionally — a ts-align source that fell
/// through to the count gate printed stale skew). Mirrors the C `genlock_clear_ts_sample()`
/// reset applied on the count-gate fall-through and the true-empty paths. `sampled` is
/// whether the ts-align branch produced a fresh sample this tick.
pub fn genlock_ts_audit_sample(
    sampled: bool,
    present_ts: u64,
    due: u32,
    head_skew_ns: i64,
) -> (u64, u32, i64) {
    if sampled {
        (present_ts, due, head_skew_ns)
    } else {
        (0, 0, 0) // sentinel: no ts-align sample this tick
    }
}

/// (#278) Pure mirror of the obs-display.c `render_display()` ADAPTIVE, budget-based skip
/// for a heavy monitoring display (the built-in Multiview projector, marked by
/// `render_divisor > 1`). This SUPERSEDES the #276 fixed every-Nth-frame skip: with 4 live
/// cams a SINGLE multiview render (~18-23 ms, rig-measured) alone exceeds the 16.6 ms 60 fps
/// budget, so even every-other-frame the frames it DID render overran the deadline → ~29 %
/// program renderSkip → the LED-wall IMAG program dropped to ~43 fps. The fix renders a
/// monitoring display ONLY when its measured cost (EWMA, ns) fits the budget REMAINING after
/// the program has already rendered this tick.
///
/// This mirrors only the BUDGET portion of the decision. As of #293 the full decision the
/// vendored C uses also has an anti-starvation FLOOR (a 4-live-cam multiview render alone
/// exceeds the budget every tick, so a budget-only skip froze the strih Multiview solid for a
/// whole live event). The full, OBS-dependency-free decision now lives in
/// `vendor/obs-studio/libobs/obs-display-budget.h` as `obs_display_should_skip(...)` and is
/// directly unit-tested by `tests/obs_display_budget.rs` (a standalone C harness over that
/// real header). `render_display()` calls it BEFORE `render_display_begin()` (skipping there
/// is ~0 cost — no clear/present — and leaves the last presented frame, so no flicker):
/// ```c
/// if (display->render_divisor > 1) {
///     ... read interval, tick_start, ewma; compute elapsed, budget = interval - interval/10 ...
///     if (obs_display_should_skip(display->render_divisor, ewma, elapsed, budget,
///                                 display->render_consecutive_skips)) {
///         display->render_consecutive_skips++;   /* #293: count the skip */
///         return;
///     }
/// }
/// /* a real render resets display->render_consecutive_skips = 0 */
/// ```
/// `display_render_skip_budget` returns `true` iff the display is over budget this frame.
/// Guarantees of the BUDGET portion:
/// - `render_divisor <= 1` (program output + preview) → NEVER throttled (always render).
/// - `ewma_ns == 0` (not warmed up) or `interval_ns == 0` (no timing yet) → render once to
///   measure — so a monitoring display is NEVER starved to 0 before it is even measured.
/// - Otherwise over budget iff `elapsed + ewma > 90% of the frame interval`. The C floor
///   (#293) then caps consecutive skips at `OBS_DISPLAY_MAX_CONSECUTIVE_SKIPS` so a heavy
///   monitoring display throttles to a reduced-but-NONZERO cadence (~15 fps at K=3) instead
///   of freezing, while the program (divisor ≤ 1) stays clean 60 fps.
///
/// Real GPU render timing needs the rig, which is why the supervisor rig-verifies the
/// 4-live-cam Multiview-unfreeze case.
pub fn display_render_skip_budget(
    render_divisor: u32,
    elapsed_ns: u64,
    ewma_ns: u64,
    interval_ns: u64,
) -> bool {
    // Program output + preview (divisor 0/1) are NEVER throttled — the program is sacred.
    if render_divisor <= 1 {
        return false;
    }
    // Not warmed up (no EWMA) or no frame timing yet → render once to measure, so a
    // monitoring display is never starved to 0 before its cost is even known.
    if ewma_ns == 0 || interval_ns == 0 {
        return false;
    }
    // 90% safety margin (matches the C `interval - interval / 10`). saturating_add mirrors
    // the C `elapsed + ewma` for the realistic ns domain while never panicking in debug.
    let budget = interval_ns - interval_ns / 10;
    elapsed_ns.saturating_add(ewma_ns) > budget
}

/// (#278) Pure mirror of the EWMA update applied in `render_display()` AFTER a real render of
/// a monitoring display, so the budget gate above can predict the next frame's cost. The
/// vendored C is `display->render_ewma_ns = prev ? (prev * 3 + dur) / 4 : dur;` — an
/// exponentially-weighted moving average with α = 1/4 (3/4 weight on history) that smooths
/// per-frame jitter while still tracking a load change within a few frames; a cold EWMA
/// (`prev == 0`) seeds with the first measured duration.
pub fn display_render_ewma_update(prev_ewma_ns: u64, measured_ns: u64) -> u64 {
    if prev_ewma_ns == 0 {
        // cold: seed with the first measured duration
        measured_ns
    } else {
        // α = 1/4: (prev*3 + dur)/4. saturating ops mirror the C plain arithmetic for the
        // realistic ns domain while never panicking in debug.
        (prev_ewma_ns.saturating_mul(3).saturating_add(measured_ns)) / 4
    }
}

// ============================================================================
// #275b — async cam1 capture-burn ring (move the per-frame QR render OFF the emit loop)
// ============================================================================
//
// The #174 cam1 capture-burn renders + blits the per-frame QR ON the capture/emit thread,
// between the genlock emit-gate and the NDI send. At a 60 fps emit that per-frame render is
// too heavy to hold the 16.6 ms budget, so cam1's NDI emit caps at 30 fps and the full chain
// can't be MEASURED at 60 (the #11 terminal zero-loss verdict needs it).
//
// The fix moves the render off the hot loop onto a dedicated burn thread fed by a bounded FIFO
// ring (`sync_channel`). The capture thread STAMPS each emitted frame's identity — the
// monotonic burn `frame_id`, the emit-instant `gen_ts_ns`, and the genlock boundary timecode
// of the EMITTED frame — at the gate (the genlock-authoritative instant), copies the frame, and
// hands it off; the burn thread renders the QR + NDI-sends it WITH the carried timecode. The
// ring gives the 2-3 frame look-ahead ("pre-render the next frame's QR while the current
// sends") and BACK-PRESSURES (blocks) instead of dropping, so the burn id ↔ emitted-frame
// mapping stays strictly 1:1 — a dropped job would punch a burn-id GAP the recording verdict
// misreads as a (phantom) chain loss, silently corrupting the zero-loss verdict.
//
// This module hosts the GENERIC, unit-testable ring mechanism (no frame layout, no NDI); the
// concrete `BurnJob` + thread wiring live in `main.rs`. The test below drives the real ring +
// a slow consumer to prove the 1:1 / in-order / no-drop / timecode-passthrough guarantee
// without an OBS/NDI build.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// #275b — depth of the async cam1-burn ring: how many emitted frames the capture thread may
/// queue ahead of the burn thread. 3 absorbs normal render jitter (the "pre-render the next
/// frame's QR while the current sends" look-ahead) while bounding the added latency to ≤ ~3
/// emit intervals.
pub const BURN_RING_DEPTH: usize = 3;

/// #279 FIX 2 — how long a full-ring [`BurnRing::submit`] blocks before re-checking the shutdown
/// flag. Short enough that shutdown unblocks the capture thread within a frame or two (so a
/// blocking submit can NEVER wedge the shutdown path when the burn thread is stalled in a
/// synchronous NDI send), long enough that the poll is negligible against the burn thread draining
/// a slot during ordinary back-pressure.
const SUBMIT_SHUTDOWN_POLL: Duration = Duration::from_millis(5);

/// #275b — monotonic per-EMITTED-frame burn id source. [`next_id`](Self::next_id) returns the
/// current id then advances (wrapping at `u32::MAX`, matching the legacy `burn_frame_id`).
/// Pulled once per emitted frame, in emit order, on the capture thread — that single in-order
/// draw is what keeps the burn id ↔ emitted-frame mapping strictly 1:1 once the QR render moves
/// to the async burn thread.
#[derive(Debug, Default)]
pub struct BurnFrameIdSource {
    next: u32,
}

impl BurnFrameIdSource {
    /// Return the current burn id, then advance (wrapping).
    pub fn next_id(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        id
    }
}

/// #279 FIX 2 — why [`BurnRing::submit`] returned without queuing the job. Both variants hand the
/// un-sent job back so the caller can log it (it is NEVER silently dropped).
#[derive(Debug)]
pub enum SubmitError<T> {
    /// The burn thread (receiver) is gone — the ring channel is closed. A genuine error.
    Closed(T),
    /// Shutdown was signalled (the `running` flag went false) while the ring was full, so the
    /// blocking submit was abandoned rather than parking the capture thread forever. This is the
    /// CLEAN-shutdown path, not a fault — it lets the capture thread reach `drop(ring)` / grab
    /// flush / burn-thread join even when the burn thread is stalled in a synchronous NDI send.
    ShutdownInterrupted(T),
}

/// #275b — capture-thread producer handle for the async cam1-burn ring (the sending side of a
/// bounded FIFO [`sync_channel`]).
pub struct BurnRing<T> {
    tx: SyncSender<T>,
    /// #279 FIX 2 — the capture loop's shared `running` flag. When it goes false a full-ring
    /// [`submit`](Self::submit) stops blocking and returns [`SubmitError::ShutdownInterrupted`]
    /// instead of parking the capture thread (which would never reach the while-loop's `running`
    /// re-check while the burn thread is stalled in `NDIlib_send_send_video_v2`).
    running: Arc<AtomicBool>,
}

impl<T> BurnRing<T> {
    /// Hand one emitted frame's burn work to the burn thread. MUST preserve the strict 1:1 burn
    /// id ↔ emitted-frame mapping: when the bounded ring is full it BACK-PRESSURES (waits for the
    /// burn thread to drain a slot) rather than dropping a job — a dropped job would punch a
    /// burn-id GAP the recording verdict misreads as a chain loss.
    ///
    /// #279 FIX 2 — the wait is INTERRUPTIBLE by shutdown. Instead of an unbounded blocking
    /// `SyncSender::send` (which parks the capture thread forever when the burn thread is stalled
    /// in a synchronous `NDIlib_send_send_video_v2` — it would never reach the capture loop's
    /// `running` re-check, so `drop(ring)`/grab-flush/join never run and the process wedges),
    /// submit retries `try_send` and re-checks `running` between attempts. On a full ring it keeps
    /// waiting WHILE running (never drops); once `running` goes false it returns
    /// [`SubmitError::ShutdownInterrupted`] promptly so the capture thread can drop the ring, flush
    /// the grab recording, and join the burn thread. At steady state the ring is not full, so the
    /// first `try_send` succeeds immediately and this never sleeps — the blocking-back-pressure
    /// design (and its 1:1 guarantee) is unchanged; only the unbounded park is removed.
    pub fn submit(&self, job: T) -> Result<(), SubmitError<T>> {
        let mut job = job;
        loop {
            match self.tx.try_send(job) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) => {
                    // Ring full → back-pressure. NEVER drop the job. Re-check the shutdown flag:
                    // keep waiting while running, abandon cleanly once shutdown is signalled.
                    if !self.running.load(Ordering::Relaxed) {
                        return Err(SubmitError::ShutdownInterrupted(returned));
                    }
                    job = returned;
                    // Brief sleep so a genuinely back-pressured capture thread stays responsive to
                    // shutdown without busy-spinning. Not reached at steady state (ring not full).
                    std::thread::sleep(SUBMIT_SHUTDOWN_POLL);
                }
                Err(TrySendError::Disconnected(returned)) => {
                    return Err(SubmitError::Closed(returned));
                }
            }
        }
    }
}

/// #275b — create the async cam1-burn ring: a bounded ([`BURN_RING_DEPTH`]) FIFO channel.
/// Returns the capture-thread [`BurnRing`] producer + the burn-thread [`Receiver`]. `running` is
/// the capture loop's shared run flag; it makes a full-ring [`BurnRing::submit`] interruptible on
/// shutdown (#279 FIX 2).
pub fn burn_ring<T>(running: Arc<AtomicBool>) -> (BurnRing<T>, Receiver<T>) {
    let (tx, rx) = sync_channel(BURN_RING_DEPTH);
    (BurnRing { tx, running }, rx)
}

/// #275b — run the async cam1-burn thread to completion: pop each job in RECEIVE (= emit) order
/// and hand it to `burn_and_send` (render the QR into the copied frame, then NDI-send it with the
/// gate-stamped timecode). Receiving over the FIFO [`Receiver`] preserves emit order exactly; the
/// loop ends when every [`BurnRing`] producer has dropped (capture loop exit), so the caller
/// `join`s this to flush the last queued frames before the NDI sender is destroyed. Generic over
/// the per-job action so the unit test substitutes a recorder for the NDI render+send.
pub fn run_burn_ring<T>(rx: Receiver<T>, mut burn_and_send: impl FnMut(T)) {
    while let Ok(job) = rx.recv() {
        burn_and_send(job);
    }
}

/// #279 FIX 3 — should the async cam1-burn render its QR into THIS frame? The QR burner
/// ([`crate::probe::qr::burn_qr_yuyv`]) assumes the YUYV byte layout, so only a YUYV frame may be
/// burned. cam1 captures a fixed YUYV format, but a v4l2 driver CAN substitute a different format
/// on `S_FMT` — and once the NDI sender moved onto the burn thread (#275b) a non-YUYV frame had no
/// emit path and was SILENTLY DROPPED, killing the entire cam1 feed on a format substitution. The
/// pre-#275b path always emitted such a frame UNBURNED; this predicate restores that: `false` ⇒
/// emit the frame as an unburned passthrough (still sent, still grab-written), NEVER dropped.
/// Pure so the render-vs-passthrough decision is unit-locked.
pub fn burn_should_render_qr(fourcc: &str) -> bool {
    fourcc == "YUYV"
}

/// #280 — capacity of the cam1-burn buffer pool's free list: [`BURN_RING_DEPTH`] + 2.
///
/// At most `ring depth (3 queued) + 1 (capture thread filling the next frame) + 1 (burn thread
/// rendering/sending the current frame)` = 5 buffers are ever in flight at once, so a free list of
/// 5 holds every recycled buffer without ever dropping one in steady state, while still bounding
/// total memory (no unbounded growth).
pub const BURN_POOL_CAP: usize = BURN_RING_DEPTH + 2;

/// #280 — bounded pool of reusable frame buffers for the async cam1-burn copy.
///
/// The #275b async burn hands each emitted frame's bytes capture-thread → burn-thread over the
/// [`BurnRing`]; the bytes MUST be copied off the V4L2 mmap (valid only inside the capture
/// callback). #275b copied with a per-frame `Vec::to_vec` (~4 MB at 1080p YUYV) → a fresh heap
/// allocation + free on EVERY emitted frame at up to 60 fps. This pool recycles those buffers: the
/// capture thread [`take`](Self::take)s a buffer (reusing a returned one, or allocating only when
/// the free list is empty), copies the frame in, and submits; the burn thread [`put`](Self::put)s
/// the buffer back after the NDI send. The free list is BOUNDED ([`BURN_POOL_CAP`]) so it can never
/// grow without limit — a `put` over the cap simply drops the buffer (it is freed). Memory is then
/// bounded by the peak in-flight count instead of churning one alloc per frame.
///
/// This is a pure MEMORY optimization — it carries no frame identity, so it CANNOT change the burn
/// id ↔ emitted-frame mapping, the frame ORDER, or the carried timecode (all stamped on the capture
/// thread and carried in the [`BurnRing`] job). Shared capture-thread ↔ burn-thread via `Arc`.
pub struct BufferPool {
    free: Mutex<Vec<Vec<u8>>>,
    cap: usize,
    /// Count of FRESH allocations [`take`](Self::take) had to make (free list was empty). After
    /// warm-up this stops climbing — that flat count is the proof the pool recycles rather than
    /// allocating per frame (vs the #275b `to_vec`, which allocated once per emitted frame).
    allocated: AtomicUsize,
}

impl BufferPool {
    /// Create an empty pool whose free list is bounded at `cap` buffers.
    pub fn new(cap: usize) -> Self {
        Self {
            free: Mutex::new(Vec::new()),
            cap,
            allocated: AtomicUsize::new(0),
        }
    }

    /// Take a buffer to copy a frame into: reuse a returned one when the free list is non-empty,
    /// else allocate a fresh `Vec` (and count it). The caller `clear()`s + fills it; a reused
    /// buffer keeps its ~4 MB capacity so the fill does not reallocate.
    pub fn take(&self) -> Vec<u8> {
        // Drop the lock BEFORE allocating on the empty path: pop releases the mutex, then the
        // fresh `Vec::new` (+ the counter bump) runs unlocked so it never holds the lock against
        // the burn thread's `put`.
        let popped = self.free.lock().unwrap().pop();
        match popped {
            Some(buf) => buf,
            None => {
                self.allocated.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            }
        }
    }

    /// Return a buffer for reuse after the burn thread has sent it. BOUNDED: if the free list is
    /// already at `cap`, drop the buffer (it is freed) so the pool can never grow without limit.
    pub fn put(&self, buf: Vec<u8>) {
        let mut free = self.free.lock().unwrap();
        if free.len() < self.cap {
            free.push(buf);
        }
        // else: at capacity — drop `buf` (freed). Bounds the pool's memory.
    }

    /// Number of FRESH allocations [`take`](Self::take) has made (free list empty). A count that
    /// stays flat after warm-up proves the pool recycles instead of allocating per frame.
    pub fn allocations(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }

    /// Current number of idle buffers held in the free list (≤ `cap`). Observability for the
    /// shutdown audit log; also lets a test assert the free list never grows past the cap.
    pub fn free_len(&self) -> usize {
        self.free.lock().unwrap().len()
    }
}

// ---- #401 phase-locked release cadence (mirror of the C ts-align fix) -----------------

/// One render tick's outcome from [`ReleaseCadence::tick`] — what the FIFO did, with every
/// discarded frame VISIBLE (the pre-#401 release silently erased stale due frames with no
/// counter, which is how run 7020001 lost 8,510 ids without a single audit signal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CadenceOutcome {
    /// The capture stamp presented this tick (`None` ⇒ HOLD: repeat the current frame).
    pub presented: Option<u64>,
    /// Frames discarded this tick (stale catch-up at lock/relock) — the honest `dropped_due`.
    pub dropped: Vec<u64>,
    /// HOLD because the matured boundary's frame has not ARRIVED yet (late/lost upstream) —
    /// distinct from the benign not-yet-due hold so the audit can separate them.
    pub late_hold: bool,
    /// This tick re-locked the cadence (stall/step recovery jump) — counts a relock event.
    pub relocked: bool,
}

/// #401 — per-source PHASE-LOCKED release cadence for the ts-align genlock FIFO.
///
/// WHY: the pre-#401 release re-derived the deadline from the wall clock EVERY tick
/// (`present_ts = wall_now − reserve`) and presented the NEWEST due frame, silently erasing
/// the older due ones. With render ticks and capture stamps on the same DanteSync 60 Hz grid,
/// a reserve near a multiple of the frame interval puts the deadline ON a stamp: the ±2 ms
/// render-tick slew then flips that frame due/not-due tick-to-tick — alternating HOLD +
/// silent DROP. Measured live (2026-07-02, `NDI cam5`): 43.9 distinct fps at 16/33 ms,
/// 57.7 at best-case 25 ms — no reserve value reaches 60, and no counter showed the loss.
///
/// FIX: derive the deadline from a LOCKED boundary that advances exactly one interval per
/// render tick — slew-immune by construction (no per-tick wall comparison to race). The wall
/// clock is consulted only to (a) acquire the initial lock and (b) detect DRIFT beyond
/// `interval + slack`, where the cadence re-locks (stall catch-up keeps the IMAG latency
/// contract) and counts the jumped frames HONESTLY in [`CadenceOutcome::dropped`].
///
/// SYNC (#136): every source stamps on the same shared grid, so locked boundary sequences
/// are grid-aligned across sources; steady-state multi-source in-sync is preserved.
#[derive(Debug, Clone, Default)]
pub struct ReleaseCadence {
    /// The boundary (capture-stamp instant) the NEXT tick will mature. `None` ⇒ unlocked.
    locked_next_boundary_ns: Option<u64>,
    /// camera-box #726 STICKY-N: the last CONFIRMED integer source-rate multiple (0 = none yet).
    /// The per-tick front-2-pair measurement ([`Self::measure_source_multiple`]) reads
    /// INCONCLUSIVE whenever the queue momentarily holds <2 frames or the front pair is
    /// non-monotonic (a DanteSync clock-step seam / out-of-order arrival) — on a jittery
    /// 60-into-30 input that dropped the release back to the present-oldest CRAWL for sustained
    /// runs (win5/win6 / CAM1 live, #726, `relocks` climbing ~2/s while sibling inputs stayed
    /// flat). This latch remembers the last confirmed multiple so an inconclusive tick reuses it
    /// instead of crawling; a fresh measurement is always the CONFIRMATION authority and updates
    /// it (a 1:1 rate re-latches to 1 → byte-identical), and it is CLEARED on relock/gap/acquire
    /// so a stale N can never outlive the rate it described. Mirror of the C field
    /// `obs_source_t::genlock_last_known_n` (obs-internal.h).
    last_known_n: u32,
    /// #859 follow-up: render ticks since the last SLEW-LIMITED SETTLE-BACK DRAIN fired (see
    /// [`Self::should_drain_one`]). Reset to 0 exactly when a drain fires; incremented every
    /// other plain-N==1-steady tick. Mirror of the C field
    /// `obs_source_t::genlock_ticks_since_drain` (obs-internal.h).
    ticks_since_last_drain: u64,
    /// camera-box #1003: the steady conveyor's own measured ON-AIR AGE (`wall_now − presented
    /// stamp`), updated on every STEADY / GAP-RESYNC present; `0` = UNSET (matches the C field's
    /// `bzalloc` zero-init), in which case the relock selection falls back to the source's
    /// configured latency. This is what makes an ACQUIRE / BACKLOG relock INHERIT the release
    /// PHASE (present the queued frame nearest this remembered age) instead of re-minting it from
    /// the instant-sampled newest-due comparison. PRESERVED across relocks (a relock corrects
    /// DEPTH, never phase); RE-DERIVED by a GAP RESYNC present (upstream skipped stamps, so the
    /// pre-gap age describes a timeline that no longer exists); CLEARED by the BACKLOG stale-anchor
    /// self-heal (a relock that would shed nothing proves the anchor cannot describe a queue this
    /// deep). The C's other clear sites (backward-step regime end, async flush, latency setpoint
    /// change) have no counterpart inside this reference sim, which models neither the backward-step
    /// guard nor flush/realloc. Mirror of the C field `obs_source_t::genlock_phase_anchor_ns`
    /// (obs-internal.h); the selection arithmetic is the Tier-0 authority
    /// [`crate::genlock_backlog::relock_select_nearest`] / [`relock_anchor_age_ns`] /
    /// [`phase_anchor_from_present`](crate::genlock_backlog::phase_anchor_from_present).
    phase_anchor_ns: u64,
}

impl ReleaseCadence {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while the cadence has a lock (diagnostic).
    pub fn is_locked(&self) -> bool {
        self.locked_next_boundary_ns.is_some()
    }

    /// Decide one render tick. `queue` holds the source's arrived-but-unpresented frames'
    /// capture stamps, OLDEST FIRST (single NDI source ⇒ monotonic); presented/dropped stamps
    /// are removed from it. `reserve_ms` is the per-source held latency; `interval_ns` the
    /// shared frame interval; `wall_now_ns` this tick's DanteSync wall instant.
    ///
    /// The wall clock is consulted ONLY to acquire the lock and to detect drift beyond
    /// [`Self::relock_drift_ns`]; the steady-state release keys on the LOCKED boundary, so the
    /// ±2 ms render-tick slew has no threshold to race (the pre-#401 churn source).
    ///
    /// #940 piece 3 SCOPE NOTE: the deadline below is intentionally the RAW (non-grid-
    /// -pinned) `genlock_present_ts_reserve()` value, unlike the C `ready_async_frame()`
    /// ts-align path, which now grid-quantizes it (`genlock_phase_pin_deadline` +
    /// `GENLOCK_PHASE_PIN_HYSTERESIS_NS`, see `src/genlock_backlog.rs`
    /// `phase_pinned_deadline`/`PHASE_PIN_HYSTERESIS_NS` — the Tier-0-tested pure mirror of
    /// that C arithmetic). This simulation harness is NOT wired to it: this struct's own
    /// test suite pins dozens of exact ACQUIRE/RELOCK frame-selection outcomes against the
    /// raw deadline, none of which are locally re-verifiable under this repo's Tier-0
    /// policy (probe-gated, CI-only) — rewiring every one of them without a way to observe
    /// the result before pushing is a correctness risk the design's own "unit-tested in the
    /// Tier-0 mirror first" instruction does not require taking. The production fix lives
    /// entirely in the C; `phase_pinned_deadline`/`phase_pinned_is_due` are independently
    /// Tier-0 unit-tested against the exact same numeric contract the C now uses.
    ///
    /// #1003 — the phase-continuity relock selection is now ADOPTED here (issue 1037): the
    /// ACQUIRE and BACKLOG branches below select the queued frame NEAREST the tracked phase
    /// anchor ([`Self::relock_select`] → [`crate::genlock_backlog::relock_select_nearest`] /
    /// [`relock_anchor_age_ns`](crate::genlock_backlog::relock_anchor_age_ns)) instead of the
    /// newest due one, [`Self::phase_anchor_ns`] tracks the conveyor's on-air age (updated on
    /// STEADY / GAP presents, preserved across relocks, cleared by the BACKLOG stale-anchor
    /// self-heal), and the harness routes the arithmetic through the same Tier-0 authority the C
    /// mirrors — so the reference sim documents the deployed C's SELECTION faithfully again. This
    /// was done with the probe suite runnable rather than smuggled in blind: the re-pinned
    /// outcomes were derived from a default-feature replica that imports the real authority (see
    /// the issue-1037 design comment), and the demonstrative tests below prove the new wiring.
    /// Every EXISTING pinned cadence test verifies IDENTICAL under the new selection — but that is
    /// an EMPIRICAL result over this corpus, not a general theorem: [`relock_select_nearest`]
    /// scans the WHOLE queue, so a not-yet-due frame nearer `wall_now − anchor_age` than the
    /// newest-due one WOULD be selected (dropping the newest-due frame) — exactly as the C does.
    /// The existing sims never construct that case (a cold ACQUIRE holds an arrival-gated queue
    /// whose frames all sit at/under the deadline, and the anchor is unset ⇒ target == the raw
    /// deadline), so nearest == newest-due there; a SET deep anchor is where the phase differs,
    /// which the demonstrative tests below add. CI is the final arbiter of the probe-test pins.
    ///
    /// TWO documented harness↔C divergences remain, both DELIBERATE. The first is the SEPARATE
    /// #940 piece 3 axis in the note above: this sim keeps the RAW `genlock_present_ts_reserve()`
    /// deadline where the C grid-quantizes it (`genlock_phase_pin_deadline` +
    /// `GENLOCK_PHASE_PIN_HYSTERESIS_NS`). That deadline change is a distinct question (it re-pins a
    /// different set of `due`-scan outcomes) and stays out of scope here;
    /// `phase_pinned_deadline`/`phase_pinned_is_due` are already independently Tier-0 unit-tested
    /// against the exact contract the C uses.
    ///
    /// The second is the #1161 Stage-2 ACQUIRE BRACKETING GATE
    /// (`crate::genlock_backlog::relock_acquire_should_hold`, wired into the C ACQUIRE branch and
    /// the `obs_source_set_genlock_latency_ms` pin-rise re-acquire). It is deliberately NOT mirrored
    /// here, for the SAME reason as the divergence above and one more: the gate exists ONLY to close
    /// the gap the #940 phase-pinned deadline opens — a frame up to one interval YOUNGER than the raw
    /// reserve qualifying `due`. This sim uses the RAW deadline, on which `due > 0` already implies
    /// `oldest_age >= reserve`, so `relock_acquire_should_hold` is STRUCTURALLY inert against a
    /// raw-deadline acquire (it can never fire here) — mirroring it would be dead code that changes
    /// no outcome. The gate's proof lives in the Tier-0 authority's own unit tests
    /// (`src/genlock_backlog.rs`) + the executable C-vs-Rust parity gate
    /// (`tests/genlock_relock_selection_parity.rs`) + the C-port static anchors
    /// (`tests/genlock_release_cadence.rs` + both `windows-genlock*.yml`), which is where a
    /// frame-mover that only bites the phase-pinned production path belongs.
    pub fn tick(
        &mut self,
        wall_now_ns: u64,
        reserve_ms: u32,
        interval_ns: u64,
        queue: &mut std::collections::VecDeque<u64>,
    ) -> CadenceOutcome {
        let deadline = genlock_present_ts_reserve(wall_now_ns, reserve_ms);
        let hold = |late: bool| CadenceOutcome {
            presented: None,
            dropped: Vec::new(),
            late_hold: late,
            relocked: false,
        };

        let Some(boundary) = self.locked_next_boundary_ns else {
            // ACQUIRE: first frame due by the wall deadline locks the cadence. Jump to the
            // newest due (startup backlog is stale by definition), counting the older ones.
            // #726 STICKY-N: a fresh acquire (cold start OR after a source reset that zeroed the
            // boundary) re-confirms the source multiple from scratch — clear the latch so a stale
            // N from a previous lock can't outlive it.
            self.last_known_n = 0;
            // #859 follow-up: a fresh lock starts the settle clock over — nothing has
            // overshot yet immediately after acquiring.
            self.ticks_since_last_drain = 0;
            let due = queue.iter().take_while(|&&ts| ts <= deadline).count();
            if due == 0 {
                return hold(false);
            }
            // #1003: select by PHASE CONTINUITY, not newest-due. The `due` scan above is
            // UNCHANGED and still QUALIFIES this branch (due > 0); it simply no longer SELECTS.
            // `sel` older frames are erased into `dropped` exactly as `due - 1` were before, so
            // the DEPTH correction is unchanged; only the PHASE is now inherited. On a cold
            // ACQUIRE the anchor is unset, so the target is the configured latency — the phase
            // the wall deadline would have produced anyway. Mirror of the C ACQUIRE branch
            // `release = genlock_relock_select_nearest(source, wall_now, reserve_ms) + 1`.
            let sel = self.relock_select(queue, wall_now_ns, reserve_ms);
            let mut dropped = Vec::with_capacity(sel);
            for _ in 0..sel {
                dropped.push(queue.pop_front().expect("relock prefix"));
            }
            let presented = queue.pop_front().expect("selected frame");
            self.locked_next_boundary_ns = Some(presented + interval_ns);
            return CadenceOutcome {
                presented: Some(presented),
                dropped,
                late_hold: false,
                relocked: false,
            };
        };

        // BACKLOG STORM (v2 — queue-relative, NEVER wall−boundary drift): a stall's burst
        // or a persistent inflow>presentation imbalance shows up as QUEUE DEPTH, which is
        // immune to the constant stamp→arrival skew. v1 guarded on `deadline − boundary`,
        // which EMBEDS that skew — the live canary (skew 59 ms, reserve 3 ms) relock-stormed:
        // dropped_due 2918/4202, relocks 1076. Steady depth is ~1–2 at ANY skew (the boundary
        // paces arrivals), so depth > backlog_relock_qdepth() is unambiguous backlog (#859: latency-relative); re-lock to the
        // newest due frame, counting every jumped frame (the catch-up keeps the IMAG latency
        // contract and the drop is VISIBLE).
        if queue.len() > self.backlog_relock_qdepth(queue, reserve_ms, interval_ns) {
            let due = queue.iter().take_while(|&&ts| ts <= deadline).count();
            if due > 0 {
                // #1003: same phase-continuity selection as the ACQUIRE branch. A backlog relock
                // must still SHED the stall's burst — it does, by erasing every frame OLDER than
                // the selected one (`sel` of them) — but it must no longer re-mint the release
                // PHASE while doing it. Mirror of the C `sel_1003 = genlock_relock_select_nearest(...)`.
                let mut sel = self.relock_select(queue, wall_now_ns, reserve_ms);
                // #1003 stale-anchor self-heal (adversarial-review finding): a backlog relock that
                // would shed NOTHING (sel == 0) is proof the anchor is STALE — this branch only
                // fires ABOVE the latency-implied depth, so an anchor pointing at (or before) the
                // queue head cannot describe a queue this deep. Carrying it would re-fire the
                // branch every tick shedding nothing (and, since the branch pre-empts STEADY, the
                // settle-back drain would never run either). Drop the stale anchor and re-select
                // against the CONFIGURED latency: one relock sheds the overshoot, and the anchor
                // rebuilds from the next STEADY present. ACQUIRE is deliberately exempt — index 0
                // there just means "present the head", and the fresh lock stops the branch
                // re-firing. Mirror of the C
                // `if (sel_1003 == 0 && source->genlock_phase_anchor_ns != 0) { ... }`.
                if sel == 0 && self.phase_anchor_ns != 0 {
                    self.phase_anchor_ns = 0;
                    sel = self.relock_select(queue, wall_now_ns, reserve_ms);
                }
                let mut dropped = Vec::with_capacity(sel);
                for _ in 0..sel {
                    dropped.push(queue.pop_front().expect("relock prefix"));
                }
                let presented = queue.pop_front().expect("selected frame");
                self.locked_next_boundary_ns = Some(presented + interval_ns);
                // #741/#707 B2: do NOT clear last_known_n here. A backlog re-lock is a QUEUE-DEPTH
                // event, NOT evidence the source RATE changed — the #726 clear made the next
                // INCONCLUSIVE tick crawl (N=1), re-growing the queue and re-triggering this relock
                // (a self-sustaining crawl loop, the #707 B2 crawl window uniform=0.481). The latch
                // bridges the post-relock inconclusive ticks; a genuine rate change re-confirms via
                // the next measurable front pair; a real source-timeline discontinuity still clears
                // it at acquire / gap resync / backward clock-step (and, in the C, flush/inactive).
                return CadenceOutcome {
                    presented: Some(presented),
                    dropped,
                    late_hold: false,
                    relocked: true,
                };
            }
            // Deep queue but nothing aged past the reserve yet (a just-landed burst of
            // fresh frames) — fall through; the matured path drains it next ticks or this
            // branch re-fires once they age.
        }

        // STEADY (strict FIFO): release the frame(s) matured by the LOCKED boundary.
        let matured = queue.iter().take_while(|&&ts| ts <= boundary).count();
        if matured > 0 {
            // camera-box #726: when the source runs at an integer multiple N>=2 of the canvas
            // render-tick rate (a 60fps NDI source into a 30fps canvas), the present-OLDEST-matured
            // path CRAWLS: the LOCKED boundary re-anchors to the presented stamp, and one canvas
            // interval lands a HAIR under N source intervals (30fps interval 33_333_333 ns vs
            // 2×60fps 33_333_334 ns), so the boundary matures only ONE frame per tick while N arrive
            // — content plays at ~1/N speed and the per-source queue grows ~(N-1) frames/tick until
            // the backlog storm above catches up with a multi-frame JUMP. That crawl-then-jump is the
            // live-event "like 15fps" judder (#726). FIX: for a structural N>=2 source, mature the
            // frames up to the boundary PLUS a half-interval slack (so the frame ~one canvas interval
            // ahead — the hair-past-boundary one — is included, the #136 boundary-churn tolerance),
            // present the NEWEST of them and retire the older matured one(s) into `dropped`. The
            // boundary re-anchors to that presented stamp, so it advances ONE canvas interval (=
            // N source frames) per tick — a uniform every-Nth-frame cadence that tracks real time.
            // Keying on the phase-locked BOUNDARY (not the wall clock) keeps it slew-immune — the
            // whole point of #401. Gated on the source being STRUCTURALLY at N>=2 (from the stamp
            // grid, not arrival timing) so a TRANSIENT 1:1 double-maturation stays LOSSLESS via the
            // present-oldest drain below — N==1 is byte-identical.
            // #726 STICKY-N (win5/win6 residual): the gate is the STICKY effective multiple, not a
            // per-tick front-2 re-derivation — an inconclusive tick (momentary num<2 / a
            // non-monotonic clock-step seam) bridges with the last CONFIRMED N instead of crawling
            // to present-oldest (which under-drained the queue into the backlog storm live). A fresh
            // measurement still wins (a genuine 1:1 rate re-latches to 1), and the latch is cleared
            // on acquire/relock/gap so a stale N cannot outlive its rate.
            if self.effective_source_multiple(queue, interval_ns) >= 2 {
                let mature_deadline = boundary + interval_ns / 2;
                let matured_n = queue
                    .iter()
                    .take_while(|&&ts| ts <= mature_deadline)
                    .count()
                    .max(1);
                let mut dropped = Vec::with_capacity(matured_n - 1);
                for _ in 0..matured_n - 1 {
                    dropped.push(queue.pop_front().expect("older matured"));
                }
                // #1049 PHASE CONVERGENCE: the N>=2 conveyor has NO depth-drain path and locks a
                // persistent phase, so shed one extra frame (present one SOURCE interval fresher,
                // re-anchoring the boundary to it below) when the boundary-implied age has drifted
                // a shed quantum over configured. `should_converge_phase` reads the OLD boundary
                // (not yet updated) and the shared throttle. On the N>=2 path the depth drain did
                // not run, so this also maintains `ticks_since_last_drain`. Mirror of the C tail's
                // converge block / the SimConveyor1049 shed.
                if self.should_converge_phase(queue, reserve_ms, interval_ns, wall_now_ns)
                    && queue.len() > 1
                {
                    dropped.push(queue.pop_front().expect("phase-converge shed"));
                    self.ticks_since_last_drain = 0;
                } else {
                    self.ticks_since_last_drain = self.ticks_since_last_drain.saturating_add(1);
                }
                let presented = queue.pop_front().expect("newest matured");
                self.locked_next_boundary_ns = Some(presented + interval_ns);
                // #1003: a STEADY present — the conveyor. Remember its own on-air age so the
                // next relock inherits this phase (the C present tail's shared `if (anchor_update)`).
                self.set_phase_anchor(wall_now_ns, presented);
                return CadenceOutcome {
                    presented: Some(presented),
                    dropped,
                    late_hold: false,
                    relocked: false,
                };
            }
            // N==1: present the OLDEST matured frame — exactly one in steady state; a transient
            // 2-frame maturation drains losslessly next tick (byte-identical to pre-#726).
            //
            // #859 follow-up: this is the ONE path the ticket's evidence found holds queue
            // depth CONSTANT forever after a setpoint-change overshoot (release=1/tick against
            // an inflow of exactly one). Check the slew-limited drain BEFORE popping anything,
            // so the depth it observes matches what the C's genlock_should_drain_one reads
            // (queue depth before this tick's release) — mirrors genlock_backlog_relock_qdepth's
            // own READ-ONLY convention.
            //
            // On a drain tick, drop the CURRENT oldest (what would otherwise be presented) and
            // present the NEXT one instead, re-anchoring the boundary to IT — the same
            // drop-older/present-newest idiom the ACQUIRE/relock/N>=2 paths already use. Simply
            // keeping the same presented frame and dropping the one behind it does NOT converge:
            // it desyncs the re-anchored boundary from the real (evenly-spaced) frame timeline,
            // so the VERY NEXT tick reads as a HOLD (nothing yet matured) and the queue regains
            // via a GAP RESYNC exactly what the drain just shed — a self-cancelling no-op,
            // confirmed by simulation before this was caught.
            let drain = self.should_drain_one(queue, reserve_ms, interval_ns);
            let mut dropped = Vec::new();
            if drain && queue.len() > 1 {
                let stale = queue
                    .pop_front()
                    .expect("drain: queue.len() > 1 checked above");
                dropped.push(stale);
                self.ticks_since_last_drain = 0;
            } else {
                self.ticks_since_last_drain = self.ticks_since_last_drain.saturating_add(1);
            }
            // #1049 PHASE CONVERGENCE (N==1): a residual phase below the #859 depth drain's
            // 2-frame hysteresis is shed here. The shared throttle prevents both firing this tick
            // (a drain just reset the counter to 0, so should_converge_phase reads ticks < the
            // interval and returns false) — the drain block above already maintained the counter,
            // so this block only sheds-or-nothing (never a second increment). Mirror of the C tail.
            if self.should_converge_phase(queue, reserve_ms, interval_ns, wall_now_ns)
                && queue.len() > 1
            {
                dropped.push(queue.pop_front().expect("phase-converge shed"));
                self.ticks_since_last_drain = 0;
            }
            let presented = queue.pop_front().expect("matured frame");
            self.locked_next_boundary_ns = Some(presented + interval_ns);
            // #1003: a STEADY present — the conveyor. Update the phase anchor from the frame put
            // on air (after any settle-back drain, so it reflects the actually-presented frame).
            self.set_phase_anchor(wall_now_ns, presented);
            return CadenceOutcome {
                presented: Some(presented),
                dropped,
                late_hold: false,
                relocked: false,
            };
        }

        // GAP RESYNC: nothing matured, but the oldest queued frame is BEYOND the boundary
        // and has aged past the reserve — upstream skipped stamps (sender restart, upstream
        // loss). Present it and re-anchor; not a drop of ours (nothing was discarded), not a
        // relock (no catch-up jump) — the boundary follows the real stream.
        if let Some(&oldest) = queue.front() {
            if deadline >= oldest {
                let presented = queue.pop_front().expect("oldest");
                self.locked_next_boundary_ns = Some(presented + interval_ns);
                // #726 STICKY-N: a GAP RESYNC means upstream skipped stamps (sender restart /
                // upstream loss) — the source timeline (and possibly its rate) changed. Clear the
                // latch so the post-gap stream re-confirms its multiple.
                self.last_known_n = 0;
                // #1003: a GAP RESYNC RE-DERIVES the phase anchor from the frame it puts on air.
                // Upstream skipped stamps, so the pre-gap age describes a timeline that no longer
                // exists — this present is both the "update on GAP" and the "do not carry the
                // pre-seam value forward" rule, in one assignment (the same seam that clears
                // STICKY-N above). Mirror of the C GAP-RESYNC `anchor_update = true` tail.
                self.set_phase_anchor(wall_now_ns, presented);
                return CadenceOutcome {
                    presented: Some(presented),
                    dropped: Vec::new(),
                    late_hold: false,
                    relocked: false,
                };
            }
        }

        // HOLD: late if the wall says the boundary frame should already be here (it aged
        // past the reserve upstream and hasn't arrived), benign otherwise.
        hold(deadline >= boundary)
    }

    /// #859 — the MARGIN above the depth a source's own configured latency implies, before its
    /// queue counts as a backlog storm. This was the WHOLE threshold until #859, which encoded
    /// the assumption "steady depth is ~1–2 at any arrival skew (the boundary paces arrivals)" —
    /// true only for a SHALLOW source. A source pinned deep (923 ms on the stream box's
    /// `NDI 2ME PGM`, to A/V-align against the mbc's 1 s mastering) has a steady depth of ~28, so
    /// the bare 6 was permanently exceeded and the cadence relocked EVERY tick, shedding a frame
    /// on every arrival-jitter excursion to `due == 2` and repeating one on the next.
    ///
    /// Mirror of the C `GENLOCK_QDEPTH_RELOCK_MARGIN` and of
    /// [`crate::genlock_backlog::QDEPTH_RELOCK_MARGIN`], which carries the Tier-0 unit tests.
    const QDEPTH_RELOCK_MARGIN: usize = 6;

    /// The backlog-storm queue-depth threshold for THIS source's configured latency: a depth
    /// strictly greater than this is a backlog. Delegates the arithmetic to the Tier-0-tested
    /// [`crate::genlock_backlog::backlog_relock_threshold`].
    ///
    /// The source-rate multiple matters: `queue` holds frames as the SOURCE delivered them, so a
    /// 60-into-30 input queues two entries per canvas interval and implies twice the depth the
    /// canvas rate alone would give. Mirror of the C `genlock_backlog_relock_qdepth`.
    fn backlog_relock_qdepth(
        &self,
        queue: &std::collections::VecDeque<u64>,
        reserve_ms: u32,
        interval_ns: u64,
    ) -> usize {
        if interval_ns == 0 {
            return Self::QDEPTH_RELOCK_MARGIN;
        }
        // READ-ONLY on purpose: use the PURE measurement with the sticky latch as fallback rather
        // than `effective_source_multiple`, which takes `&mut self` and would latch `last_known_n`
        // as a side effect of merely computing a threshold — on ticks that never touched it
        // before. Same value, no new write path in a getter.
        //
        // The source rate as an EXACT rational, never a truncated integer fps: a 29.97 canvas
        // would floor to 29 and under-state the implied depth. source_fps = 1e9 * n / interval_ns.
        let n = Self::measure_source_multiple(queue, interval_ns)
            .unwrap_or(self.last_known_n)
            .max(1);
        let (Ok(src_num), Ok(src_den)) = (
            u32::try_from(1_000_000_000u64.saturating_mul(n as u64)),
            u32::try_from(interval_ns),
        ) else {
            // Rates this extreme are not representable in the shared helper's u32 form; fall back
            // to the pre-#859 bare margin rather than inventing a threshold.
            return Self::QDEPTH_RELOCK_MARGIN;
        };
        // #940 piece 2: `n` is ALREADY measured above (used for the steady-depth SOURCE-rate
        // scaling via src_num/src_den) — reuse it for the MARGIN scaling too. Mirror of the C
        // `genlock_backlog_relock_qdepth`.
        let threshold =
            crate::genlock_backlog::backlog_relock_threshold(reserve_ms, src_num, src_den, n);
        usize::try_from(threshold).unwrap_or(usize::MAX)
    }

    /// #859 follow-up — SLEW-LIMITED SETTLE-BACK DRAIN decision: should THIS tick shed exactly
    /// ONE EXTRA frame to settle the queue back toward the depth its own configured latency
    /// implies, after a setpoint change? Delegates the Tier-0-tested decision to
    /// [`crate::genlock_backlog::should_drain_one`]; only the source-rate-adjusted fps
    /// conversion (identical to [`Self::backlog_relock_qdepth`]'s own conversion) lives here.
    ///
    /// READ-ONLY like `backlog_relock_qdepth` (same rationale — a decision getter must not
    /// acquire a write path): uses the pure measurement with the sticky latch as fallback, never
    /// `effective_source_multiple`. `queue.len()` is read BEFORE the caller pops the presented
    /// frame, matching the C `genlock_should_drain_one`, which reads `async_frames.num` before
    /// the caller's own removal of the presented frame.
    ///
    /// camera-box #998: this method has NO independent target arithmetic of its own — it
    /// delegates the WHOLE decision, including the target computation, to
    /// [`crate::genlock_backlog::should_drain_one`] below. That means the #998 round-to-ceil
    /// fix (see [`crate::genlock_backlog::drain_target_frames`]'s doc for the frac<0.5
    /// limit-cycle it fixes) is inherited here automatically — no change needed in this
    /// probe-gated file to stay in lock-step with the crate-root Tier-0-tested source of truth.
    ///
    /// Mirror of the C `genlock_should_drain_one`.
    fn should_drain_one(
        &self,
        queue: &std::collections::VecDeque<u64>,
        reserve_ms: u32,
        interval_ns: u64,
    ) -> bool {
        if interval_ns == 0 {
            return false;
        }
        let n = Self::measure_source_multiple(queue, interval_ns)
            .unwrap_or(self.last_known_n)
            .max(1);
        let (Ok(src_num), Ok(src_den)) = (
            u32::try_from(1_000_000_000u64.saturating_mul(n as u64)),
            u32::try_from(interval_ns),
        ) else {
            // Rates this extreme are not representable in the shared helper's u32 form; never
            // drain on an unrepresentable rate rather than inventing a decision.
            return false;
        };
        crate::genlock_backlog::should_drain_one(
            queue.len() as u64,
            reserve_ms,
            src_num,
            src_den,
            self.ticks_since_last_drain,
        )
    }

    /// #1049 — the STEADY-conveyor PHASE-CONVERGENCE shed decision. Delegates the whole decision
    /// to the Tier-0-tested [`crate::genlock_backlog::should_converge_phase`]; only the READ-ONLY
    /// source-multiple derivation lives here (same measure-with-sticky-fallback as
    /// [`Self::should_drain_one`] / [`Self::backlog_relock_qdepth`], never `effective_source_multiple`
    /// which would latch as a side effect of a getter). The comparator is the LOCKED boundary's
    /// implied age (`wall_now - locked_next_boundary_ns`), read before the caller updates the
    /// boundary; the achievable floor is the freshest queued frame (`queue.back()`, the C
    /// `array[num-1]`) — a frame cannot present before it arrives, so the target is
    /// `max(reserve, floor)` (issue-1049 review finding). Mirror of the C
    /// `genlock_should_converge_phase`.
    fn should_converge_phase(
        &self,
        queue: &std::collections::VecDeque<u64>,
        reserve_ms: u32,
        interval_ns: u64,
        wall_now_ns: u64,
    ) -> bool {
        let Some(boundary) = self.locked_next_boundary_ns else {
            return false;
        };
        let Some(&newest_stamp) = queue.back() else {
            return false;
        };
        if interval_ns == 0 {
            return false;
        }
        let n = Self::measure_source_multiple(queue, interval_ns)
            .unwrap_or(self.last_known_n)
            .max(1);
        crate::genlock_backlog::should_converge_phase(
            wall_now_ns,
            boundary,
            newest_stamp,
            reserve_ms,
            interval_ns,
            n,
            self.ticks_since_last_drain,
        )
    }

    /// #741/#707 B2 — how many front queued frames [`Self::measure_source_multiple`] scans for a
    /// strictly-increasing consecutive pair. Reading only the front pair read INCONCLUSIVE on a
    /// duplicate/degenerate front stamp, so a jittery 60-into-30 input crawled; scanning the first
    /// few entries recovers one real source interval past a leading duplicate. Mirror of the C
    /// `#define GENLOCK_MEASURE_SCAN_DEPTH`.
    const MEASURE_SCAN_DEPTH: usize = 6;

    /// camera-box #726 STICKY-N — FRESHLY measure the integer source-rate multiple N from the
    /// STAMP GRID of the front queued frames, or `None` when it cannot be measured this tick. The
    /// delta of a strictly-increasing consecutive stamp pair is the true source frame interval
    /// regardless of arrival jitter (a single NDI source delivers in monotonic capture order). A
    /// 60fps source into a 30fps canvas stamps every 16.6ms → `canvas / src ≈ 2` → `Some(2)`; a
    /// 1:1 source (30fps into 30fps) stamps every 33.3ms → `Some(1)`.
    ///
    /// #741/#707 B2: SCAN the first [`Self::MEASURE_SCAN_DEPTH`] entries for that pair rather than
    /// reading only `front()`/`get(1)` — a DUPLICATE front stamp or an arrival-non-monotonic seam
    /// at the very front used to read INCONCLUSIVE and (sustained) crawl. `None` = INCONCLUSIVE —
    /// fewer than 2 queued frames, or NO strictly-increasing pair in the scan window. `Some(N)`
    /// (N>=1) is a genuine measurement and the CONFIRMATION authority; the sticky latch bridges
    /// ONLY the `None` ticks (see
    /// [`Self::effective_source_multiple`]). Mirror of the C helper `genlock_measure_source_multiple`
    /// in obs-source.c.
    fn measure_source_multiple(
        queue: &std::collections::VecDeque<u64>,
        canvas_interval_ns: u64,
    ) -> Option<u32> {
        if canvas_interval_ns == 0 {
            return None;
        }
        // #741/#707 B2 ROBUST: scan the first K entries for a strictly-increasing consecutive
        // pair (skip a degenerate front pair — a DUPLICATE stamp or an arrival non-monotonic
        // seam). #1042: take the MINIMUM adjacent grid delta over that window, not the first —
        // every source stamps on the monotonic DanteSync grid, so the true frame interval is the
        // SMALLEST gap; a duplicate/dropped frame only ENLARGES a gap. The pure crate-root seam
        // `genlock_backlog::source_interval_from_stamps` is the Tier-0-tested authority (the C
        // `genlock_measure_source_multiple` mirrors the same min-loop). Still None when NO
        // increasing pair exists in the window — the sticky latch bridges the None.
        let scan = queue.len().min(Self::MEASURE_SCAN_DEPTH);
        let window: Vec<u64> = queue.iter().take(scan).copied().collect();
        let src_interval = crate::genlock_backlog::source_interval_from_stamps(&window)?;
        // Round-to-nearest N = canvas / src; clamp to >=1 (a slower-than-canvas source reads 1).
        let n = (canvas_interval_ns + src_interval / 2) / src_interval;
        Some(n.max(1) as u32)
    }

    /// camera-box #726 STICKY-N — the EFFECTIVE source-rate multiple to release at THIS tick.
    ///
    /// A fresh measurement ([`Self::measure_source_multiple`]) is the CONFIRMATION authority: when
    /// the front pair is measurable it WINS and updates the latch (so a genuine 1:1 rate re-latches
    /// to 1 → the present-oldest lossless path, byte-identical). When the front pair is INCONCLUSIVE
    /// (momentary num<2 / a non-monotonic clock-step seam) it BRIDGES with the last confirmed
    /// multiple instead of crawling — the #726 residual fix. It NEVER invents a multiple: an
    /// unconfirmed latch (0) reads 1. The latch is CLEARED on relock/gap/acquire (see [`Self::tick`])
    /// so a stale N can never outlive the rate it described. Mirror of the C helper
    /// `genlock_effective_source_multiple` in obs-source.c.
    fn effective_source_multiple(
        &mut self,
        queue: &std::collections::VecDeque<u64>,
        canvas_interval_ns: u64,
    ) -> u32 {
        match Self::measure_source_multiple(queue, canvas_interval_ns) {
            Some(n) => {
                self.last_known_n = n; // fresh measurement is the confirmation authority
                n
            }
            None => self.last_known_n.max(1), // bridge with the latch; never invent (0 -> 1)
        }
    }

    /// camera-box #1003 — the phase-continuity relock SELECTION index into `queue` (arrival
    /// order, OLDEST first): the frame whose capture stamp is NEAREST the anchor-implied target
    /// `wall_now_ns − relock_anchor_age_ns(phase_anchor_ns, reserve_ms)`.
    ///
    /// This is a thin adapter over the Tier-0-tested crate-root authority — it re-implements
    /// NOTHING. [`crate::genlock_backlog::relock_anchor_age_ns`] floors the tracked anchor at the
    /// configured latency (unset ⇒ the configured latency, the cold-ACQUIRE fallback), and
    /// [`crate::genlock_backlog::relock_select_nearest`] does the nearest-neighbour scan (ties
    /// toward the OLDER frame). The C `genlock_relock_select_nearest` (obs-source.c) mirrors the
    /// same authority in lock-step, and the two are held byte-identical by the executable parity
    /// gate `tests/genlock_relock_selection_parity.rs` — so this harness joins NO new selection
    /// surface, it just calls the one already covered.
    fn relock_select(
        &self,
        queue: &std::collections::VecDeque<u64>,
        wall_now_ns: u64,
        reserve_ms: u32,
    ) -> usize {
        let anchor_age =
            crate::genlock_backlog::relock_anchor_age_ns(self.phase_anchor_ns, reserve_ms);
        let queue_ts: Vec<u64> = queue.iter().copied().collect();
        crate::genlock_backlog::relock_select_nearest(&queue_ts, wall_now_ns, anchor_age)
    }

    /// camera-box #1003 — record the conveyor's own on-air age after a STEADY / GAP-RESYNC
    /// present, so the next relock INHERITS this phase. This is the reference-sim form of the C
    /// present tail's single shared `if (anchor_update) genlock_phase_anchor_ns =
    /// genlock_phase_anchor_from_present(wall_now, next_frame->timestamp)` — the harness's
    /// per-branch early returns make a literal shared tail awkward, so the three conveyor arms
    /// call this instead. The relock arms (ACQUIRE / BACKLOG) deliberately do NOT call it: a
    /// relock inherits the phase, it must never mint one, or every lock episode re-rolls a phase.
    fn set_phase_anchor(&mut self, wall_now_ns: u64, presented_ts_ns: u64) {
        self.phase_anchor_ns =
            crate::genlock_backlog::phase_anchor_from_present(wall_now_ns, presented_ts_ns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- #401 phase-locked release cadence ---------------------------------

    /// Drive a [`ReleaseCadence`] through a deterministic rig model: render ticks and
    /// capture stamps share one 60 Hz grid (DanteSync); the render tick wall time slews
    /// ±2 ms tick-to-tick (the live slew cap); every frame ARRIVES `skew_ns` after its
    /// stamp (the measured ~20 ms stamp→arrival pipeline). Returns
    /// `(presented_stamps, dropped_stamps, late_holds)`.
    fn run_cadence_sim(
        reserve_ms: u32,
        skew_ns: u64,
        n_frames: u64,
    ) -> (Vec<u64>, Vec<u64>, usize) {
        run_cadence_sim_skewfn(reserve_ms, &|_f| skew_ns, n_frames)
    }

    /// Like [`run_cadence_sim`] but with a per-frame arrival-skew function — models a
    /// mid-run pipeline change (the live 2026-07-02 canary saw the stamp→arrival skew at
    /// 59 ms where the earlier audit read 19–21 ms; v1's wall-based drift guard embedded
    /// that constant and relock-stormed: dropped_due=2918/4202, relocks=1076).
    fn run_cadence_sim_skewfn(
        reserve_ms: u32,
        skew_of: &dyn Fn(u64) -> u64,
        n_frames: u64,
    ) -> (Vec<u64>, Vec<u64>, usize) {
        const I: u64 = 16_666_667; // 60 Hz interval
        const BASE: u64 = 1_000_000_000_000; // arbitrary epoch offset
        let mut cadence = ReleaseCadence::new();
        let mut queue: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut next_arrival: u64 = 0; // next frame index to enqueue
        let mut presented = Vec::new();
        let mut dropped = Vec::new();
        let mut late_holds = 0usize;
        // Enough ticks for every frame to be released well past its arrival.
        let n_ticks = n_frames + 20;
        for k in 0..n_ticks {
            let slew: i64 = if k % 2 == 0 { 2_000_000 } else { -2_000_000 };
            let wall = (BASE + k * I).saturating_add_signed(slew);
            // Frames whose arrival instant (stamp + skew + per-frame jitter) has passed
            // enter the queue. The deterministic ±4 ms jitter models real NDI delivery —
            // WITHOUT it the pre-#401 release only churns at grid-aligned reserves; with
            // it the live curve reproduces (17–50% silent loss at 3/8/16/33 ms).
            while next_arrival < n_frames
                && BASE
                    + next_arrival * I
                    + skew_of(next_arrival)
                    + ((next_arrival * 2_654_435_761) % 8_000_001)
                    - 4_000_000
                    <= wall
            {
                queue.push_back(BASE + next_arrival * I);
                next_arrival += 1;
            }
            let out = cadence.tick(wall, reserve_ms, I, &mut queue);
            if let Some(ts) = out.presented {
                presented.push(ts);
            }
            dropped.extend(out.dropped);
            if out.late_hold {
                late_holds += 1;
            }
        }
        (presented, dropped, late_holds)
    }

    /// #402 — drives [`ReleaseCadence::tick`] through a DEEP reserve, then a backward
    /// DanteSync wall-clock step (Δ > one frame interval — the #147 trigger) applied to
    /// BOTH the wall clock and every subsequent capture stamp (a real backward step
    /// regresses the SAME shared clock that stamps captures). Arrival is gated on REAL
    /// elapsed ticks — never on comparing a (possibly stepped) stamp against a (possibly
    /// stepped) wall — because a logical clock correction changes what a frame's STAMP
    /// reads and what `wall_now` reads, it does NOT un-deliver frames already in flight
    /// over the network. Returns, for every presented frame, `(render_tick,
    /// held_latency_ns)` where `held_latency_ns = wall_now − presented_ts` — the actual
    /// delay between capture and presentation — so a test can assert it stays ≈
    /// `reserve_ms` straight through the seam instead of collapsing toward the live edge.
    fn run_cadence_sim_backward_step(
        reserve_ms: u32,
        skew_ns: u64,
        step_frame_idx: u64,
        step_ns: u64,
        n_frames: u64,
    ) -> Vec<(u64, i64)> {
        const I: u64 = 16_666_667; // 60 Hz interval
        const BASE: u64 = 10_000_000_000_000; // large epoch, well clear of the step
        let mut cadence = ReleaseCadence::new();
        let mut queue: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut next_arrival: u64 = 0;
        let mut results = Vec::new();
        let lag_ticks = skew_ns.div_ceil(I);
        let n_ticks = n_frames + lag_ticks + 60;
        for k in 0..n_ticks {
            // The LOGICAL (DanteSync) wall clock steps BACKWARD by `step_ns` once `k`
            // passes `step_frame_idx` — the SAME instant capture stamps below step, since
            // both derive from the one shared clock — and runs normally from there.
            let wall = if k < step_frame_idx {
                BASE + k * I
            } else {
                BASE + k * I - step_ns
            };
            // Physical arrival is gated on elapsed ticks, not on the (steppable) wall —
            // frames already in flight over the network are not un-delivered by a logical
            // clock correction; only their STAMP value (below) reflects the step.
            while next_arrival < n_frames && next_arrival + lag_ticks <= k {
                let stamp = if next_arrival < step_frame_idx {
                    BASE + next_arrival * I
                } else {
                    BASE + next_arrival * I - step_ns
                };
                queue.push_back(stamp);
                next_arrival += 1;
            }
            let out = cadence.tick(wall, reserve_ms, I, &mut queue);
            if let Some(ts) = out.presented {
                results.push((k, wall as i64 - ts as i64));
            }
        }
        results
    }

    /// #726 — drive the cadence with a SOURCE arriving at `src_interval_ns` into a CANVAS whose
    /// render tick fires at `canvas_interval_ns` (the live 60fps-camera-into-30fps-strih-canvas
    /// case). The render-tick wall time slews ±2ms; frames arrive `skew_ns` after their stamp
    /// with the same deterministic ±4ms NDI jitter the 1:1 sim uses. Returns
    /// `(presented_src_frame_indices, dropped_src_frame_indices)` — each stamp mapped back to its
    /// source-frame INDEX `(ts − BASE) / src_interval_ns`, so a test reads the PRESENTATION
    /// CADENCE directly (consecutive index deltas): a smooth every-Nth-frame cadence at N =
    /// canvas/src is Δ==N uniform; the pre-#726 crawl is Δ==1 punctuated by backlog-storm jumps.
    fn run_cadence_sim_ratio(
        reserve_ms: u32,
        src_interval_ns: u64,
        canvas_interval_ns: u64,
        skew_ns: u64,
        n_src_frames: u64,
    ) -> (Vec<u64>, Vec<u64>) {
        const BASE: u64 = 1_000_000_000_000;
        let mut cadence = ReleaseCadence::new();
        let mut queue: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut next_arrival: u64 = 0;
        let mut presented_idx = Vec::new();
        let mut dropped_idx = Vec::new();
        // Enough canvas ticks to drain every source frame (source runs faster than the canvas).
        let n_ticks = n_src_frames * src_interval_ns / canvas_interval_ns + 40;
        for k in 0..n_ticks {
            let slew: i64 = if k % 2 == 0 { 2_000_000 } else { -2_000_000 };
            let wall = (BASE + k * canvas_interval_ns).saturating_add_signed(slew);
            // A source frame enters the queue once its arrival instant (stamp + skew + ±4ms
            // jitter) has passed. next_arrival is processed in order, so the queue stays
            // monotonic in stamps regardless of the jitter (a later frame never overtakes).
            while next_arrival < n_src_frames
                && BASE
                    + next_arrival * src_interval_ns
                    + skew_ns
                    + ((next_arrival * 2_654_435_761) % 8_000_001)
                    - 4_000_000
                    <= wall
            {
                queue.push_back(BASE + next_arrival * src_interval_ns);
                next_arrival += 1;
            }
            let out = cadence.tick(wall, reserve_ms, canvas_interval_ns, &mut queue);
            if let Some(ts) = out.presented {
                presented_idx.push((ts - BASE) / src_interval_ns);
            }
            for d in out.dropped {
                dropped_idx.push((d - BASE) / src_interval_ns);
            }
        }
        (presented_idx, dropped_idx)
    }

    /// #401 REGRESSION LOCK — the measured live failure: at a grid-aligned reserve
    /// (16 ms ≈ one 60 Hz interval) the pre-#401 per-tick wall-compare release churned
    /// hold↔drop and delivered only ~44 of 60 distinct fps on `NDI cam5` (and 43.8 at
    /// 33 ms), silently. The cadence must release EVERY frame EXACTLY ONCE with nothing
    /// silently dropped, at every reserve the operator can pick.
    #[test]
    fn cadence_releases_every_frame_once_at_grid_aligned_reserve() {
        for reserve_ms in [3u32, 8, 16, 25, 33] {
            let (presented, dropped, _late) = run_cadence_sim(reserve_ms, 20_000_000, 600);
            assert_eq!(
                dropped,
                Vec::<u64>::new(),
                "reserve {reserve_ms} ms: steady 60→60 flow must drop NOTHING (pre-#401 \
                 dropped ~16/s at 16/33 ms — the run-7020001 loss)"
            );
            let mut uniq = presented.clone();
            uniq.dedup();
            assert_eq!(
                uniq.len(),
                600,
                "reserve {reserve_ms} ms: every one of 600 frames must be presented \
                 exactly once (got {} distinct of {} presents)",
                uniq.len(),
                presented.len()
            );
            // Order preserved (a cadence never goes backward).
            let mut sorted = uniq.clone();
            sorted.sort_unstable();
            assert_eq!(uniq, sorted, "reserve {reserve_ms} ms: presentation order");
        }
    }

    /// #401 v2 REGRESSION LOCK — the LIVE canary failure of cadence v1 (2026-07-02,
    /// strih `NDI cam5`): with the stamp→arrival skew at 59 ms and the 3 ms reserve, v1's
    /// wall-based drift guard (`deadline − boundary > 2.25·I`) embedded the constant
    /// arrival latency (59 − 3 = 56 ms > 37.5 ms) and RELOCK-STORMED — dropped_due
    /// 2918 of 4202 received (69 %), relocks 1076. The cadence must deliver every frame
    /// at ANY constant arrival skew: the steady release may key ONLY on queue-relative
    /// state (matured backlog, queue depth), never on wall−boundary drift.
    #[test]
    fn cadence_survives_deep_arrival_skew() {
        for skew_ms in [20u64, 59, 90] {
            for reserve_ms in [3u32, 16, 33] {
                let (presented, dropped, _late) =
                    run_cadence_sim_skewfn(reserve_ms, &|_f| skew_ms * 1_000_000, 600);
                assert_eq!(
                    dropped,
                    Vec::<u64>::new(),
                    "skew {skew_ms} ms / reserve {reserve_ms} ms: zero drops required \
                     (v1 relock-stormed at skew 59)"
                );
                let mut uniq = presented.clone();
                uniq.dedup();
                assert_eq!(
                    uniq.len(),
                    600,
                    "skew {skew_ms} ms / reserve {reserve_ms} ms"
                );
            }
        }
    }

    /// #401 v2 — a MID-RUN skew shift (pipeline slows 20 → 80 ms) must lose nothing: the
    /// cadence holds through the transition and settles at the new arrival phase.
    #[test]
    fn cadence_adapts_to_mid_run_skew_shift() {
        let (presented, dropped, _late) =
            run_cadence_sim_skewfn(3, &|f| if f < 300 { 20_000_000 } else { 80_000_000 }, 600);
        assert_eq!(dropped, Vec::<u64>::new(), "skew shift must drop nothing");
        let mut uniq = presented.clone();
        uniq.dedup();
        assert_eq!(uniq.len(), 600);
    }

    /// #726 REGRESSION LOCK — the live-event "like 15fps" judder at a 30fps canvas. A 60fps
    /// NDI source feeding a 30fps canvas must present a UNIFORM every-2nd-frame cadence (each
    /// presented frame is exactly 2 source frames past the previous, so presented content tracks
    /// real time), NOT the pre-#726 crawl: STEADY presented the OLDEST matured frame and advanced
    /// the boundary by one CANVAS interval, so content advanced only +1 SOURCE frame per tick
    /// while real time advanced 2 → content fell progressively behind (playing ~half speed), the
    /// per-source queue grew ~1 frame/tick until `genlock_backlog_relock_qdepth()` fired the backlog storm,
    /// which JUMPED ~+7 frames (~5×/s). The crawl+jump nets to the right average (2.0/frame → the
    /// loss gates stay clean, which is why every earlier gate was blind to it) but visibly halves
    /// perceived motion. The fix: when the source is at an integer multiple N>=2 of the canvas
    /// rate (derived from the stamp grid), STEADY presents the NEWEST matured frame and retires
    /// the older matured one(s), collapsing the delta histogram to a clean Δ==2.
    #[test]
    fn cadence_60_into_30_presents_uniform_every_second_frame() {
        const SRC_I: u64 = 16_666_667; // 60 Hz source (cam2 painter / camera emit)
        const CANVAS_I: u64 = 33_333_333; // 30 Hz strih canvas render tick
                                          // reserve 3ms = the production genlock floor; 20ms stamp→arrival skew = the measured live
                                          // pipeline latency (same as the 1:1 sims).
        let (presented, _dropped) = run_cadence_sim_ratio(3, SRC_I, CANVAS_I, 20_000_000, 400);
        // Skip the ACQUIRE / cold-start window (matches the live win0 "cold-start noise on top"
        // vs the clean win1/win2); read the steady-state cadence.
        let steady: Vec<u64> = presented.iter().skip(15).copied().collect();
        assert!(
            steady.len() > 100,
            "#726: expected a long steady presented window, got {}",
            steady.len()
        );
        let deltas: Vec<i64> = steady
            .windows(2)
            .map(|w| w[1] as i64 - w[0] as i64)
            .collect();
        let mut hist = std::collections::BTreeMap::new();
        for &d in &deltas {
            *hist.entry(d).or_insert(0usize) += 1;
        }
        let uniform = deltas.iter().filter(|&&d| d == 2).count();
        let frac = uniform as f64 / deltas.len() as f64;
        assert!(
            frac > 0.95,
            "#726: a 60fps source into a 30fps canvas must present a UNIFORM every-2nd-frame \
             cadence (Δ==2 source frames per presented frame); got {:.1}% uniform of {} deltas, \
             histogram {:?} — the pre-#726 crawl (mostly Δ==1) then backlog jump (Δ==7) is the \
             live-event 15fps-like judder",
            frac * 100.0,
            deltas.len(),
            hist
        );
        // No net loss: mean delta ≈ 2 (every-other-frame, long-run real-time).
        let mean: f64 = deltas.iter().map(|&d| d as f64).sum::<f64>() / deltas.len() as f64;
        assert!(
            (mean - 2.0).abs() < 0.1,
            "#726: 60→30 mean presented-frame step must be ≈2 (got {mean:.3})"
        );
        // Cadence never runs backward.
        assert!(
            deltas.iter().all(|&d| d >= 1),
            "#726: presentation order must be preserved"
        );
    }

    /// #726 — the fix must NOT change the 1:1 (source rate == canvas rate) path: a 30fps source
    /// into a 30fps canvas still presents EVERY frame exactly once with nothing dropped (the
    /// present-oldest lossless drain). `measure_source_multiple` is derived from the STAMP
    /// GRID (33.3ms stamps → N==1), so the multi-consume never engages here.
    #[test]
    fn cadence_30_into_30_still_lossless_every_frame() {
        const I: u64 = 33_333_333; // 30 Hz source AND 30 Hz canvas — the 1:1 stream 'NDI 2ME PGM' path
        let (presented, dropped) = run_cadence_sim_ratio(3, I, I, 20_000_000, 300);
        let steady: Vec<u64> = presented.iter().skip(15).copied().collect();
        let deltas: Vec<i64> = steady
            .windows(2)
            .map(|w| w[1] as i64 - w[0] as i64)
            .collect();
        assert!(
            deltas.iter().all(|&d| d == 1),
            "#726: a 1:1 source must present EVERY frame (Δ==1), never skip — the multi-consume \
             must stay gated to the integer-multiple case"
        );
        assert!(
            dropped.is_empty(),
            "#726: a 1:1 source must drop NOTHING (got {} drops) — the present-oldest lossless \
             drain path is unchanged for N==1",
            dropped.len()
        );
    }

    /// #726 RESIDUAL (win5/win6 / CAM1 live, 2026-07-13) RED→GREEN — STICKY-N.
    ///
    /// The fast-swap fix (dev.355) made STEADY multi-consume when the front-2 pair measures a
    /// structural N>=2. But that per-tick re-derivation reads INCONCLUSIVE whenever the queue
    /// momentarily holds <2 frames OR the front pair is non-monotonic (a DanteSync clock-step
    /// seam / out-of-order arrival) — and on a jittery input (win6/'NDI cam5'→CAM1) a SUSTAINED
    /// run of inconclusive detections dropped the release back to the present-oldest CRAWL (Δ==1),
    /// under-drained the queue, and backlog-stormed (the live {1:459,7:46} histogram, `relocks`
    /// climbing ~2/s while sibling 60-in-30 inputs stayed flat in the SAME window).
    ///
    /// The fix LATCHES the last CONFIRMED multiple and reuses it to bridge an inconclusive tick,
    /// so the cadence stays at N even across momentary jitter. A fresh measurement is always the
    /// confirmation authority (a genuine 1:1 rate re-latches to 1 → byte-identical). This test
    /// drives the N-decision through a clean-then-inconclusive sequence: the pre-sticky code
    /// returns 1 (crawl) on the inconclusive states; sticky-N returns the latched 2.
    #[test]
    fn sticky_n_bridges_inconclusive_ticks_after_confirming_the_multiple() {
        use std::collections::VecDeque;
        const CANVAS: u64 = 33_333_333; // 30 Hz canvas
        let mut cadence = ReleaseCadence::new();

        // A clean 60fps front pair (16.6ms apart) CONFIRMS N==2 and latches it.
        let clean: VecDeque<u64> = [1_000_000_000, 1_016_666_667, 1_033_333_334].into();
        assert_eq!(
            cadence.effective_source_multiple(&clean, CANVAS),
            2,
            "#726: a clean 60fps front pair must confirm N==2"
        );

        // INCONCLUSIVE #1 — only ONE frame queued (num<2). The pre-sticky code reads 1 (crawl);
        // sticky-N must reuse the latched 2.
        let one: VecDeque<u64> = [2_000_000_000].into();
        assert_eq!(
            cadence.effective_source_multiple(&one, CANVAS),
            2,
            "#726 STICKY-N: a momentary num<2 drain must reuse the latched N (2), not crawl to 1"
        );

        // INCONCLUSIVE #2 — non-monotonic front pair (a clock-step seam, arrival out of stamp
        // order). Pre-sticky reads 1; sticky-N must still bridge with the latched 2.
        let seam: VecDeque<u64> = [3_000_000_000, 2_999_000_000, 3_016_000_000].into();
        assert_eq!(
            cadence.effective_source_multiple(&seam, CANVAS),
            2,
            "#726 STICKY-N: a non-monotonic front pair must reuse the latched N (2), not crawl"
        );

        // A GENUINE 1:1 measurement (33.3ms front pair) is the confirmation authority — it wins
        // and RE-LATCHES to 1 (a real rate change), so the fix never fossilises a stale multiple.
        let onetoone: VecDeque<u64> = [4_000_000_000, 4_033_333_333].into();
        assert_eq!(
            cadence.effective_source_multiple(&onetoone, CANVAS),
            1,
            "#726: a fresh 1:1 measurement must win and re-latch to 1 (rate change), never keep 2"
        );

        // Now an inconclusive tick reuses the RE-LATCHED 1 (not the stale 2) — never invents N.
        let one2: VecDeque<u64> = [5_000_000_000].into();
        assert_eq!(
            cadence.effective_source_multiple(&one2, CANVAS),
            1,
            "#726 STICKY-N: after a 1:1 re-latch, an inconclusive tick reuses 1, not the old 2"
        );
    }

    /// #726 STICKY-N — a FRESH ReleaseCadence with no confirmed multiple yet must NOT invent one:
    /// an inconclusive first tick (num<2) reads 1 (present-oldest), never a fabricated N>=2. The
    /// latch only ever holds a value the front pair actually confirmed.
    #[test]
    fn sticky_n_never_invents_a_multiple_before_confirmation() {
        use std::collections::VecDeque;
        const CANVAS: u64 = 33_333_333;
        let mut cadence = ReleaseCadence::new();
        let one: VecDeque<u64> = [1_000_000_000].into();
        assert_eq!(
            cadence.effective_source_multiple(&one, CANVAS),
            1,
            "#726: an inconclusive tick before any confirmation must read 1, never invent N>=2"
        );
    }

    /// #741 (#707 B2) RED→GREEN — `measure_source_multiple` must SCAN past a degenerate front pair.
    ///
    /// The pre-#741 measure read ONLY `array[0]`/`array[1]`, so a DUPLICATE capture stamp at the
    /// front returned INCONCLUSIVE (`None`). Sustained on a jittery 60-into-30 input that dropped
    /// the release to the present-oldest CRAWL — the B2 half of #707 (a window with `uniform=0.481`,
    /// histogram `{1:295,2:407,3:102,7:39}`: +1 steps at half rate, i.e. consecutive ids WERE
    /// present, not an arrival problem). Scanning the first K queued entries for the FIRST
    /// strictly-increasing CONSECUTIVE pair recovers one real source interval past a leading
    /// duplicate, so a fresh measurement re-confirms N instead of falling to the crawl.
    #[test]
    fn measure_scans_past_a_degenerate_front_pair_741() {
        use std::collections::VecDeque;
        const CANVAS: u64 = 33_333_333; // 30 Hz canvas
        const SRC: u64 = 16_666_667; // 60 Hz source (N == 2)
        let base = 1_000_000_000u64;

        // Regression: a clean 60fps front pair still measures N==2 (the fix only WIDENS the search).
        let clean: VecDeque<u64> = [base, base + SRC, base + 2 * SRC].into();
        assert_eq!(
            ReleaseCadence::measure_source_multiple(&clean, CANVAS),
            Some(2),
            "#741: a clean front pair must still measure N==2"
        );

        // DUPLICATE front stamp: pre-#741 array[0..1]-only measure reads t1<=t0 => None (crawl).
        // Post-fix: skip the equal pair, the next consecutive pair (base, base+SRC) = one real
        // source interval => N==2.
        let dup_front: VecDeque<u64> = [base, base, base + SRC, base + 2 * SRC].into();
        assert_eq!(
            ReleaseCadence::measure_source_multiple(&dup_front, CANVAS),
            Some(2),
            "#741 B2: a duplicate front stamp must not read INCONCLUSIVE — scan to the next \
             strictly-increasing consecutive pair for one real source interval (N==2)"
        );

        // A 1:1 (30-into-30) duplicate-led queue must still re-latch to N==1 (never fabricate 2).
        let dup_one_to_one: VecDeque<u64> = [base, base, base + CANVAS, base + 2 * CANVAS].into();
        assert_eq!(
            ReleaseCadence::measure_source_multiple(&dup_one_to_one, CANVAS),
            Some(1),
            "#741: a duplicate-led 1:1 queue must measure N==1, not a fabricated multiple"
        );

        // Genuinely inconclusive — NO strictly-increasing pair anywhere in the scan window: still
        // None. The fix widens the search; it never fabricates a measurement.
        let all_flat: VecDeque<u64> = [base, base, base, base].into();
        assert_eq!(
            ReleaseCadence::measure_source_multiple(&all_flat, CANVAS),
            None,
            "#741: a queue with no strictly-increasing pair in the first K entries stays \
             inconclusive (None) — never fabricate a measurement"
        );

        // Fewer than 2 queued frames stays inconclusive (nothing to compare).
        let one: VecDeque<u64> = [base].into();
        assert_eq!(
            ReleaseCadence::measure_source_multiple(&one, CANVAS),
            None,
            "#741: fewer than 2 queued frames stays inconclusive"
        );
    }

    /// #741 (#707 B2) RED→GREEN — a BACKLOG-STORM relock must NOT clear the sticky-N latch.
    ///
    /// A queue-depth relock (a burst catch-up) is NOT evidence the source RATE changed. Clearing
    /// `last_known_n` there forced the very next INCONCLUSIVE tick to crawl at N==1, which under a
    /// steady 60-into-30 backlog re-grew the queue and re-triggered the relock: a self-sustaining
    /// crawl→relock loop (the #707 B2 crawl window). The latch must SURVIVE a relock; it is cleared
    /// only on a genuine source-timeline discontinuity (acquire / gap resync / backward clock-step).
    #[test]
    fn backlog_relock_preserves_the_confirmed_multiple_741() {
        use std::collections::VecDeque;
        const SRC: u64 = 16_666_667; // 60 Hz source
        const CANVAS: u64 = 33_333_333; // 30 Hz canvas
        let mut cadence = ReleaseCadence::new();

        // Confirm N==2 from a clean 60fps front pair → latches last_known_n = 2.
        let clean: VecDeque<u64> =
            [1_000_000_000, 1_000_000_000 + SRC, 1_000_000_000 + 2 * SRC].into();
        assert_eq!(cadence.effective_source_multiple(&clean, CANVAS), 2);
        assert_eq!(cadence.last_known_n, 2, "setup: N==2 must be latched");

        // Lock the cadence, then feed a genuine BACKLOG STORM (> the backlog threshold in frames, all aged
        // past the reserve so `due > 0`) — the relock branch.
        //
        // #940 piece 2: the threshold this queue must exceed is now
        // steady_depth_frames(...) + QDEPTH_RELOCK_MARGIN * n (n=2 here, a confirmed
        // 60-into-30 source) instead of the pre-#940 bare + QDEPTH_RELOCK_MARGIN — at this
        // fixture's shallow reserve_ms=3, steady_depth_frames rounds to 0, so the threshold
        // is QDEPTH_RELOCK_MARGIN * 2. `* 2 + 3` reliably exceeds it (was `+ 3` pre-#940,
        // when the threshold was the bare QDEPTH_RELOCK_MARGIN).
        cadence.locked_next_boundary_ns = Some(2_000_000_000);
        let base = 3_000_000_000u64;
        let mut queue: VecDeque<u64> = (0..(ReleaseCadence::QDEPTH_RELOCK_MARGIN as u64 * 2 + 3))
            .map(|i| base + i * SRC)
            .collect();
        let wall_now = base + 100 * SRC; // every queued frame is due
        let out = cadence.tick(wall_now, 3, CANVAS, &mut queue);

        assert!(
            out.relocked,
            "#741 setup: the backlog storm must hit the relock branch (got {out:?})"
        );
        assert_eq!(
            cadence.last_known_n, 2,
            "#741 B2: a backlog-storm relock is a queue-depth event, NOT a rate change — it must \
             PRESERVE the confirmed N (2); clearing it re-crawls on the next inconclusive tick and \
             re-triggers the relock (the self-sustaining crawl the fix removes)"
        );
    }

    /// #1003 (issue 1037) RED→GREEN — a BACKLOG relock INHERITS the tracked phase anchor
    /// instead of jumping to the newest due frame.
    ///
    /// This is the demonstrative lock for the whole ticket: the pre-1037 harness presented the
    /// NEWEST due frame at a backlog relock (the live edge), re-minting the release phase on every
    /// lock episode — the instant-sampled defect #1003 removes. With a deep phase anchor tracked
    /// (a ~900 ms conveyor, floored above the shallow 3 ms configured latency), the relock must
    /// present the frame NEAREST `wall_now − anchor` — well BEHIND the live edge — keeping the
    /// deep delay-line depth, and must PRESERVE the anchor (a relock corrects DEPTH, never phase).
    ///
    /// The pinned indices were OBSERVED from a default-feature replica that imports the real
    /// Tier-0 authority (`relock_select_nearest` / `relock_anchor_age_ns`), not guessed. Trace:
    /// interval 33.333 ms, anchor 900 ms ≈ 27 intervals; target = `wall − 900 ms` ≈ frame 12.15,
    /// so the nearest stamp is index 12 (`|12−12.15| < |13−12.15|`). Newest-due would be index 39.
    #[test]
    fn backlog_relock_inherits_the_phase_anchor_not_newest_due_1037() {
        use std::collections::VecDeque;
        const I: u64 = 33_333_333;
        let base = 1_000_000_000_000u64;
        let mut cadence = ReleaseCadence::new();
        cadence.locked_next_boundary_ns = Some(base); // past ACQUIRE
        cadence.last_known_n = 1; // 1:1 source, small backlog qdepth
        cadence.phase_anchor_ns = 900_000_000; // a deep ~900 ms conveyor, tracked from steady
        let n = 40u64;
        let mut queue: VecDeque<u64> = (0..n).map(|i| base + i * I).collect();
        let wall_now = base + (n - 1) * I + 5_000_000; // every queued frame is due

        // The newest-due rule (pre-1037) WOULD have presented the live edge.
        let deadline = genlock_present_ts_reserve(wall_now, 3);
        let newest_due_idx = queue.iter().take_while(|&&ts| ts <= deadline).count() - 1;
        assert_eq!(
            newest_due_idx, 39,
            "setup: newest-due is the live edge (index 39)"
        );

        let out = cadence.tick(wall_now, 3, I, &mut queue);

        assert!(
            out.relocked,
            "#1037: the deep backlog must hit the relock branch"
        );
        assert_eq!(
            out.presented,
            Some(base + 12 * I),
            "#1037: the relock must present the frame NEAREST the 900 ms phase anchor (index 12), \
             NOT the newest due one (index 39 — the live edge). Presenting newest-due re-mints the \
             release phase every episode, the #1003 defect."
        );
        assert_eq!(
            out.dropped.len(),
            12,
            "#1037: exactly the 12 frames OLDER than the selected one are shed (DEPTH correction \
             intact); the ~27-frame conveyor behind index 12 is KEPT"
        );
        assert_eq!(
            queue.len(),
            27,
            "#1037: the ~900 ms/33 ms ≈ 27-frame delay line is preserved"
        );
        assert_eq!(
            cadence.phase_anchor_ns, 900_000_000,
            "#1037: a relock corrects DEPTH, never PHASE — the anchor must be PRESERVED, never \
             re-minted from the frame the relock happened to select (the C anchor_update gate: \
             relocks never write the anchor)"
        );
    }

    /// #1003 (issue 1037) — the phase anchor is UPDATED on STEADY and GAP-RESYNC presents (the
    /// conveyor), and is NEVER written by an ACQUIRE (a lock inherits phase, it does not mint it).
    /// Values observed from the same authority-importing replica.
    #[test]
    fn steady_and_gap_presents_update_the_phase_anchor_acquire_does_not_1037() {
        use std::collections::VecDeque;
        const I: u64 = 33_333_333;
        let base = 1_000_000_000_000u64;
        let mut cadence = ReleaseCadence::new();

        // ACQUIRE presents the head; the anchor stays UNSET (acquire never updates it).
        let mut q: VecDeque<u64> = [base].into_iter().collect();
        let a = cadence.tick(base + 10_000_000, 3, I, &mut q);
        assert_eq!(a.presented, Some(base), "acquire presents the head");
        assert_eq!(
            cadence.phase_anchor_ns, 0,
            "#1037: ACQUIRE must NOT write the phase anchor — a cold lock inherits the \
             configured-latency fallback, it does not define a phase"
        );

        // STEADY present: anchor = wall_now − presented stamp (the conveyor's on-air age).
        q.push_back(base + I);
        let s = cadence.tick(base + I + 10_000_000, 3, I, &mut q);
        assert_eq!(
            s.presented,
            Some(base + I),
            "steady presents the matured frame"
        );
        assert_eq!(
            cadence.phase_anchor_ns, 10_000_000,
            "#1037: a STEADY present updates the anchor to wall_now − presented (10 ms here)"
        );

        // GAP RESYNC (oldest queued frame is beyond the boundary and aged past the reserve):
        // re-derives the anchor from the frame it puts on air, and clears STICKY-N.
        q.push_back(base + 5 * I);
        let g = cadence.tick(base + 5 * I + 15_000_000, 3, I, &mut q);
        assert_eq!(
            g.presented,
            Some(base + 5 * I),
            "gap resync presents the far-ahead frame"
        );
        assert_eq!(
            cadence.phase_anchor_ns, 15_000_000,
            "#1037: a GAP RESYNC RE-DERIVES the anchor from the presented frame (distinct 15 ms \
             value proves it is recomputed, not carried forward from the pre-gap 10 ms)"
        );
        assert_eq!(
            cadence.last_known_n, 0,
            "#726: a gap also clears the STICKY-N latch"
        );
    }

    /// #1003 (issue 1037) — the BACKLOG stale-anchor self-heal: a relock that would shed NOTHING
    /// (selected index 0) with an anchor set is proof the anchor is stale (it cannot describe a
    /// queue THIS deep). The branch clears the anchor and re-selects against the configured
    /// latency, so one relock still sheds the overshoot instead of re-firing every tick forever.
    #[test]
    fn backlog_stale_anchor_self_heals_to_configured_latency_1037() {
        use std::collections::VecDeque;
        const I: u64 = 33_333_333;
        let base = 1_000_000_000_000u64;
        let mut cadence = ReleaseCadence::new();
        cadence.locked_next_boundary_ns = Some(base);
        cadence.last_known_n = 1;
        // An absurd 10 s anchor points BEFORE the whole queue → nearest = index 0 → sheds nothing.
        cadence.phase_anchor_ns = 10_000_000_000;
        let n = 40u64;
        let mut queue: VecDeque<u64> = (0..n).map(|i| base + i * I).collect();
        let wall_now = base + (n - 1) * I + 5_000_000;

        let out = cadence.tick(wall_now, 3, I, &mut queue);

        assert!(out.relocked, "#1037: still a backlog relock");
        assert_eq!(
            out.presented,
            Some(base + 39 * I),
            "#1037: after clearing the stale anchor, the re-selection targets the configured 3 ms \
             latency → the newest due frame (index 39), so the overshoot IS shed this tick"
        );
        assert_eq!(
            out.dropped.len(),
            39,
            "#1037: the whole overshoot is shed in one relock"
        );
        assert_eq!(
            cadence.phase_anchor_ns, 0,
            "#1037: the stale-anchor self-heal CLEARS the anchor (mirror of the C \
             `if (sel_1003 == 0 && genlock_phase_anchor_ns != 0) {{ genlock_phase_anchor_ns = 0; ...}}`) \
             so it rebuilds from the next STEADY present rather than re-firing this branch every tick"
        );
    }

    /// #401 — a genuine upstream STALL must still catch up (the IMAG latency contract):
    /// after a 30-frame arrival gap the cadence re-locks near the live edge and counts the
    /// jumped frames HONESTLY in `dropped` (never silently).
    #[test]
    fn cadence_relocks_after_stall_and_counts_dropped() {
        const I: u64 = 16_666_667;
        const BASE: u64 = 1_000_000_000_000;
        let mut cadence = ReleaseCadence::new();
        let mut queue: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut dropped_total = 0usize;
        let mut relocked = false;
        // 100 steady frames, then a stall: frames 100..130 all arrive AT ONCE (burst) at
        // tick 135, then steady again to 200.
        let mut presented_after_burst = Vec::new();
        for k in 0..240u64 {
            let wall = BASE + k * I;
            let arrive_upto = if k < 100 {
                k.saturating_sub(1) // steady: frame k-1 arrived (skew ~1 tick)
            } else if k < 135 {
                99 // stall — nothing new arrives
            } else {
                (k - 1).min(199) // burst at 135 delivers 100..=134-1 at once, then steady
            };
            while queue.len() < 200 {
                let next = queue.back().map(|&t| (t - BASE) / I + 1).unwrap_or(0);
                if next > arrive_upto {
                    break;
                }
                queue.push_back(BASE + next * I);
            }
            let out = cadence.tick(wall, 25, I, &mut queue);
            dropped_total += out.dropped.len();
            relocked |= out.relocked;
            if k >= 135 {
                if let Some(ts) = out.presented {
                    presented_after_burst.push(ts);
                }
            }
        }
        assert!(relocked, "a 35-tick stall+burst must trigger a re-lock");
        assert!(
            dropped_total > 0,
            "catch-up must drop the stale backlog — and COUNT it (silent drops are the \
             pre-#401 bug)"
        );
        // After the burst the cadence rides the live edge again: the last steady frames
        // present in order.
        let tail: Vec<u64> = presented_after_burst
            .iter()
            .rev()
            .take(20)
            .cloned()
            .collect();
        let mut tail_sorted = tail.clone();
        tail_sorted.sort_unstable();
        tail_sorted.reverse();
        assert_eq!(tail, tail_sorted, "post-relock presentation stays ordered");
    }

    /// #401/#136 — two sources locked at DIFFERENT ticks must present the SAME stamp
    /// sequence afterward (multi-camera in-sync): the lock phase comes from the shared
    /// stamp grid, not from the lock instant.
    #[test]
    fn cadence_multi_source_presents_identical_boundaries() {
        const I: u64 = 16_666_667;
        const BASE: u64 = 1_000_000_000_000;
        let mut a = ReleaseCadence::new();
        let mut b = ReleaseCadence::new();
        let mut qa: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut qb: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut pa = Vec::new();
        let mut pb = Vec::new();
        for k in 0..200u64 {
            let wall = BASE + k * I;
            // Source A delivers from the start; source B's frames only start arriving at
            // tick 50 (activated later) — same grid, same 20 ms skew.
            if k >= 1 {
                qa.push_back(BASE + (k - 1) * I);
            }
            if k >= 50 {
                qb.push_back(BASE + (k - 1) * I);
            }
            if let Some(ts) = a.tick(wall, 25, I, &mut qa).presented {
                pa.push((k, ts));
            }
            if let Some(ts) = b.tick(wall, 25, I, &mut qb).presented {
                pb.push((k, ts));
            }
        }
        // On every tick where BOTH presented, the stamps must be IDENTICAL (in-sync).
        let map_a: std::collections::HashMap<u64, u64> = pa.into_iter().collect();
        let mut both = 0;
        for (k, ts_b) in pb {
            if let Some(&ts_a) = map_a.get(&k) {
                assert_eq!(ts_a, ts_b, "tick {k}: sources must present the same stamp");
                both += 1;
            }
        }
        assert!(
            both > 100,
            "expected a long overlapping steady window, got {both}"
        );
    }

    /// #402 REGRESSION LOCK — "ts-align cadence: large backward clock step under a deep
    /// reserve collapses held latency at the pre/post-step seam". As filed, the trace
    /// described cadence v1's STEADY path: once the pre-step (high-stamped) backlog is
    /// drained by the #147 re-anchor, the newly-queued post-step (low-stamped) frames all
    /// satisfy `ts ≤ boundary` against the still-HIGH locked boundary, so v1 presented the
    /// NEWEST of that set and dropped the rest (`matured − 1` into `dropped_due`) —
    /// collapsing the held reserve straight to the live edge with nothing to restore it.
    /// v2 (cc815e73e, landed ~30 min after this issue was filed) rewrote STEADY to release
    /// the OLDEST matured frame — exactly ONE per tick — regardless of how many satisfy
    /// `ts ≤ boundary`. That means the seam drains the DEEP backlog that built up while the
    /// pre-step tail was still presenting (one frame per tick, same as always), not the
    /// live edge. This test proves it: fill a 450 ms reserve (~27 buffered frames), step
    /// the clock backward by 500 ms (> one interval — the #147 trigger) mid-stream, and
    /// assert every presented frame's held latency (`wall_now − presented_ts`) stays within
    /// a few frame intervals of the 450 ms reserve on BOTH sides of the seam — it must
    /// never collapse toward the ~skew-only live edge.
    #[test]
    fn cadence_holds_reserve_latency_across_a_backward_clock_step() {
        const I_NS: i64 = 16_666_667;
        const RESERVE_MS: u32 = 450;
        const RESERVE_NS: i64 = RESERVE_MS as i64 * 1_000_000;
        const STEP_FRAME_IDX: u64 = 250; // deep into steady state (locks by ~tick 27)
        const STEP_NS: u64 = 500_000_000; // 500 ms > one interval — the #147 trigger
        const N_FRAMES: u64 = 400;

        let results = run_cadence_sim_backward_step(
            RESERVE_MS,
            20_000_000,
            STEP_FRAME_IDX,
            STEP_NS,
            N_FRAMES,
        );

        // Sanity: PRE-step steady state actually holds ≈ reserve — proves the harness
        // built a genuinely deep buffered backlog before asserting anything about the
        // step (ticks 50..200 are well past ACQUIRE's settle-in and well before the step
        // at tick 250).
        let pre_step: Vec<i64> = results
            .iter()
            .filter(|&&(k, _)| (50..200).contains(&k))
            .map(|&(_, held)| held)
            .collect();
        assert!(
            pre_step.len() > 100,
            "expected a long pre-step steady window, got {}",
            pre_step.len()
        );
        for &held in &pre_step {
            assert!(
                (held - RESERVE_NS).abs() <= 3 * I_NS,
                "pre-step steady held latency {held} ns should be ≈ reserve {RESERVE_NS} ns \
                 (harness sanity check, BEFORE the step)"
            );
        }

        // The #402 claim: held latency must stay ≈ reserve on the far side of the seam
        // too — never collapse toward the live edge (tens-of-ms arrival skew). Ticks
        // ≥340 are well past the seam (pre-step backlog drains by ~tick 277) and well
        // clear of the run's tail-off (frames exhaust around tick 426).
        let post_step: Vec<i64> = results
            .iter()
            .filter(|&&(k, _)| k >= N_FRAMES.saturating_sub(60))
            .map(|&(_, held)| held)
            .collect();
        assert!(
            post_step.len() > 30,
            "expected a long post-step steady window, got {}",
            post_step.len()
        );
        for &held in &post_step {
            assert!(
                (held - RESERVE_NS).abs() <= 3 * I_NS,
                "post-step held latency {held} ns collapsed away from the {RESERVE_NS} ns \
                 reserve (#402: the seam must not throw the buffered depth away)"
            );
        }
    }

    // ---- #278 multiview ADAPTIVE budget-based skip --------------------------
    //
    // Supersedes the #276 fixed every-Nth-frame cadence: the skip decision is now driven by
    // the display's measured render cost (EWMA) vs the budget REMAINING after the program has
    // already rendered this tick — so the program NEVER skips, no matter how heavy the
    // monitoring display is. A 60fps frame interval is 16,666,666 ns; the 90% budget is
    // 16,666,666 − 16,666,666/10 = 15,000,000 ns (15.0 ms).

    const I60: u64 = 16_666_666; // 60fps frame interval (ns)
    const BUDGET60: u64 = I60 - I60 / 10; // 90% safety margin = 15,000,000 ns

    #[test]
    fn program_and_preview_displays_are_never_throttled() {
        // Program output + preview never set a divisor (bzalloc → 0/1). They MUST render
        // EVERY frame regardless of elapsed/ewma/interval — the program is sacred.
        for divisor in [0u32, 1] {
            // even with an absurd elapsed + ewma that would skip a monitoring display:
            assert!(
                !display_render_skip_budget(divisor, I60, I60 * 10, I60),
                "divisor {divisor} (program/preview) must NEVER be skipped"
            );
        }
    }

    #[test]
    fn heavy_monitoring_display_with_no_slack_is_skipped() {
        // The #278 regression: a 4-live-cam multiview render (~18ms EWMA) added to the ~4.3ms
        // the program already consumed this tick (elapsed) exceeds the 15ms budget → the
        // monitoring display MUST be skipped so the program does not overrun and renderSkip.
        let elapsed = 4_340_000; // program already rendered ~4.34ms this tick
        let ewma = 18_000_000; // multiview's measured ~18ms render
        assert!(
            elapsed + ewma > BUDGET60,
            "test premise: this case has no slack"
        );
        assert!(
            display_render_skip_budget(2, elapsed, ewma, I60),
            "a heavy monitoring display with no remaining budget MUST be skipped (else the \
             program renderSkips — the exact #278 bug)"
        );
    }

    #[test]
    fn monitoring_display_renders_when_there_is_slack() {
        // When the monitoring display's cost DOES fit the remaining budget it MUST render —
        // it is never starved when there is genuine slack.
        let elapsed = 4_000_000; // 4.0ms program
        let ewma = 8_000_000; // 8.0ms light multiview → 12.0ms total < 15ms budget
        assert!(
            elapsed + ewma <= BUDGET60,
            "test premise: this case has slack"
        );
        assert!(
            !display_render_skip_budget(2, elapsed, ewma, I60),
            "a monitoring display whose cost fits the remaining budget MUST render"
        );
    }

    #[test]
    fn cold_monitoring_display_renders_to_measure_never_starved() {
        // EWMA not warmed up (==0) → render once to measure, never skip-forever before we
        // even know the cost. Same for a zero interval (no timing yet).
        assert!(
            !display_render_skip_budget(2, I60, 0, I60),
            "ewma==0 (cold) must render to measure — never starved to 0"
        );
        assert!(
            !display_render_skip_budget(2, I60, I60, 0),
            "interval==0 (no timing) must render — never starved to 0"
        );
    }

    #[test]
    fn budget_boundary_is_inclusive_render_exclusive_skip() {
        // Exactly AT the 90% budget → render (<=). One ns OVER → skip (>). Matches the C
        // `elapsed + ewma > budget`.
        assert!(
            !display_render_skip_budget(2, 0, BUDGET60, I60),
            "elapsed+ewma == budget → render (not skip)"
        );
        assert!(
            display_render_skip_budget(2, 0, BUDGET60 + 1, I60),
            "elapsed+ewma one ns over budget → skip"
        );
    }

    #[test]
    fn ewma_update_seeds_cold_then_converges_toward_load() {
        // Cold (prev==0) seeds with the first measurement.
        assert_eq!(
            display_render_ewma_update(0, 18_000_000),
            18_000_000,
            "cold EWMA seeds with the first measured duration"
        );
        // α=1/4: from 8ms, a heavy 20ms frame nudges the average UP toward the load, smoothed.
        let next = display_render_ewma_update(8_000_000, 20_000_000);
        assert_eq!(
            next,
            (8_000_000 * 3 + 20_000_000) / 4,
            "EWMA = (prev*3 + dur)/4"
        );
        assert!(
            next > 8_000_000 && next < 20_000_000,
            "EWMA moves toward, not to, the new load"
        );
        // Repeated heavy frames converge the EWMA up across the budget so the gate learns the
        // display is over-budget and starts skipping (the #278 self-throttle).
        let mut e = 8_000_000u64;
        for _ in 0..12 {
            e = display_render_ewma_update(e, 20_000_000);
        }
        assert!(
            e > BUDGET60,
            "sustained heavy load drives the EWMA over budget → gate skips"
        );
    }

    // ---- #136: timestamp-aligned release (multi-source IN-SYNC) -------------

    const NS30: u64 = 33_333_333; // ~one 30fps frame interval in ns
    const WBASE: u64 = WALLCLOCK_TS_MIN_NS + 1_000_000_000; // a plausible wall-clock base

    /// `n` frames captured at a steady 30fps cadence from `WBASE` (oldest-first).
    fn caps(n: usize) -> Vec<u64> {
        (0..n).map(|i| WBASE + i as u64 * NS30).collect()
    }

    #[test]
    fn release_steady_presents_one_drops_none() {
        // Steady state at matched rates: each tick drops the presented frame, so the
        // queue HEAD is the next due frame and the rest are future. Exactly one frame
        // is due → present it, drop nothing.
        let q = caps(6); // head q[0]=WBASE is due; q[1..] are future
        let present_ts = WBASE; // only the head has ts <= deadline
        let d = genlock_release(present_ts, &q);
        assert_eq!(
            d,
            GenlockRelease {
                drop_oldest: 0,
                present: true
            },
            "with only the due frame at the head, present it and drop nothing"
        );
        assert_eq!(
            q[d.drop_oldest], present_ts,
            "the presented frame is the due head"
        );
    }

    #[test]
    fn release_drops_stale_when_render_pass_lagged() {
        // The rate-leak / lagged-render case: present_ts jumped two intervals, so TWO
        // frames are now past-due. Present the NEWEST, drop the stale one — a uniform
        // controlled drop, NOT an unbounded FIFO fill (the depth-FIFO failure #136 fixes).
        let q = caps(6); // indices 0..5 due-able
        let present_ts = WBASE + 4 * NS30; // indices 0..=4 are due (5 frames)
        let d = genlock_release(present_ts, &q);
        assert_eq!(
            d,
            GenlockRelease {
                drop_oldest: 4,
                present: true
            },
            "five frames past-due → drop the four stale, present the newest"
        );
        // the presented frame (queue[drop_oldest]) is the one captured at present_ts
        assert_eq!(q[d.drop_oldest], present_ts);
    }

    #[test]
    fn release_holds_when_source_early_or_empty() {
        // Source ahead: every queued frame is in the future relative to the deadline.
        let q = caps(4); // ts >= WBASE
        let present_ts = WBASE - 1; // nothing due yet
        assert_eq!(
            genlock_release(present_ts, &q),
            GenlockRelease {
                drop_oldest: 0,
                present: false
            },
            "no frame at/before the deadline → hold (repeat last), drop nothing"
        );
        // Source empty (stalled): nothing to present.
        assert_eq!(
            genlock_release(WBASE + 100 * NS30, &[]),
            GenlockRelease {
                drop_oldest: 0,
                present: false
            }
        );
    }

    #[test]
    fn release_keeps_two_sources_in_sync_despite_different_depths() {
        // THE core property (#136). Two genlock cams capture at the SAME wall-clock
        // cadence but sit at DIFFERENT FIFO depths (the exact desync cause under the
        // old count gate: cam A shallow=5, cam B deep=20). At ONE shared presentation
        // deadline, both present the frame with the IDENTICAL capture timestamp →
        // in-sync BY CONSTRUCTION, independent of buffered depth.
        let a = caps(5);
        let b = caps(20);
        let present_ts = WBASE + 3 * NS30;
        let da = genlock_release(present_ts, &a);
        let db = genlock_release(present_ts, &b);
        assert!(da.present && db.present, "both have a due frame");
        let pres_a = a[da.drop_oldest];
        let pres_b = b[db.drop_oldest];
        assert_eq!(
            pres_a, pres_b,
            "different depths must present the SAME capture instant"
        );
        assert_eq!(
            pres_a, present_ts,
            "the presented frame is the one captured at the deadline"
        );
    }

    #[test]
    fn present_ts_subtracts_the_common_delay_with_half_interval_bias() {
        // present_ts = tick_wall - delay_frames*interval + interval/2 (saturating).
        // The +interval/2 is the #136 boundary-churn tolerance.
        let tick = WBASE + 100 * NS30;
        assert_eq!(
            genlock_present_ts(tick, 6, NS30),
            tick - 6 * NS30 + NS30 / 2
        );
        // sub saturates to 0, then the half-interval bias is added.
        assert_eq!(
            genlock_present_ts(NS30, 6, NS30),
            NS30 / 2,
            "saturates, never wraps below 0"
        );
    }

    // ---- #184: sub-frame MS-granular jitter reserve --------------------------

    #[test]
    fn parse_reserve_ms_default_when_unset_or_invalid() {
        // Unset / empty / whitespace / junk / negative ⇒ DEFAULT (0 = disabled).
        assert_eq!(parse_reserve_ms(None), GENLOCK_RESERVE_MS_DEFAULT);
        assert_eq!(parse_reserve_ms(Some("")), GENLOCK_RESERVE_MS_DEFAULT);
        assert_eq!(parse_reserve_ms(Some("   ")), GENLOCK_RESERVE_MS_DEFAULT);
        assert_eq!(parse_reserve_ms(Some("abc")), GENLOCK_RESERVE_MS_DEFAULT);
        assert_eq!(parse_reserve_ms(Some("3x")), GENLOCK_RESERVE_MS_DEFAULT);
        assert_eq!(parse_reserve_ms(Some("-1")), GENLOCK_RESERVE_MS_DEFAULT);
        // strtol quirk parity with parse_preload: trailing space ⇒ default; leading ok; +ok.
        assert_eq!(parse_reserve_ms(Some("5 ")), GENLOCK_RESERVE_MS_DEFAULT);
        assert_eq!(parse_reserve_ms(Some("  2")), 2);
        assert_eq!(parse_reserve_ms(Some("+3")), 3);
        assert_eq!(
            GENLOCK_RESERVE_MS_DEFAULT, 0,
            "0 = disabled, whole-frame path"
        );
    }

    #[test]
    fn parse_reserve_ms_valid_and_clamped() {
        assert_eq!(parse_reserve_ms(Some("0")), 0); // explicit disabled
        assert_eq!(parse_reserve_ms(Some("2")), 2); // strih→stream jitter ~1.6ms + margin
        assert_eq!(parse_reserve_ms(Some("10")), 10); // cam1→strih jitter ~8.1ms + margin
        assert_eq!(GENLOCK_RESERVE_MS_MAX, 100);
        assert_eq!(parse_reserve_ms(Some("100")), 100);
        assert_eq!(parse_reserve_ms(Some("101")), GENLOCK_RESERVE_MS_MAX); // clamped
        assert_eq!(parse_reserve_ms(Some("99999")), GENLOCK_RESERVE_MS_MAX); // overflow path
    }

    #[test]
    fn present_ts_reserve_is_a_pure_ms_delay_no_frame_quantization() {
        // The deadline = wall - reserve_ms (in ns). NO frame quantization, NO
        // +interval/2 bias — the held latency is EXACTLY reserve_ms.
        let wall = WBASE + 100 * NS30;
        assert_eq!(genlock_present_ts_reserve(wall, 3), wall - 3_000_000);
        assert_eq!(
            genlock_present_ts_reserve(wall, 0),
            wall,
            "0 reserve = no delay"
        );
        // saturates, never wraps below 0.
        assert_eq!(genlock_present_ts_reserve(1_000_000, 5), 0);
    }

    #[test]
    fn present_ts_reserve_beats_a_whole_frame_preload() {
        // THE #184 win: at 30fps one preload frame is 33.3ms; the frame-based deadline
        // holds (33.3 − 16.7) = 16.6ms effective. A 3ms reserve holds only 3ms — far
        // less held latency, while still covering the measured ~1.6ms strih→stream jitter.
        let wall = WBASE + 100 * NS30;
        let held_frame = wall - genlock_present_ts(wall, 1, NS30); // effective frame-path delay
        let held_reserve = wall - genlock_present_ts_reserve(wall, 3); // reserve-path delay
        assert_eq!(held_reserve, 3_000_000, "reserve holds exactly 3ms");
        assert!(
            held_reserve < held_frame,
            "a 3ms reserve ({held_reserve}ns) must hold LESS than one preload frame ({held_frame}ns)"
        );
    }

    #[test]
    fn release_under_reserve_holds_each_frame_for_exactly_the_reserve() {
        // A frame is due only once it has aged >= reserve_ms. With a 3ms reserve and
        // frames captured at the steady cadence, the head is due exactly when
        // wall >= head_ts + 3ms.
        let reserve_ms = 3u32;
        let q = caps(6); // head q[0] = WBASE
                         // wall just BEFORE the head has aged 3ms → not yet due → hold.
        let wall_early = WBASE + 3_000_000 - 1;
        assert!(
            !genlock_release(genlock_present_ts_reserve(wall_early, reserve_ms), &q).present,
            "head not yet aged the reserve → hold (no premature present)"
        );
        // wall once the head has aged exactly 3ms → due → present it, drop nothing.
        let wall_due = WBASE + 3_000_000;
        let d = genlock_release(genlock_present_ts_reserve(wall_due, reserve_ms), &q);
        assert!(
            d.present && d.drop_oldest == 0,
            "head aged the reserve → present it"
        );
        assert_eq!(
            q[d.drop_oldest], WBASE,
            "the presented frame is the reserve-aged head"
        );
    }

    #[test]
    fn release_under_reserve_keeps_two_sources_in_sync() {
        // The #136 in-sync invariant must survive the reserve path: both sources share
        // present_ts = wall - reserve, so when both have captured up to the deadline they
        // present the SAME capture instant regardless of buffered depth. Both queues must
        // CONTAIN the due frame (in-sync is about the shared deadline, not about a source
        // that is simply behind) — so choose a wall where the newest due frame is one both
        // caps(5) and caps(20) hold.
        let reserve_ms = 5u32;
        let a = caps(5); // newest = WBASE + 4*NS30
        let b = caps(20);
        // Land the deadline a hair after WBASE + 3*NS30 so the newest due frame is index 3
        // (present in BOTH queues); reserve_ms is the offset from `wall` to that deadline.
        let deadline = WBASE + 3 * NS30;
        let wall = deadline + (reserve_ms as u64) * 1_000_000;
        let present_ts = genlock_present_ts_reserve(wall, reserve_ms);
        assert_eq!(
            present_ts, deadline,
            "reserve path lands the shared deadline at wall-reserve"
        );
        let da = genlock_release(present_ts, &a);
        let db = genlock_release(present_ts, &b);
        assert!(da.present && db.present, "both have a due frame");
        assert_eq!(
            a[da.drop_oldest], b[db.drop_oldest],
            "different depths must present the SAME capture instant under the reserve path"
        );
        assert_eq!(
            a[da.drop_oldest], deadline,
            "presented frame is the one captured at the deadline"
        );
    }

    #[test]
    fn release_no_churn_for_a_boundary_landing_frame() {
        // #136 boundary-churn regression guard. A frame captured EXACTLY on the nominal
        // deadline (wall - preload*interval) must be robustly DUE even when wall_now is a
        // hair before the boundary (render-tick slew) — otherwise it alternates hold/drop
        // tick-to-tick (the ~3 fps stream churn). With the half-interval bias it is due.
        let interval = NS30;
        let wall = WBASE + 100 * interval;
        let q = vec![wall - interval, wall]; // head captured at the nominal deadline (preload=1)
        assert!(
            genlock_release(genlock_present_ts(wall, 1, interval), &q).present,
            "boundary-landing frame must be due (no hold churn)"
        );
        // wall a hair BEFORE the boundary (−2ms slew): WITHOUT the bias this would HOLD;
        // with the half-interval bias it stays due.
        let slewed = wall - 2_000_000;
        assert!(
            genlock_release(genlock_present_ts(slewed, 1, interval), &q).present,
            "still due under −2ms render-tick slew (the fix kills the boundary jitter)"
        );
    }

    #[test]
    fn wallclock_guard_accepts_real_ts_rejects_garbage() {
        assert!(
            is_wallclock_ts(WBASE),
            "a real DanteSync wall-clock ts is accepted"
        );
        assert!(
            !is_wallclock_ts(0),
            "0 (no timecode) is rejected → count-gate fallback"
        );
        assert!(
            !is_wallclock_ts(NS30),
            "a small monotonic-style ts is rejected"
        );
        assert!(
            !is_wallclock_ts(WALLCLOCK_TS_MAX_NS),
            "upper bound is exclusive"
        );
    }

    // ---- #126: reconnect re-arm (sustained-empty → rebuild) ----------------

    #[test]
    fn rearm_unit_fires_only_after_sustained_empty_in_steady_state() {
        // Re-arm fires when the source is in STEADY state (filled) AND it has just
        // resumed after a sustained true-empty run >= the threshold.
        assert!(
            genlock_rearm_on_resume(GENLOCK_REARM_EMPTY_TICKS, true),
            "an empty run AT the threshold while filled must re-arm"
        );
        assert!(
            genlock_rearm_on_resume(GENLOCK_REARM_EMPTY_TICKS + 50, true),
            "an empty run past the threshold while filled must re-arm"
        );
    }

    #[test]
    fn rearm_unit_brief_empty_below_threshold_never_rearms() {
        // The jitter-safety guard: a brief empty (1..threshold-1) must NOT re-arm —
        // a spurious re-arm would force a ~preload-frame rebuild hold on every blip,
        // catastrophic at the shallow cam preload=1.
        for run in 0..GENLOCK_REARM_EMPTY_TICKS {
            assert!(
                !genlock_rearm_on_resume(run, true),
                "empty run {run} < threshold {GENLOCK_REARM_EMPTY_TICKS} must NOT re-arm"
            );
        }
    }

    #[test]
    fn rearm_unit_never_fires_while_building() {
        // While !filled the FIFO is already in the build phase rebuilding the reserve;
        // re-arming again is a no-op concept — never fire when not filled, at ANY run.
        for run in [
            0u32,
            1,
            GENLOCK_REARM_EMPTY_TICKS,
            GENLOCK_REARM_EMPTY_TICKS + 100,
        ] {
            assert!(
                !genlock_rearm_on_resume(run, false),
                "must never re-arm while !filled (run {run})"
            );
        }
    }

    #[test]
    fn rearm_threshold_is_safely_above_jitter() {
        // ~1 s @ 30 fps. Must be far above any realistic single-dip jitter so normal
        // operation NEVER re-arms (only a real disconnect sustains empties this long).
        assert_eq!(GENLOCK_REARM_EMPTY_TICKS, 30);
    }

    #[test]
    fn rearm_empty_run_resets_on_any_consume() {
        // The empty-run counter must reset to 0 whenever a distinct frame is consumed
        // (modelled by genlock_empty_run_next: a consume zeroes it, a true-empty +1).
        assert_eq!(genlock_empty_run_next(5, true), 0, "consume resets the run");
        assert_eq!(
            genlock_empty_run_next(5, false),
            6,
            "true-empty increments the run"
        );
        assert_eq!(
            genlock_empty_run_next(0, false),
            1,
            "first empty after a consume"
        );
        // Saturates — never wraps on a very long disconnect.
        assert_eq!(genlock_empty_run_next(u32::MAX, false), u32::MAX);
    }

    #[test]
    fn build_drain_unit_trims_to_target() {
        // At the build latch (depth > preload) drain down to target = preload+1.
        assert_eq!(genlock_build_drain(2, 1), 0); // depth == target → 0
        assert_eq!(genlock_build_drain(6, 1), 4); // deep burst → trim 4 to reach 2
        assert_eq!(genlock_build_drain(31, 30), 0); // target
        assert_eq!(genlock_build_drain(41, 30), 10);
    }

    #[test]
    fn build_drain_unit_zero_while_building_or_at_target() {
        // Below the latch (still building, depth <= preload): nothing to drain.
        for depth in 0..=30usize {
            assert_eq!(genlock_build_drain(depth, 30), 0);
        }
        // Exactly at target (preload+1): the latch instant, no trim.
        assert_eq!(genlock_build_drain(31, 30), 0);
    }

    #[test]
    fn build_drain_unit_never_underflows() {
        // A queue at or below target never produces a negative/wrapped drain.
        assert_eq!(genlock_build_drain(0, 0), 0);
        assert_eq!(genlock_build_drain(1, 0), 0); // target for preload 0 is 1
        assert_eq!(genlock_build_drain(0, 128), 0);
    }

    #[test]
    fn build_drain_unit_equals_depth_minus_target_above_latch() {
        // The exact contract: drain = depth - steady_state_depth(preload) when
        // depth > steady_state_depth, else 0.
        for preload in [0u32, 1, 2, 30, 128] {
            let target = steady_state_depth(preload) as usize;
            for depth in 0..target + 20 {
                let expected = depth.saturating_sub(target);
                assert_eq!(
                    genlock_build_drain(depth, preload),
                    expected,
                    "preload={preload} depth={depth}"
                );
            }
        }
    }

    // ---- #99 point 2: peak depth must include the PRODUCER-side high-water mark ----

    #[test]
    fn peak_update_takes_the_higher_of_prev_and_observed() {
        // The audit high-water mark only ever GROWS toward the true maximum: a freshly
        // observed depth above the running peak raises it; a lower one leaves it.
        assert_eq!(
            genlock_peak_update(0, 5),
            5,
            "first observation sets the peak"
        );
        assert_eq!(
            genlock_peak_update(5, 8),
            8,
            "a higher depth raises the peak"
        );
        assert_eq!(
            genlock_peak_update(8, 3),
            8,
            "a lower depth leaves the peak"
        );
        assert_eq!(
            genlock_peak_update(8, 8),
            8,
            "an equal depth leaves the peak"
        );
    }

    #[test]
    fn peak_captures_a_producer_burst_that_drains_before_the_next_tick() {
        // THE #99 point-2 BUG: a CONSUMER-side-only peak under-reports. Model the timeline:
        // the producer pushes the FIFO up to depth 6 between render ticks (its high-water
        // mark), then the consumer drains it back to 1 before the next render tick observes
        // the queue. If peak is folded ONLY at the consumer-observed depth (1), it records 1
        // and the operator never sees how close the queue got to the cap. Folding the
        // PRODUCER-observed depth (6) at the push site captures the true 6.
        let mut peak = 0u32;

        // --- producer pushes 6 frames between ticks (each push observes the new depth) ---
        for depth_after_push in 1..=6u32 {
            peak = genlock_peak_update(peak, depth_after_push);
        }
        // --- consumer drains to 1 by the next render tick; only THAT depth is consumer-seen ---
        let consumer_observed_depth = 1u32;
        peak = genlock_peak_update(peak, consumer_observed_depth);

        assert_eq!(
            peak, 6,
            "peak must reflect the producer-side high-water mark (6), not just the \
             consumer-observed depth (1) — the #99 point-2 under-report"
        );
    }

    // ---- #200: tear-checked fps snapshot (value-seqlock acceptance) -----------

    #[test]
    fn fps_pair_accepted_only_when_two_reads_agree() {
        // Two back-to-back snapshots that AGREE are the consistent pair → accept it.
        assert_eq!(
            genlock_fps_pair_consistent((30000, 1001), (30000, 1001)),
            Some((30000, 1001))
        );
        assert_eq!(genlock_fps_pair_consistent((30, 1), (30, 1)), Some((30, 1)));
        // A consistent zero pair is still accepted; the CALLER guards fps_num==0.
        assert_eq!(genlock_fps_pair_consistent((0, 0), (0, 0)), Some((0, 0)));
    }

    #[test]
    fn fps_pair_rejected_on_a_torn_read() {
        // obs_reset_video tore the pair between the two reads → reject (None), so the C
        // caller takes the fps-unknown branch instead of formatting a mismatched ms.
        // den changed (num matched):
        assert_eq!(genlock_fps_pair_consistent((30000, 1001), (30000, 1)), None);
        // num changed (den matched):
        assert_eq!(
            genlock_fps_pair_consistent((30000, 1001), (60000, 1001)),
            None
        );
        // both changed (a full fps switch mid-read):
        assert_eq!(genlock_fps_pair_consistent((30000, 1001), (60, 1)), None);
        // the classic tear: old num + new den vs new num + old den.
        assert_eq!(genlock_fps_pair_consistent((30000, 1), (60000, 1001)), None);
    }

    // ---- #148: ts-align HOLD vs underrun split -------------------------------

    #[test]
    fn source_early_hold_is_not_an_underrun() {
        // THE #148 split: frames ARE queued but none is due yet (the ts-align deadline
        // is ahead of the head frame's capture ts) → a BENIGN source-early HOLD, which
        // increments genlock_holds — NOT genlock_underruns. This is the case the
        // pre-#148 C code mis-counted as an underrun, hiding the #136 churn.
        assert_eq!(classify_ts_align_tick(1, 0), GenlockTick::SourceEarlyHold);
        assert_eq!(classify_ts_align_tick(5, 0), GenlockTick::SourceEarlyHold);
        assert_eq!(classify_ts_align_tick(30, 0), GenlockTick::SourceEarlyHold);
    }

    #[test]
    fn due_frame_is_a_present() {
        // Any due frame (due >= 1) presents this tick — neither a hold nor an underrun.
        assert_eq!(classify_ts_align_tick(1, 1), GenlockTick::Present);
        assert_eq!(classify_ts_align_tick(6, 4), GenlockTick::Present);
    }

    #[test]
    fn empty_queue_is_a_real_underrun_not_a_hold() {
        // The split's other side: an EMPTY FIFO (depth 0) is a real starvation →
        // genlock_underruns. (Not reached on the ts-align path, which is entered with
        // num>=1, but the classifier must keep the two categories distinct.)
        assert_eq!(classify_ts_align_tick(0, 0), GenlockTick::Underrun);
    }

    #[test]
    fn classification_matches_genlock_release_on_a_non_empty_queue() {
        // Cross-check against the release decision: with frames queued, `present==false`
        // from genlock_release is EXACTLY the SourceEarlyHold case, and `present==true`
        // is Present — so the audit counter split tracks the real release outcome.
        let q = caps(5);
        let early = genlock_release(WBASE - 1, &q); // nothing due → hold
        assert!(!early.present);
        assert_eq!(
            classify_ts_align_tick(q.len(), 0),
            GenlockTick::SourceEarlyHold
        );
        let present = genlock_release(WBASE + 2 * NS30, &q); // some due → present
        assert!(present.present);
        let due = present.drop_oldest + 1; // due count = dropped stale + the presented one
        assert_eq!(classify_ts_align_tick(q.len(), due), GenlockTick::Present);
    }

    // ---- #269 review: cached last-good fps (findings [0]/[1]/[2]) -----------

    #[test]
    fn fps_cached_agreement_updates_cache_and_returns_pair() {
        // Two agreeing reads accept the fresh pair AND publish it to the cache.
        let (c, r) = genlock_fps_cached(None, (30000, 1001), (30000, 1001));
        assert_eq!(r, Some((30000, 1001)));
        assert_eq!(c, Some((30000, 1001)));
        // A later agreement on a NEW pair refreshes the cache.
        let (c2, r2) = genlock_fps_cached(c, (60, 1), (60, 1));
        assert_eq!(r2, Some((60, 1)));
        assert_eq!(c2, Some((60, 1)));
    }

    #[test]
    fn fps_cached_tear_after_good_read_returns_cached_pair() {
        // #269 [0]/[1]/[2]: once a good pair was cached, a torn read must NOT collapse to
        // the fps-unknown branch — it returns the cached last-good pair (so drop_cap keeps
        // its deep-latency frames and frame_interval stays nonzero / ts-align stays engaged).
        let cache = Some((30000, 1001));
        let (c, r) = genlock_fps_cached(cache, (30000, 1001), (30000, 1)); // torn den
        assert_eq!(
            r,
            Some((30000, 1001)),
            "a tear must return the cached good pair"
        );
        assert_eq!(c, cache, "the cache is unchanged on a tear");
    }

    #[test]
    fn fps_cached_tear_with_no_cache_rejects() {
        // The ONLY reject: a tear before any good pair was ever read (first-ever call
        // mid-tear) → the fps-unknown fallback.
        let (c, r) = genlock_fps_cached(None, (30000, 1001), (60000, 1001));
        assert_eq!(r, None);
        assert_eq!(c, None);
    }

    #[test]
    fn fps_cached_degenerate_agreement_not_cached() {
        // (0, _) is agreed but is not a GOOD fps → returned to the caller but never cached
        // (it must not overwrite a good cache), mirroring the C `a.fps_num != 0` guard.
        let (c, r) = genlock_fps_cached(Some((30, 1)), (0, 0), (0, 0));
        assert_eq!(r, Some((0, 0)));
        assert_eq!(
            c,
            Some((30, 1)),
            "a degenerate agreement must not clobber a good cache"
        );
    }

    // ---- #269 finding [4]: count-gate build-fill is a HOLD, not an underrun ----

    #[test]
    fn count_gate_build_fill_is_a_hold_not_an_underrun() {
        // The count gate is entered only with queue_depth>=1; a non-consume tick there is a
        // BENIGN build-fill HOLD → genlock_holds, NEVER genlock_underruns.
        assert_eq!(
            classify_count_gate_tick(1, false),
            GenlockTick::SourceEarlyHold
        );
        assert_eq!(
            classify_count_gate_tick(3, false),
            GenlockTick::SourceEarlyHold
        );
    }

    #[test]
    fn count_gate_consume_is_present() {
        assert_eq!(classify_count_gate_tick(1, true), GenlockTick::Present);
        assert_eq!(classify_count_gate_tick(5, true), GenlockTick::Present);
    }

    #[test]
    fn count_gate_true_empty_is_underrun() {
        // The only real underrun on the count-gate model (counted at the num==0 guard).
        assert_eq!(classify_count_gate_tick(0, false), GenlockTick::Underrun);
    }

    // ---- #269 finding [5]: stale ts-align audit sample → sentinel --------------

    #[test]
    fn ts_audit_sample_fresh_on_a_sampled_tick() {
        assert_eq!(
            genlock_ts_audit_sample(true, 123_456, 2, -5),
            (123_456, 2, -5)
        );
    }

    #[test]
    fn ts_audit_sample_sentinel_on_a_non_sampled_tick() {
        // #269 [5]: a count-gate / true-empty tick must NOT reprint the previous ts-align
        // sample — it publishes the all-zero sentinel.
        assert_eq!(genlock_ts_audit_sample(false, 123_456, 2, -5), (0, 0, 0));
    }

    // ---- #147: backward wall-clock step (NTP/PTP correction) recovery -------
    // SINK ts-align analogue of the cam-EMIT guard #131/#134
    // (ndi.rs::genlock_gate_recovers_after_backward_clock_step). A backward DanteSync
    // wall-clock step drops present_ts below every pre-step (high) frame timestamp →
    // `due == 0` every tick → the unguarded path HOLDs (freezes the program feed)
    // INDEFINITELY. The guard must re-anchor and resume within ~1 interval.

    const RESERVE_MS: u32 = 3; // the prod genlock latency (#257 floor)

    #[test]
    fn sink_backward_clock_step_freezes_the_unguarded_release() {
        // Documents the bug the guard fixes. Steady state: frames captured around the
        // wall clock are due. Then the wall clock steps BACKWARD by ~5 s (a real NTP/PTP
        // sawtooth correction). present_ts = wall - reserve regresses far below every
        // already-queued (pre-step, high) frame timestamp, so the RAW genlock_release
        // finds nothing due and HOLDs — the indefinite freeze.
        let wall0 = WBASE + 10 * NS30;
        let queued = vec![wall0 - NS30, wall0]; // captured just before "now", ascending
                                                // Pre-step: a frame IS due (normal).
        let pre = genlock_release(genlock_present_ts_reserve(wall0, RESERVE_MS), &queued);
        assert!(pre.present, "pre-step a queued frame must be due");

        // Backward step: the clock jumps ~5 s into the past. The queued frames are
        // unchanged (still stamped at the pre-step, higher wall time).
        let wall_after = wall0 - 5_000_000_000;
        let present_ts_after = genlock_present_ts_reserve(wall_after, RESERVE_MS);
        let frozen = genlock_release(present_ts_after, &queued);
        assert!(
            !frozen.present,
            "the bug: after a backward clock step the unguarded release HOLDs (due==0) — \
             this is the program-feed freeze #147 fixes"
        );
    }

    #[test]
    fn sink_backward_clock_step_re_anchors_instead_of_freezing() {
        // The #147 fix: on the SAME backward-step tick the unguarded path would freeze,
        // the guarded release RE-ANCHORS — it flags the backward step AND presents a frame
        // instead of holding. #269 [0]: it presents the OLDEST queued frame and drops NOTHING
        // extra (drop_oldest == 0), preserving the buffer (it drains one frame/tick as the
        // caller erases the presented head) — NOT "present newest, drop num-1" which drained
        // the queue to empty and re-froze the feed for ~latency_ms while it refilled.
        let wall0 = WBASE + 10 * NS30;
        let queued = vec![wall0 - NS30, wall0];
        let wall_after = wall0 - 5_000_000_000;
        let present_ts_after = genlock_present_ts_reserve(wall_after, RESERVE_MS);

        let g = genlock_release_guarded(wall_after, NS30, present_ts_after, &queued);
        assert!(
            g.backward_step,
            "a future-stamped queue after a backward clock step must be DETECTED (#147)"
        );
        assert!(
            g.release.present,
            "the guard must PRESENT a frame, not freeze, after a backward clock step (#147)"
        );
        assert_eq!(
            g.release.drop_oldest, 0,
            "#269 [0]: the re-anchor presents the OLDEST frame and preserves the buffer \
             (drop nothing extra), not drain-to-empty"
        );
    }

    #[test]
    fn sink_backward_step_recovers_within_one_interval_not_frozen_forever() {
        // Mirror of ndi.rs::genlock_gate_recovers_after_backward_clock_step: drive several
        // ticks AT the rewound clock (post-step frames arriving stamped at the new low
        // wall time) and confirm presentation RESUMES within ~1 interval — i.e. the guard
        // self-heals over the non-monotonic seam (stale high frames then fresh low ones),
        // never wedging at "frozen forever" like the unguarded path.
        let wall0 = WBASE + 100 * NS30;
        // At the step instant the FIFO holds the freshest pre-step (high) frame.
        let mut queue: Vec<u64> = vec![wall0];
        let rewound = wall0 - 5_000_000_000; // clock jumps ~5 s back

        let mut presented = 0usize;
        let mut anchored = 0usize;
        for k in 0..4u64 {
            let wall = rewound + k * NS30; // clock advances normally from the rewound point
            let present_ts = genlock_present_ts_reserve(wall, RESERVE_MS);
            let g = genlock_release_guarded(wall, NS30, present_ts, &queue);
            if g.backward_step {
                anchored += 1;
            }
            if g.release.present {
                presented += 1;
                // Apply the real consume (get_closest_frame): drop the `drop_oldest` stale
                // frames AND erase the presented head (array[0]) — drop_oldest + 1 total. With
                // the #269 [0] present-oldest re-anchor (drop_oldest == 0) this drains exactly
                // one frame/tick, the genlock consume rate, preserving the buffer.
                queue.drain(0..=g.release.drop_oldest);
            }
            // A post-step frame arrives stamped at the rewound clock (the cam re-anchored
            // its own emit gate, #131), appended in capture order.
            queue.push(rewound + (k + 1) * NS30);
        }
        assert!(
            presented >= 1,
            "presentation must RESUME after a backward clock step (got {presented}); \
             the unguarded path stays frozen at 0 forever (#147)"
        );
        assert!(
            anchored >= 1,
            "the backward step must be detected + recovered at least once (got {anchored})"
        );
    }

    #[test]
    fn benign_source_early_hold_is_not_treated_as_a_backward_step() {
        // A normal source-early hold: the head frame simply has not aged to the deadline
        // yet (ts in (present_ts, wall_now] — recent, NOT future). The guard must leave it
        // a plain HOLD, never spuriously re-anchor, so the legitimate large per-source
        // latency feature (up to 2000 ms of deliberate, past-stamped buffering) is intact.
        let wall = WBASE + 50 * NS30;
        // A 1000 ms deliberate latency: a frame captured 100 ms ago is queued but not yet
        // due (aged < 1000 ms). It is PAST-stamped (ts <= wall), never future.
        let latency_ms = 1000u32;
        let head_ts = wall - 100_000_000; // 100 ms old — recent, not future
        let queued = vec![head_ts];
        let present_ts = genlock_present_ts_reserve(wall, latency_ms);
        let g = genlock_release_guarded(wall, NS30, present_ts, &queued);
        assert!(
            !g.backward_step,
            "a recent (past-stamped) not-yet-due frame is NOT a backward step (#147)"
        );
        assert!(
            !g.release.present,
            "a benign source-early hold must stay a HOLD (the large-latency buffer fill)"
        );
    }

    #[test]
    fn normal_due_tick_is_unchanged_by_the_guard() {
        // When a frame is genuinely due, the guard is a pass-through: same release as the
        // raw genlock_release, never flagged as a backward step.
        let wall = WBASE + 20 * NS30;
        let queued = vec![wall - 2 * NS30, wall - NS30];
        let present_ts = genlock_present_ts_reserve(wall, RESERVE_MS);
        let g = genlock_release_guarded(wall, NS30, present_ts, &queued);
        assert_eq!(g.release, genlock_release(present_ts, &queued));
        assert!(g.release.present);
        assert!(!g.backward_step);
    }

    // ---- #269 deep-review fixes on the #147 re-anchor (RED→GREEN) -----------

    #[test]
    fn sink_backward_step_preserves_the_latency_buffer_not_drain_to_empty() {
        // #269 [0]: a backward step BIGGER than the source's buffer makes every queued frame
        // future vs present_ts (due==0). The re-anchor must NOT drain the queue to a single
        // frame — for a deep per-source latency override (up to 2000 ms) that collapses the
        // deliberate buffer, so the post-step frames are not "due" until they age the full
        // latency_ms and the program feed FREEZES for ~latency_ms while it refills. The fix
        // presents the OLDEST queued frame and KEEPS the rest as the buffer (it drains one
        // frame per tick as the caller erases the presented head) — a few-frame blip at ANY
        // latency, never a drain-to-empty re-freeze.
        let latency_ms = 500u32;
        let wall0 = WBASE + 1000 * NS30;
        // A full ~500 ms buffer of pre-step frames (≈15 @ 30 fps), ascending capture order.
        let depth = (latency_ms as u64 * 1_000_000).div_ceil(NS30) as usize; // ~15
        let queued: Vec<u64> = (0..depth as u64)
            .map(|i| wall0 - (depth as u64 - 1 - i) * NS30)
            .collect();
        // Backward step far bigger than the buffer depth → all queued frames future → due==0.
        let wall_after = wall0 - 5_000_000_000;
        let present_ts = genlock_present_ts_reserve(wall_after, latency_ms);
        assert!(
            !genlock_release(present_ts, &queued).present,
            "setup: the backward step must produce a due==0 freeze on the unguarded path"
        );

        let g = genlock_release_guarded(wall_after, NS30, present_ts, &queued);
        assert!(
            g.backward_step,
            "a backward step beyond the buffer must re-anchor"
        );
        assert!(
            g.release.present,
            "the re-anchor must present, never freeze"
        );
        // Frames REMAINING after this tick's consume (drop_oldest stale + 1 presented head):
        // the latency buffer must survive, not collapse to ~0.
        let remaining = queued.len() - g.release.drop_oldest - 1;
        assert!(
            remaining >= depth - 2,
            "re-anchor must PRESERVE the latency buffer (left {remaining} of {depth}); the \
             drain-to-empty bug left 0 → froze for ~latency_ms while refilling (#269 [0])"
        );
    }

    #[test]
    fn sink_backward_step_re_anchors_uniformly_regardless_of_queue_depth() {
        // #269 [3]: the re-anchor trigger must be DEPTH-INDEPENDENT so every genlock source
        // recovers UNIFORMLY (in step) from one backward clock step. The old trigger tested
        // the OLDEST queued frame (array[0]), whose future-ness depends on the source's
        // buffer depth — so a deep-buffer source could stay frozen (a benign HOLD) while a
        // shallow source jumped to live: the exact cross-source DESYNC genlock prevents. The
        // trigger must test the NEWEST (max-ts) queued frame instead.
        let wall0 = WBASE + 1000 * NS30;
        // A 5-frame buffer; backward step = 10 intervals (~333 ms — above the #1009
        // re-qualified margin of max(3 intervals, 250 ms); the pre-#1009 revision of this
        // test used a 6-interval/200 ms step, which the margin now deliberately does NOT
        // treat as a backward step — the scenario scaled WITH the trigger, its point is
        // unchanged). The step exceeds the buffer (due==0, would freeze) but the OLDEST
        // frame is only 5 intervals (~167 ms) ahead — UNDER the margin — while the NEWEST
        // is 9 intervals (~300 ms) ahead — OVER it. A depth-independent (max-ts) trigger
        // must still detect the step.
        let queued: Vec<u64> = (1..=5u64).rev().map(|i| wall0 - i * NS30).collect(); // [-5..-1]
        let wall_after = wall0 - 10 * NS30;
        let present_ts = genlock_present_ts_reserve(wall_after, RESERVE_MS);
        assert!(
            !genlock_release(present_ts, &queued).present,
            "setup: must be a due==0 freeze on the unguarded path"
        );
        let margin = crate::genlock_backlog::backward_step_margin_ns(NS30);
        assert!(
            queued[0] <= wall_after + margin,
            "setup: the OLDEST frame must NOT exceed the #1009 margin — else an \
             oldest-frame trigger would already catch it and there'd be nothing to fix"
        );
        assert!(
            *queued.last().unwrap() > wall_after + margin,
            "setup: the NEWEST frame must exceed the #1009 margin, else the guard has \
             nothing to detect at any depth"
        );

        let g = genlock_release_guarded(wall_after, NS30, present_ts, &queued);
        assert!(
            g.backward_step,
            "a deep-buffer source must re-anchor on a backward step too (#269 [3]); the old \
             oldest-frame trigger left it frozen while shallow sources jumped → desync"
        );
        assert!(
            g.release.present,
            "the re-anchor must present, never freeze"
        );
    }

    #[test]
    fn backward_step_counts_once_per_event_not_per_recovery_tick() {
        // #269 [2]: one backward-step EVENT recovers over many ticks (the buffer drains one
        // frame/tick). The audit counter must increment ONCE per event (on the entry edge),
        // not once per tick — else a single NTP/PTP step over N ticks reports as N steps and
        // the LOG_WARNING spams at frame rate, breaking the 5 s audit-log gating.
        let mut in_step = false;
        let mut count = 0u64;
        let mut logs = 0u64;
        for _ in 0..10 {
            let l = genlock_backward_step_latch(in_step, true);
            if l.count_event {
                count += 1;
            }
            if l.log_event {
                logs += 1;
            }
            in_step = l.in_step;
        }
        assert_eq!(
            count, 1,
            "one backward-step event over 10 ticks counts ONCE (#269 [2])"
        );
        assert_eq!(
            logs, 1,
            "LOG_WARNING fires ONCE per event, not per tick (#269 [2])"
        );
    }

    #[test]
    fn separate_backward_step_events_each_count_once() {
        // Two distinct events separated by a normal (non-re-anchor) period each count once —
        // the latch resets when a tick is not a backward step.
        let mut in_step = false;
        let mut count = 0u64;
        // event(2 ticks), gap(2), event(3), gap(1)
        for &bw in &[true, true, false, false, true, true, true, false] {
            let l = genlock_backward_step_latch(in_step, bw);
            if l.count_event {
                count += 1;
            }
            in_step = l.in_step;
        }
        assert_eq!(
            count, 2,
            "two separated backward-step events count twice (#269 [2])"
        );
    }

    // ---- #275b async cam1 capture-burn ring ---------------------------------

    #[test]
    fn burn_frame_id_source_is_monotonic_and_wraps() {
        let mut s = BurnFrameIdSource::default();
        assert_eq!(s.next_id(), 0);
        assert_eq!(s.next_id(), 1);
        assert_eq!(s.next_id(), 2);
        // wraps at u32::MAX (matches the legacy `burn_frame_id`).
        let mut w = BurnFrameIdSource { next: u32::MAX };
        assert_eq!(w.next_id(), u32::MAX);
        assert_eq!(w.next_id(), 0);
    }

    #[test]
    fn async_burn_ring_preserves_1to1_mapping_in_order_under_backpressure() {
        // THE #275b CORRECTNESS CRUX, reproduced deterministically. Moving the cam1 burn render
        // off the emit loop onto an async burn thread is only sound if the burn id ↔ emitted-frame
        // mapping stays strictly 1:1 — every emitted frame's burn id reaches the burn thread
        // EXACTLY ONCE, in emit order, carrying the timecode the gate stamped — even while the
        // burn thread momentarily lags. A full ring MUST back-pressure the capture thread, NEVER
        // silently drop a job (a drop punches a burn-id GAP the recording verdict misreads as a
        // chain loss, corrupting the zero-loss verdict).
        //
        // The capture/emit thread is modelled by the producer loop (stamp id+timecode at the gate,
        // submit); a deliberately SLOW consumer fills the bounded ring so the producer hits
        // back-pressure. With a DROPPING ring (`try_send`) jobs vanish and the assertions FAIL
        // (the RED); with a BLOCKING ring (`send`) every job survives in order (the GREEN).
        use std::thread;
        use std::time::Duration;

        let running = Arc::new(AtomicBool::new(true));
        let (ring, rx) = burn_ring::<(u32, i64)>(running);
        let consumer = thread::spawn(move || {
            let mut seen: Vec<(u32, i64)> = Vec::new();
            run_burn_ring(rx, |job| {
                // a slow burn render → the bounded ring fills → the producer must back-pressure.
                thread::sleep(Duration::from_micros(50));
                seen.push(job);
            });
            seen
        });

        const N: u32 = 500;
        let mut ids = BurnFrameIdSource::default();
        for k in 0..N {
            let frame_id = ids.next_id();
            // a stand-in genlock 60 fps boundary timecode, stamped at the gate instant; the burn
            // thread must send EXACTLY this value (no re-derivation on the send thread).
            let emit_timecode = 1_000_000 + (k as i64) * 166_667;
            ring.submit((frame_id, emit_timecode))
                .expect("the bounded ring blocks; it must never drop while the consumer is alive");
        }
        drop(ring); // close the channel so the consumer's recv loop ends
        let seen = consumer.join().expect("burn thread joins cleanly");

        assert_eq!(
            seen.len(),
            N as usize,
            "every emitted frame's burn job survives the ring — no drop, no duplicate (got {})",
            seen.len()
        );
        for (k, (frame_id, emit_timecode)) in seen.iter().enumerate() {
            assert_eq!(
                *frame_id, k as u32,
                "burn ids stay strictly monotonic AND in emit order (1:1, no reorder)"
            );
            assert_eq!(
                *emit_timecode,
                1_000_000 + (k as i64) * 166_667,
                "the gate-stamped emitted-frame timecode is carried through the ring unchanged"
            );
        }
    }

    #[test]
    fn submit_unblocks_on_shutdown_when_ring_full() {
        // #279 FIX 2 — the SHUTDOWN-DEADLOCK guard. A full-ring submit MUST stay interruptible:
        // when the burn thread stalls (e.g. a synchronous NDI send to a disconnected strih OBS)
        // the ring fills and the capture thread blocks in submit. With an unbounded blocking send
        // (the RED) the only `running` re-check is at the top of the capture while-loop, which the
        // blocked submit never reaches — so on shutdown drop(ring) / grab-flush / burn-join never
        // run and the process wedges (needs SIGKILL; the grab recording is truncated). The GREEN
        // polls with a timeout and re-checks `running`, returning promptly on shutdown.
        use std::sync::mpsc;
        use std::thread;

        let running = Arc::new(AtomicBool::new(true));
        let (ring, _rx) = burn_ring::<u32>(Arc::clone(&running));
        // Fill the bounded ring; NOTHING drains it, so the NEXT submit must block (back-pressure).
        for k in 0..BURN_RING_DEPTH as u32 {
            ring.submit(k)
                .expect("a free slot accepts the job without blocking");
        }

        // The (DEPTH+1)th submit blocks on the full ring. Run it on a worker and signal it back
        // a `done` token the instant it returns, so the test can assert it unblocked PROMPTLY
        // after shutdown rather than parking forever.
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let r = ring.submit(999u32);
            let _ = done_tx.send(());
            r
        });

        // Let the worker reach the blocking submit, then signal shutdown.
        thread::sleep(Duration::from_millis(50));
        running.store(false, Ordering::Relaxed);

        done_rx.recv_timeout(Duration::from_secs(2)).expect(
            "submit must unblock within 2s of shutdown — it parked forever (the #279 FIX 2 deadlock)",
        );
        let result = worker.join().expect("worker thread joins cleanly");
        assert!(
            matches!(result, Err(SubmitError::ShutdownInterrupted(999))),
            "a shutdown-interrupted submit returns the un-sent job (never a silent drop)"
        );
        // `_rx` is kept alive until here so the block was genuine back-pressure (full ring with a
        // live receiver), NOT a closed channel returning early.
        drop(_rx);
    }

    #[test]
    fn burn_renders_qr_only_for_yuyv_else_unburned_passthrough() {
        // #279 FIX 3 — only a YUYV frame may be QR-burned (the burner assumes the YUYV layout).
        // EVERY other format must take the unburned-passthrough branch (still emitted, never
        // dropped) so a v4l2 format substitution can't kill the cam1 feed.
        assert!(burn_should_render_qr("YUYV"), "YUYV is burned");
        for other in [
            "NV12", "MJPG", "UYVY", "BGRA", "BGR4", "RX24", "", "yuyv", "YUY2",
        ] {
            assert!(
                !burn_should_render_qr(other),
                "{other} must NOT be QR-burned — emit it UNBURNED (passthrough), never drop"
            );
        }
    }

    // ---- #280 cam1-burn buffer pool (recycle, no per-frame to_vec churn) -----

    #[test]
    fn buffer_pool_recycles_a_returned_buffer_instead_of_reallocating() {
        // The narrow recycle contract: a buffer returned via `put` is handed back by the next
        // `take` (no fresh allocation), and its ~4 MB capacity survives so the refill never
        // reallocates. RED stub (`take` always allocs, `put` no-op) ⇒ `allocations()` climbs on
        // every take and the returned capacity is lost ⇒ this FAILS. GREEN ⇒ one allocation reused.
        let pool = BufferPool::new(BURN_POOL_CAP);
        let mut buf = pool.take(); // first take: free list empty ⇒ 1 allocation
        assert_eq!(pool.allocations(), 1);
        buf.resize(4096, 0); // give it a real capacity, as a frame copy would
        let cap_before = buf.capacity();
        pool.put(buf); // return it for reuse
        assert_eq!(pool.free_len(), 1, "the returned buffer is held for reuse");

        let reused = pool.take(); // must come from the free list — NOT a fresh allocation
        assert_eq!(
            pool.allocations(),
            1,
            "a returned buffer is recycled — take must NOT allocate again"
        );
        assert!(
            reused.capacity() >= cap_before,
            "the recycled buffer keeps its capacity so the refill does not reallocate (got {}, was {})",
            reused.capacity(),
            cap_before
        );
        assert_eq!(pool.free_len(), 0, "the reused buffer left the free list");
    }

    #[test]
    fn pooled_async_burn_recycles_buffers_and_preserves_1to1_ordering_under_backpressure() {
        // #280 — the cam1 async-burn frame copy now reuses a bounded [`BufferPool`] instead of a
        // per-frame ~4 MB `to_vec`. Both invariants proven TOGETHER on the REAL ring + a slow
        // consumer (the same back-pressure harness as the #275b 1:1 test):
        //   (1) RECYCLE — across N emitted frames at most [`BURN_POOL_CAP`] buffers are ever
        //       ALLOCATED (no per-frame heap churn, no unbounded growth), because the burn thread
        //       RETURNS each buffer to the pool after "sending" and the capture thread reuses it.
        //       RED stub (`take` always allocs) ⇒ allocations == N ⇒ FAILS. GREEN ⇒ bounded.
        //   (2) 1:1 / IN ORDER / TIMECODE — the pool is a memory optimization ONLY; it must not
        //       change the frame_id↔emit mapping or the carried timecode (same bar as #275b/#279).
        use std::thread;
        use std::time::Duration;

        const FRAME: usize = 4096; // stand-in for the 1080p YUYV frame bytes
        const N: u32 = 500;

        let pool = Arc::new(BufferPool::new(BURN_POOL_CAP));
        let running = Arc::new(AtomicBool::new(true));
        // Job carries (frame_id, timecode, buffer) — the buffer is the pooled allocation.
        let (ring, rx) = burn_ring::<(u32, i64, Vec<u8>)>(Arc::clone(&running));

        let consumer_pool = Arc::clone(&pool);
        let consumer = thread::spawn(move || {
            let mut seen: Vec<(u32, i64)> = Vec::new();
            run_burn_ring(rx, |(frame_id, tc, buf)| {
                // a slow burn render → the bounded ring fills → the producer must back-pressure.
                thread::sleep(Duration::from_micros(50));
                seen.push((frame_id, tc));
                consumer_pool.put(buf); // #280 — return the buffer for the capture thread to reuse
            });
            seen
        });

        let mut ids = BurnFrameIdSource::default();
        for k in 0..N {
            let frame_id = ids.next_id();
            // a stand-in genlock 60 fps boundary timecode, stamped at the gate instant.
            let emit_tc = 1_000_000 + (k as i64) * 166_667;
            let mut buf = pool.take(); // reuse a returned buffer (or alloc only when empty)
            buf.clear();
            buf.resize(FRAME, (k & 0xFF) as u8); // copy "the frame" in (reuses capacity)
            ring.submit((frame_id, emit_tc, buf))
                .expect("the bounded ring blocks; it must never drop while the consumer is alive");
        }
        drop(ring); // close the channel so the consumer's recv loop ends
        let seen = consumer.join().expect("burn thread joins cleanly");

        // (2) 1:1 / in-order / timecode carried — unchanged by the pool.
        assert_eq!(
            seen.len(),
            N as usize,
            "every emitted frame's burn job survives the ring — no drop, no duplicate (got {})",
            seen.len()
        );
        for (k, (frame_id, tc)) in seen.iter().enumerate() {
            assert_eq!(
                *frame_id, k as u32,
                "burn ids stay strictly monotonic AND in emit order (1:1, no reorder)"
            );
            assert_eq!(
                *tc,
                1_000_000 + (k as i64) * 166_667,
                "the gate-stamped emitted-frame timecode is carried through the ring unchanged"
            );
        }
        // (1) RECYCLE — fresh allocations bounded by the pool cap, NOT N (the per-frame to_vec).
        assert!(
            pool.allocations() <= BURN_POOL_CAP,
            "pool recycles buffers: {} allocations for {} frames (must be ≤ {})",
            pool.allocations(),
            N,
            BURN_POOL_CAP
        );
        // NO UNBOUNDED GROWTH — the free list never exceeds the cap.
        assert!(
            pool.free_len() <= BURN_POOL_CAP,
            "free list bounded: {} (must be ≤ {})",
            pool.free_len(),
            BURN_POOL_CAP
        );
    }
}
