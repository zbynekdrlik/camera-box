//! #1260 — pure, dependency-free within-tick "prepare once, reuse" decision for the DistroAV QR
//! burn filter (`vendor/distroav/src/ndi-burn-filter.cpp`).
//!
//! The burn filter's `video_render` runs once per DRAW of its parent source — the PROGRAM mix +
//! (Studio-Mode) preview + every Multiview cell. Doing the full base texrender + QR raster/upload
//! per draw meant strih's 4K Multiview re-ran the burn for all 7 cam sources every MV frame, which
//! pushed the MV `render_ewma` over the per-tick budget (`obs-display-budget.h`, #278/#293) and
//! collapsed the MV to `30/(K+1) = 7.5 fps` (issue 1260) while the program render stayed healthy.
//!
//! This models the fix: do the expensive prep + advance the burn `frame_id` EXACTLY ONCE per video
//! tick (the first draw — always the program, since `output_frames()` runs before
//! `render_displays()`), and let the later within-tick draws REUSE the cached base texrender + QR
//! texture (a cheap sprite blit). Cadence safety (verified against the verdict contract, issue
//! 1260): strih/stream burn contiguity is `node_render_step == 1` gap-ignore (forward gaps are
//! unconditionally ignored, so a smaller per-recorded-frame step is inert), imag's step is
//! auto-derived (`imag_tick_gate::calibrate_burn_step`) and its per-frame content gate is
//! report-only — so stamping once per tick keeps every existing verdict green, and additionally
//! removes the per-draw `frame_id` pollution (preview/MV draws no longer advance the counter the
//! recorded program frame carries).
//!
//! Tier-0 authority for the C mirror `vendor/distroav/src/burn-tick-cache.hpp` (byte-identical,
//! proven by the C-parity harness in `tests/burn_tick_cache_parity.rs`). Pure `std` so it
//! unit-tests on default features (Tier-0) via the standalone-rustc recipe
//! (`vendored-libobs-change-safety.md`).

/// Per-filter-instance within-tick prepare/reuse state. `prepared_this_tick` is cleared once per
/// video tick (the filter's `video_tick` callback) and set by the first render of that tick, so
/// exactly one draw per tick does the expensive prep + stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurnTickCache {
    prepared_this_tick: bool,
}

impl Default for BurnTickCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BurnTickCache {
    /// A fresh cache: nothing prepared yet, so the very first render prepares. Matches a
    /// `bzalloc`'d C `struct burn_tick_cache` (all-zero ⇒ `prepared_this_tick == false`).
    pub const fn new() -> Self {
        Self {
            prepared_this_tick: false,
        }
    }

    /// Called once per video tick (the filter's `video_tick`). Invalidates the cached composite so
    /// the next render re-preps and re-stamps the burn for the new frame.
    pub fn on_tick(&mut self) {
        self.prepared_this_tick = false;
    }

    /// Called at the start of each render's draw (after the burn-enabled + resources gates).
    /// Returns `true` iff this render must do the EXPENSIVE prep (base texrender + QR raster +
    /// upload + advance `frame_id`); `false` iff it may REUSE the cached composite. Exactly ONE
    /// render per tick returns `true`.
    pub fn on_render(&mut self) -> bool {
        // #1260 [red] STUB — the within-tick cache is not yet wired; every draw still "prepares"
        // (the pre-fix per-draw behaviour), so the MV re-renders all 7 burns every frame. Replaced
        // by the once-per-tick state machine in the [green] commit.
        true
    }

    /// A prep that FAILED (a transient graphics-reset window) must not leave the tick marked
    /// prepared — else a later within-tick draw would reuse an unprepared/stale composite. Call
    /// this on a prep failure to re-arm the next draw this tick.
    pub fn abort_prepare(&mut self) {
        self.prepared_this_tick = false;
    }

    /// Introspection for tests / logs.
    pub fn prepared_this_tick(&self) -> bool {
        self.prepared_this_tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_render_prepares_then_reuses_within_a_tick() {
        let mut c = BurnTickCache::new();
        assert!(c.on_render(), "first render of a tick must PREPARE");
        assert!(!c.on_render(), "2nd within-tick render must REUSE");
        assert!(!c.on_render(), "3rd within-tick render must REUSE");
    }

    #[test]
    fn a_new_tick_re_prepares() {
        let mut c = BurnTickCache::new();
        assert!(c.on_render());
        assert!(!c.on_render());
        c.on_tick();
        assert!(
            c.on_render(),
            "the first render after a new tick must PREPARE again"
        );
        assert!(!c.on_render());
    }

    #[test]
    fn stamp_advances_exactly_once_per_tick_regardless_of_draw_count() {
        // Simulate N ticks, each with a VARYING within-tick draw count (program + preview + MV
        // cells), and count how many draws PREPARE (== how many times frame_id advances). It must
        // equal the TICK count, never the draw count — the verdict-cadence property (#1260).
        let mut c = BurnTickCache::new();
        let draws_per_tick = [1usize, 3, 2, 7, 1, 3];
        let mut stamps = 0usize;
        for &draws in &draws_per_tick {
            c.on_tick();
            for _ in 0..draws {
                if c.on_render() {
                    stamps += 1;
                }
            }
        }
        assert_eq!(
            stamps,
            draws_per_tick.len(),
            "frame_id must advance once per TICK, not per draw"
        );
    }

    #[test]
    fn abort_prepare_re_arms_within_the_same_tick() {
        let mut c = BurnTickCache::new();
        assert!(c.on_render(), "prepare");
        c.abort_prepare(); // the prep hit a transient failure
        assert!(
            c.on_render(),
            "after an aborted prep, the next within-tick draw must PREPARE again"
        );
        assert!(!c.on_render(), "then reuse");
    }

    #[test]
    fn default_matches_new_and_starts_unprepared() {
        assert_eq!(BurnTickCache::default(), BurnTickCache::new());
        assert!(!BurnTickCache::new().prepared_this_tick());
    }
}
