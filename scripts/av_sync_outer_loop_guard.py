"""#806 (epic #800 A/V-desync endgame round) -- the OUTER-loop guard, Python mirror.

A LITERAL port of `src/asrc_outer_loop.rs`'s `OuterLoopGuard` (same constants, same formula) --
this is the module the actual watchdog imports, since the watchdog itself is Python (it drives the
SyncNet/ffmpeg pipeline `av_sync_measure.py` already owns; there is no Rust runtime on the stream
box). Keep the two in lock-step -- same convention `vendor/obs-studio/libobs/media-io/
asrc-compensator.c` already follows for `src/asrc_bench.rs`.

Root cause + full design (sign convention, safety rails, rejected alternatives): see the Rust
module's own doc comment (`src/asrc_outer_loop.rs`) and issue #806's design comment
(`gh issue view 806 --comments`). In one line: the inner ASRC servo (issue #803) locks a source's
audio timeline to the video master clock via sample-count/wall-clock rate estimation, but still
leaves a small long-term residual (~3-20 ms/hour) invisible to it by construction; SyncNet
(`av_sync_measure.py`) is the only thing that measures REAL audio-video alignment, at a slow (~7
min) cadence -- exactly right as an OUTER feedback loop correcting the inner loop's own bias.
"""

from dataclasses import dataclass, field

# Number of most-recent confident SyncNet residual samples averaged before the guard will act.
# Mirrors src/asrc_outer_loop.rs WINDOW_N.
WINDOW_N = 3

# The ticket's own threshold, in ms: only a sustained average residual whose magnitude exceeds
# this triggers a correction. Mirrors src/asrc_outer_loop.rs RESIDUAL_THRESHOLD_MS.
RESIDUAL_THRESHOLD_MS = 40.0

# Rate limit: the maximum bias_ppm change per correction event. Mirrors
# src/asrc_outer_loop.rs STEP_PPM.
STEP_PPM = 1.0

# Hard bound on the bias this guard will ever produce, in ppm -- the ticket's own "max +/-10 ppm
# od inner-loop odhadu" safety rail. Mirrors src/asrc_bench.rs OUTER_BIAS_MAX_PPM (the SAME
# constant the Rust C mirror clamps to at the vendored libobs setter).
OUTER_BIAS_MAX_PPM = 10.0


@dataclass(frozen=True)
class CorrectionEvent:
    """One correction the guard decided to apply -- the telemetry record the watchdog
    Discord-reports and logs (the ticket's own "plna telemetria + Discord hlasenie kazdej upravy"
    safety rail). Mirrors src/asrc_outer_loop.rs CorrectionEvent."""

    avg_residual_ms: float
    previous_bias_ppm: float
    new_bias_ppm: float


def _clamp(value: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, value))


@dataclass
class OuterLoopGuard:
    """The outer-loop guard itself -- cadence-agnostic (it does not know or care how far apart in
    wall time successive `observe()` calls are; the caller supplies confident SyncNet measurements
    at whatever cadence it runs, ~7 min in production via `av_sync_measure.py --loop 420`).
    Mirrors src/asrc_outer_loop.rs OuterLoopGuard."""

    bias_ppm: float = 0.0
    window: list = field(default_factory=list)

    @classmethod
    def from_bias_ppm(cls, bias_ppm: float) -> "OuterLoopGuard":
        """Restore a guard from a previously persisted bias (watchdog restart) -- clamped like the
        setter; the window itself is intentionally NOT persisted (a restart naturally re-warms it
        from fresh measurements). Mirrors src/asrc_outer_loop.rs OuterLoopGuard::from_bias_ppm."""
        return cls(bias_ppm=_clamp(bias_ppm, -OUTER_BIAS_MAX_PPM, OUTER_BIAS_MAX_PPM), window=[])

    def observe(self, residual_ms: float) -> "CorrectionEvent | None":
        """Observe one CONFIDENT SyncNet residual measurement (ms, same sign convention as this
        module's own offset_ms elsewhere in the file: positive = audio leads video). Returns a
        CorrectionEvent exactly when the bias actually changed this call, None otherwise (window
        not yet full, average within bounds, or already saturated at the clamp in the needed
        direction). Mirrors src/asrc_outer_loop.rs OuterLoopGuard::observe."""
        self.window.append(residual_ms)
        if len(self.window) > WINDOW_N:
            self.window.pop(0)
        if len(self.window) < WINDOW_N:
            return None

        avg_residual_ms = sum(self.window) / len(self.window)
        if abs(avg_residual_ms) <= RESIDUAL_THRESHOLD_MS:
            return None

        direction = 1.0 if avg_residual_ms > 0.0 else -1.0
        candidate = _clamp(self.bias_ppm + direction * STEP_PPM, -OUTER_BIAS_MAX_PPM, OUTER_BIAS_MAX_PPM)
        if candidate == self.bias_ppm:
            return None

        previous_bias_ppm = self.bias_ppm
        self.bias_ppm = candidate
        return CorrectionEvent(
            avg_residual_ms=avg_residual_ms,
            previous_bias_ppm=previous_bias_ppm,
            new_bias_ppm=candidate,
        )
