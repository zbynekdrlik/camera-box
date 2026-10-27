//! Issue 1482 step 2 — the long-horizon SOURCE phase estimate (RED stub: never ready).

/// The source fit's window: the last this many seconds of folded samples.
pub const SOURCE_FIT_SECONDS: u64 = 180;

/// The source fit drives the stamp only once its window spans at least this many seconds.
pub const SOURCE_FIT_MIN_SECONDS: u64 = 60;

/// One source-fit point per this many frames of sequence position.
pub const SOURCE_DECIMATION_FRAMES: i64 = 10;

/// The long-horizon least-squares fit of `t = a + P * seq` (RED stub).
#[derive(Debug, Clone, Default)]
pub struct SourcePhase;

impl SourcePhase {
    pub(super) fn reset(&mut self) {}

    pub(super) fn offer(&mut self, _x: i64, _t: u64) {}

    /// The time the window spans (ns).
    pub fn span_ns(&self) -> u64 {
        0
    }

    /// Points in the window.
    pub fn points(&self) -> usize {
        0
    }

    /// True once the window spans [`SOURCE_FIT_MIN_SECONDS`].
    pub fn ready(&self) -> bool {
        false
    }

    /// The source capture time at sequence position `x`, once [`Self::ready`].
    pub fn predict_ns(&self, _x: i64) -> Option<u64> {
        None
    }

    /// The fitted source frame period (ns).
    pub fn period_ns(&self) -> Option<f64> {
        None
    }
}
