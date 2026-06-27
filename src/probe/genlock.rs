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
/// #131/#134 guard fixed (`src/ndi.rs genlock_emit_gate`: a boundary latched impossibly
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
    // depth-independent, so all genlock sources re-anchor UNIFORMLY once the step exceeds one
    // interval. A legitimate large per-source latency override (≤2000 ms) buffers PAST-stamped
    // frames (ts ≤ wall_now, aging toward the deadline), whose max is never > wall_now +
    // interval, so the guard never touches it.
    let max_ts = queued_ts_ascending.iter().copied().max();
    let backward_step = max_ts.is_some_and(|m| m > wall_now_ns.saturating_add(interval_ns));
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

/// (#276) Pure mirror of the obs-display.c `render_display()` per-display frame-skip
/// gate that decouples the heavy built-in Multiview projector from the 60fps program
/// render. The vendored C is
/// `if (display->render_divisor > 1 && (display->frame_counter++ % display->render_divisor) != 0) return;`
/// — a render is SKIPPED when the divisor is >1 and the current counter is not a
/// multiple of it; the counter is post-incremented ONLY when the divisor is >1 (the
/// `&&` short-circuits for divisor 0/1, so those displays — program output, preview —
/// never touch the counter and render EVERY frame). Returns `(skip, next_counter)`.
/// This is the only unit-testable part of #276 (the GPU render timing itself needs the
/// rig); it locks the skip CADENCE (divisor=2 → renders every other frame → halves the
/// multiview's render-thread cost, freeing the program presentation).
pub fn display_render_skip(frame_counter: u32, render_divisor: u32) -> (bool, u32) {
    if render_divisor > 1 {
        // skip == C's `(frame_counter % render_divisor) != 0` (== not a multiple of the divisor).
        let skip = !frame_counter.is_multiple_of(render_divisor);
        (skip, frame_counter.wrapping_add(1))
    } else {
        // divisor 0/1: render every frame; counter untouched (matches the C `&&` short-circuit).
        (false, frame_counter)
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

use std::sync::mpsc::{sync_channel, Receiver, SendError, SyncSender};

/// #275b — depth of the async cam1-burn ring: how many emitted frames the capture thread may
/// queue ahead of the burn thread. 3 absorbs normal render jitter (the "pre-render the next
/// frame's QR while the current sends" look-ahead) while bounding the added latency to ≤ ~3
/// emit intervals.
pub const BURN_RING_DEPTH: usize = 3;

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

/// #275b — capture-thread producer handle for the async cam1-burn ring (the sending side of a
/// bounded FIFO [`sync_channel`]).
pub struct BurnRing<T> {
    tx: SyncSender<T>,
}

impl<T> BurnRing<T> {
    /// Hand one emitted frame's burn work to the burn thread. MUST preserve the strict 1:1 burn
    /// id ↔ emitted-frame mapping: when the bounded ring is full it BACK-PRESSURES (blocks the
    /// capture thread until the burn thread drains a slot) rather than dropping a job. Returns
    /// `Err` only once the burn thread (receiver) has gone (shutdown).
    pub fn submit(&self, job: T) -> Result<(), SendError<T>> {
        // BLOCKING send: when the bounded ring is full, the capture thread waits for the burn
        // thread to drain a slot rather than dropping the job. This is the whole correctness
        // crux — a dropped job would punch a burn-id GAP the recording verdict misreads as a
        // chain loss. Throughput is then min(emit-gate rate, burn-thread rate); the ring depth
        // absorbs jitter so the capture thread rarely actually blocks.
        self.tx.send(job)
    }
}

/// #275b — create the async cam1-burn ring: a bounded ([`BURN_RING_DEPTH`]) FIFO channel.
/// Returns the capture-thread [`BurnRing`] producer + the burn-thread [`Receiver`].
pub fn burn_ring<T>() -> (BurnRing<T>, Receiver<T>) {
    let (tx, rx) = sync_channel(BURN_RING_DEPTH);
    (BurnRing { tx }, rx)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- #276 multiview render-divisor skip cadence -------------------------

    #[test]
    fn render_divisor_0_or_1_renders_every_frame_and_never_touches_counter() {
        // Program output + preview never set a divisor (bzalloc → 0). They MUST render
        // every frame and the counter must stay 0 (the C `&&` short-circuits the post-
        // increment), so they are genuinely unaffected by #276.
        for divisor in [0u32, 1] {
            let mut counter = 0u32;
            for _ in 0..10 {
                let (skip, next) = display_render_skip(counter, divisor);
                assert!(!skip, "divisor {divisor} must never skip");
                assert_eq!(
                    next, 0,
                    "divisor {divisor} must leave the counter untouched"
                );
                counter = next;
            }
        }
    }

    #[test]
    fn render_divisor_2_renders_every_other_frame() {
        // The multiview is throttled to 2 → renders frames 0,2,4,… skips 1,3,5,… so its
        // 9-18ms render lands on the program thread only every other frame (halved cost).
        let mut counter = 0u32;
        let mut rendered = 0;
        let mut skipped = 0;
        for i in 0..10u32 {
            let (skip, next) = display_render_skip(counter, 2);
            if i.is_multiple_of(2) {
                assert!(!skip, "even frame {i} renders");
                rendered += 1;
            } else {
                assert!(skip, "odd frame {i} skips");
                skipped += 1;
            }
            counter = next;
        }
        assert_eq!((rendered, skipped), (5, 5));
        assert_eq!(counter, 10, "counter advances every call when divisor>1");
    }

    #[test]
    fn render_divisor_3_renders_one_in_three() {
        let mut counter = 0u32;
        let rendered: usize = (0..9)
            .filter(|_| {
                let (skip, next) = display_render_skip(counter, 3);
                counter = next;
                !skip
            })
            .count();
        assert_eq!(rendered, 3, "divisor 3 renders 1/3 of frames (0,3,6)");
    }

    #[test]
    fn render_divisor_counter_wraps_without_panic() {
        // The counter is u32 and increments forever on a live multiview; it must wrap, not
        // overflow-panic, and the cadence must stay correct across the wrap.
        let (skip, next) = display_render_skip(u32::MAX, 2);
        // u32::MAX is odd → MAX % 2 == 1 → skip; next wraps to 0.
        assert!(skip);
        assert_eq!(next, 0);
        let (skip0, _) = display_render_skip(0, 2);
        assert!(!skip0, "after the wrap, counter 0 renders again");
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
        // A 5-frame buffer; backward step = 6 intervals. The step exceeds the buffer (due==0,
        // would freeze) but the OLDEST frame is NOT itself > one interval in the future — only
        // the NEWEST is. A depth-independent (max-ts) trigger must still detect the step.
        let queued: Vec<u64> = (1..=5u64).rev().map(|i| wall0 - i * NS30).collect(); // [-5..-1]
        let wall_after = wall0 - 6 * NS30;
        let present_ts = genlock_present_ts_reserve(wall_after, RESERVE_MS);
        assert!(
            !genlock_release(present_ts, &queued).present,
            "setup: must be a due==0 freeze on the unguarded path"
        );
        assert!(
            queued[0] <= wall_after + NS30,
            "setup: the OLDEST frame must NOT be > one interval ahead — else the old \
             oldest-frame trigger would already catch it and there'd be nothing to fix"
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

        let (ring, rx) = burn_ring::<(u32, i64)>();
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
}
