#pragma once

/*
 * camera-box A/V-sync dock -- decode the measurement source's OWN share of the program mix (issue 1470).
 *
 * The dock paired every QPSK marker it heard in the whole track-1 mix with the cam2 painter's QR
 * ring. On stream-snv the program also played the measurement clip (run 911016, a synthesized copy of
 * the painter's marker), so its digital markers formed a tighter, bigger cluster than the room marker
 * on `mbc` and the dock locked on -900.9 ms while the stream was in sync (+24.5 ms). An 8-bit marker
 * cannot tell two emitters apart, so the decision (issue 1470 ROZHODNUTÉ 6381433364) is to decode only
 * the measurement source's own audio, on the timeline the program output carries.
 *
 * Where that audio is (vendored libobs, read in the code):
 *   - each mix tick, process_audio_source_tick (obs-source.c) peeks the source's input into its
 *     per-source mix buffer (`obs_source_get_audio_mix`), zeroes the unrouted tracks and applies its
 *     volume / mute; mix_audio (obs-audio.c) and scene_audio_render (obs-scene.c) add exactly those
 *     samples at `audio_ts - ts.start` -- after the sync offset and every placement term;
 *   - a callback the dock connects with obs_add_raw_audio_callback runs after the mix of the SAME
 *     tick (media-io/audio-io.c input_and_output), with the tick's start as its timestamp;
 *   - but discard_audio (obs-audio.c) has already set the source's `audio_ts = ts.end` by then, for
 *     an inactive or pending source too, so the share's start offset is gone.
 *
 * So `CbMeasureShareTracker` takes a tick's share ONLY when, judged from what libobs left after the
 * tick, it is the whole tick at offset 0:
 * the source is found, active (in the program), not pending (it was rendered this tick), its timeline
 * ended the PREVIOUS tick exactly at this tick's start (mix_audio skips one that is even 1 ns early)
 * and ends this tick at its end (libobs's frame-counted end, 0-1 ns after the dock's). Every other tick is silence -- never the mix. A source that is not
 * taken for CB_SHARE_ABSENT_END_NS reads `absent_long`: the dock ends its decode session (STALE + one
 * log line) instead of measuring something else.
 *
 * What "in the program" means here: rendered in this tick's audio tree and aligned. Known limits (the
 * dock still reads only the measurement source's own marker, so none of them lets a foreign emitter
 * in): (1) the residual -- a producer-side timeline reset (reset_audio_timing / reset_audio_data on the
 * source's ingest thread) landing between the dock's read after one tick and the next tick's mix makes
 * that one tick taken although it was mixed at an offset (up to one tick, 21.3 ms) or not at all;
 * pinned by tests/av_sync_dock_measure_share_lift_1470.rs; (2) the share carries no scene-item fade and
 * is also taken when the source is active only through a view whose audio is not mixed, or when a
 * sibling drags its scene's timestamp out of the window; (3) a source that renders its own audio
 * (info.audio_render) never reaches the tick-end rule, so it is never taken. The obs-vban `mbc` input
 * is a push source and is taken on every steady tick.
 *
 * Dependency-free (STL + <cstdint>), C++11; pinned by the g++ self-test
 * `vendor/av-sync-dock/test/measure-share-selftest.cpp`, which tests/av_sync_dock_measure_share_1470.rs
 * compiles and runs.
 */

#include <cstddef>
#include <cstdint>
#include <cstring>

namespace camerabox {

/* A measurement source not taken for this long (on the mix clock) ends the dock's decode session. */
static const uint64_t CB_SHARE_ABSENT_END_NS = 1000000000ull;

/* What the dock reads about the measurement source right after one mix tick. */
struct CbShareTick {
	uint64_t tick_start_ns;      // the tick's start (the callback's data->timestamp)
	uint64_t tick_end_ns;        // tick start + the tick's frames
	bool source_found;           // the dock holds the measurement source
	bool source_active;          // obs_source_active: in the program, so in this tick's audio tree
	bool source_pending;         // obs_source_audio_pending: not rendered this tick
	uint64_t source_ts_after_ns; // obs_source_get_audio_timestamp after the tick (discard_audio set it)
};

struct CbShareDecision {
	bool take;        // the per-source mix buffer is this whole tick's share, at offset 0
	bool absent_long; // not taken for CB_SHARE_ABSENT_END_NS: end the decode session (STALE)
};

class CbMeasureShareTracker {
public:
	CbMeasureShareTracker() : have_prev_(false), prev_after_ns_(0), absent_(false), absent_since_ns_(0) {}

	/* A new source object (the held reference changed): its previous timeline is unknown. */
	void reset()
	{
		have_prev_ = false;
		prev_after_ns_ = 0;
		absent_ = false;
		absent_since_ns_ = 0;
	}

	/* One tick, right after its mix: is the per-source mix buffer this whole tick's share? The
	 * share sits at offset 0 only when the source's timeline stood EXACTLY at this tick's start
	 * before the mix -- which is where the previous tick's discard left it -- and the mix consumed
	 * it to this tick's end:
	 *   - the start is exact: mix_audio skips a timeline before the start even by 1 ns (while
	 *     discard_audio pops it as aligned), and one just after it is left silent (conservative);
	 *   - the end is the dock's floor-computed end or 1 ns later: libobs counts its tick ends from
	 *     the frame total, so its end is never earlier. */
	CbShareDecision observe(const CbShareTick &t)
	{
		const bool aligned = have_prev_ && prev_after_ns_ == t.tick_start_ns &&
				     t.source_ts_after_ns >= t.tick_end_ns && t.source_ts_after_ns - t.tick_end_ns <= 1;
		CbShareDecision d;
		d.take = t.source_found && t.source_active && !t.source_pending && aligned;
		/* A found source's timeline is the next tick's reference whether it was mixed or not: an
		 * inactive or pending source is discarded to the tick end all the same. */
		have_prev_ = t.source_found;
		prev_after_ns_ = t.source_ts_after_ns;
		if (d.take) {
			absent_ = false;
		} else if (!absent_) {
			absent_ = true;
			absent_since_ns_ = t.tick_start_ns;
		}
		d.absent_long = absent_ && t.tick_end_ns >= absent_since_ns_ &&
				t.tick_end_ns - absent_since_ns_ >= CB_SHARE_ABSENT_END_NS;
		return d;
	}

private:
	bool have_prev_;
	uint64_t prev_after_ns_;
	bool absent_;
	uint64_t absent_since_ns_;
};

/* The block the dock decodes for one tick: the share's first `frames` samples per channel when
 * `take`, else silence. `out` planes hold at least `frames` floats each, sized before the audio thread
 * ever writes them (no first touch there). */
inline void cb_measure_share_fill(bool take, const float *const *share, size_t channels, size_t frames,
				  float *const *out)
{
	for (size_t c = 0; c < channels; c++) {
		if (take && share[c])
			std::memcpy(out[c], share[c], frames * sizeof(float));
		else
			std::memset(out[c], 0, frames * sizeof(float));
	}
}

} // namespace camerabox
