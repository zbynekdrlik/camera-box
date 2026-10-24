//! issue 1424 (owner 24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni monitora aj
//! po zapnuti!!!"; main design 6386467303, layer 3) -- the Rust twin of
//! `vendor/obs-studio/libobs/obs-drm-output-retry.h`, the PURE retry decisions of a vk-direct HDMI
//! output: when a dormant output retries its start, when a live one whose present loop died is
//! released, and how the present loop backs off rebuilding a presentation whose monitor went away.
//!
//! Every function mirrors its C twin line for line; `tests/drm_output_retry_1424.rs` compiles the C
//! header and requires both to print the same answers over scripted supervisor scenarios.

/// The dormant retry: 2 s for the first [`FAST_TRIES`] failures, then 5 s.
pub const FAST_NS: u64 = 2_000_000_000;
pub const SLOW_NS: u64 = 5_000_000_000;
pub const FAST_TRIES: u32 = 5;
/// The dormant retry while X RandR reads the connector disconnected.
pub const UNPLUGGED_NS: u64 = 30_000_000_000;

/// The present loop's rebuild: 1 s for [`REBUILD_FAST_TRIES`] tries, then 5 s beside another display.
pub const REBUILD_FAST_NS: u64 = 1_000_000_000;
pub const REBUILD_SLOW_NS: u64 = 5_000_000_000;
pub const REBUILD_FAST_TRIES: u32 = 10;

/// The supervisor's view of one output (a default struct = LIVE: nothing seen yet).
pub const LIVE: i32 = 0;
pub const DORMANT: i32 = 1;

/// What the supervisor does after one look.
pub const NONE: i32 = 0;
pub const START: i32 = 1;
pub const RELEASE: i32 = 2;

/// The present loop's rebuild log lines.
pub const REBUILD_LOG_NONE: i32 = 0;
pub const REBUILD_LOG_TRY: i32 = 1;
pub const REBUILD_LOG_SLOW: i32 = 2;

/// `struct drm_output_retry`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retry {
    pub state: i32,
    pub fails: u32,
    pub next_ns: u64,
}

/// `drm_output_retry_delay_ns`: the wait after `fails` failed starts.
pub fn delay_ns(fails: u32, connected: bool) -> u64 {
    if !connected {
        return UNPLUGGED_NS;
    }
    if fails < FAST_TRIES {
        FAST_NS
    } else {
        SLOW_NS
    }
}

/// `drm_output_retry_step`: one look at an output. `started` = the backend reports it open, `alive` =
/// its present loop runs, `connected` = X RandR does not read the connector disconnected.
pub fn step(r: &mut Retry, started: bool, alive: bool, connected: bool, now_ns: u64) -> i32 {
    if started && alive {
        r.state = LIVE;
        r.fails = 0;
        return NONE;
    }
    if started {
        return RELEASE;
    }
    if r.state == LIVE {
        r.state = DORMANT;
        r.fails = 0;
        r.next_ns = now_ns.wrapping_add(delay_ns(0, connected));
        return NONE;
    }
    if connected && r.next_ns > now_ns.wrapping_add(delay_ns(r.fails, true)) {
        r.next_ns = now_ns;
    }
    if now_ns >= r.next_ns {
        START
    } else {
        NONE
    }
}

/// `drm_output_retry_released`: a released LIVE output is dormant, first retry after the first wait.
pub fn released(r: &mut Retry, connected: bool, now_ns: u64) {
    r.state = DORMANT;
    r.fails = 0;
    r.next_ns = now_ns.wrapping_add(delay_ns(0, connected));
}

/// `drm_output_retry_attempted`: the result of a start attempted at `now_ns`.
pub fn attempted(r: &mut Retry, ok: bool, connected: bool, now_ns: u64) {
    if ok {
        r.state = LIVE;
        r.fails = 0;
        r.next_ns = 0;
        return;
    }
    if r.fails < u32::MAX {
        r.fails += 1;
    }
    r.next_ns = now_ns.wrapping_add(delay_ns(r.fails, connected));
}

/// `drm_output_rebuild_next`: after `failed_tries` failed rebuilds, the wait before the next try, or
/// `None` = give up (release; the supervisor retries the whole open).
pub fn rebuild_next(failed_tries: u32, others_open: bool) -> Option<u64> {
    if failed_tries < REBUILD_FAST_TRIES {
        return Some(REBUILD_FAST_NS);
    }
    if others_open {
        return Some(REBUILD_SLOW_NS);
    }
    None
}

/// `drm_output_rebuild_log_kind`: which line rebuild try number `tries` (1-based) logs.
pub fn rebuild_log_kind(tries: u32) -> i32 {
    if (1..=REBUILD_FAST_TRIES).contains(&tries) {
        return REBUILD_LOG_TRY;
    }
    if tries == REBUILD_FAST_TRIES + 1 {
        return REBUILD_LOG_SLOW;
    }
    REBUILD_LOG_NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn a_reconnected_connector_is_tried_at_once_but_a_failing_one_keeps_its_backoff() {
        let mut r = Retry::default();
        assert_eq!(step(&mut r, false, false, true, 0), NONE);
        assert_eq!(step(&mut r, false, false, true, 2 * S), START);
        attempted(&mut r, false, true, 2 * S);
        // connected the whole time: the 2 s backoff is not short-circuited
        assert_eq!(step(&mut r, false, false, true, 3 * S), NONE);
        assert_eq!(step(&mut r, false, false, true, 4 * S), START);
    }

    #[test]
    fn a_live_output_never_starts_or_releases() {
        let mut r = Retry::default();
        for t in 0..100 {
            assert_eq!(step(&mut r, true, true, t % 2 == 0, t * S), NONE);
        }
        assert_eq!(r, Retry::default());
    }

    #[test]
    fn the_fail_count_saturates() {
        let mut r = Retry {
            state: DORMANT,
            fails: u32::MAX,
            next_ns: 0,
        };
        attempted(&mut r, false, true, 0);
        assert_eq!(r.fails, u32::MAX);
        assert_eq!(r.next_ns, SLOW_NS);
    }
}
