/* camera-box issue 1425 (E3 slice S4, design 6225701358) -- the scenarios of
 * tests/genlock_audio_arrival_rebase_1425.rs.
 *
 * NOT compiled on its own: the test takes tests/c/genlock_audio_relabel_ingest_1381_harness.c with the
 * shipped obs-source.c ingest branch substituted in (the issue-1381 lift, unchanged), renames that
 * harness's main, appends this file and substitutes the shipped genlock_audio_arrival_rebase_log for
 * the at-sign marker below. The scenarios drive the same lifted place-vs-append branch with ARRIVAL-basis
 * sources (an Off hold on non-direct stamps, a resampler, the arrival servo's level captured) and the
 * sources that must keep OBS's own path. One line per event packet: appended (push_back with a live
 * audio_ts: the samples join the buffer end), the timeline reset, the dropped buffer, timing_adjust
 * re-latched, the rebase counter, and the stock LOG_DEBUG lines ("exceeded TS_SMOOTHING_THRESHOLD" /
 * "jumped") the packet printed. */

@ARRIVAL_LOG_FN@

/* the source's stamps: on a foreign clock (not direct: libobs latches timing_adjust) or on the mixer
 * clock (direct) */
static bool a_direct;
static int a_mode;

static void a_reset(const char *name, int mode, bool direct, bool captured, bool monitor_only, bool resampler)
{
	h_reset(name);
	a_mode = mode;
	a_direct = direct;
	h_src.asrc_enabled = true;
	h_src.resampler = resampler ? (void *)&h_src : NULL;
	h_src.asrc.level_captured = captured;
	h_src.monitoring_type = monitor_only ? OBS_MONITORING_TYPE_MONITOR_ONLY : 0;
}

struct a_out {
	bool appended;
	bool reset;
	bool dropped;
	bool ta_moved;
	int debug;
};

static struct a_out a_packet(void)
{
	struct audio_data d;
	memset(&d, 0, sizeof(d));
	d.frames = H_PACKET_FRAMES;
	const uint64_t base = a_direct ? H_MONO : H_WALL;
	d.timestamp = base + h_k * H_PACKET_NS + (uint64_t)h_shift;
	const uint64_t now = H_MONO + h_k * H_PACKET_NS + H_LAG_NS;
	h_k++;
	const uint64_t ta_before = h_src.timing_adjust;
	const bool live = h_src.audio_ts != 0;
	const int debug_before = h_debug_lines;
	struct h_out o = h_ingest(&h_src, &d, a_mode, h_off, now);
	genlock_audio_arrival_rebase_log(&h_src, now);
	struct a_out a = {o.push_back && live, o.reset, o.dropped, h_src.timing_adjust != ta_before,
			  h_debug_lines - debug_before};
	return a;
}

static void a_print(const char *what, struct a_out a)
{
	printf("%s appended=%d reset=%d dropped=%d ta_moved=%d rebases=%u debug=%d\n", what, a.appended ? 1 : 0,
	       a.reset ? 1 : 0, a.dropped ? 1 : 0, a.ta_moved ? 1 : 0, h_src.genlock_arrival_rebases, a.debug);
	if (h_info_set)
		printf("log %s\n", h_info);
}

/* steady packets; each must append quietly (the source's very first packet places, as in OBS) */
static void a_steady(int n)
{
	for (int i = 0; i < n; i++) {
		const bool first = h_k == 0;
		struct a_out a = a_packet();
		if (a.appended == first || a.reset || a.dropped || a.debug || h_info_set)
			a_print("A-STEADY-BROKEN", a);
	}
}

int main(void)
{
	(void)h_main_1381;
	/* a locked arrival source (stock VLC: stamps on its own sample clock) whose resampled continuation
	 * drifted 70 ms from its stamps, either way: OBS placed the packet at its stamp (a 70 ms gap or
	 * overwrite of its own audio); the rebase appends it */
	static const struct {
		const char *name;
		int64_t drift_ns;
	} drift[] = {
		{"arrival_drift_70ms", 70000000},
		{"arrival_drift_back_70ms", -70000000},
	};
	for (size_t j = 0; j < sizeof(drift) / sizeof(drift[0]); j++) {
		a_reset(drift[j].name, GENLOCK_AUDIO_HOLD_OFF, false, true, false, true);
		a_steady(40);
		h_shift += drift[j].drift_ns;
		a_print("drift", a_packet());
		a_print("after", a_packet());
		a_steady(5);
	}
	/* the same at a latency hold (a genlock source on the #1303 latency hold is an arrival source too) */
	a_reset("arrival_latency_hold_70ms", GENLOCK_AUDIO_HOLD_LATENCY, false, true, false, true);
	a_steady(40);
	h_shift += 70000000;
	a_print("drift", a_packet());
	a_steady(5);
	/* the stamps jump 3 s with continuous arrival: OBS ran handle_ts_jump (the queued buffer dropped,
	 * timing_adjust re-latched); the rebase appends, timing_adjust untouched */
	a_reset("arrival_jump_3s", GENLOCK_AUDIO_HOLD_OFF, false, true, false, true);
	a_steady(40);
	h_shift += 3000000000ll;
	a_print("jump", a_packet());
	a_print("after", a_packet());
	a_steady(5);
	/* the render thread moved timing_adjust 80 ms between two packets (an async source's video re-pairs
	 * it, obs_source_update_async_video): the stamps snap, the SYSTEM domain is 80 ms off. OBS placed
	 * the packet; the system-domain rebase appends it */
	a_reset("arrival_sys_jump_80ms", GENLOCK_AUDIO_HOLD_OFF, false, true, false, true);
	a_steady(40);
	h_src.timing_adjust += 80000000ull;
	a_print("sys_jump", a_packet());
	a_print("after", a_packet());
	a_steady(5);
	/* one line per minute per source at most: a second rebase 11 packets later counts and prints
	 * nothing, a third one 60.4 s after the first prints again */
	a_reset("arrival_rate_limited", GENLOCK_AUDIO_HOLD_OFF, false, true, false, true);
	a_steady(40);
	h_shift += 70000000;
	a_print("first", a_packet());
	a_steady(10);
	h_shift += 70000000;
	a_print("second", a_packet());
	a_steady(1801);
	h_shift += 70000000;
	a_print("third", a_packet());
	/* rows that keep OBS's own path, byte for byte: */
	/* an unlocked arrival source (the arrival servo has not captured its depth: the launch alignment) */
	a_reset("arrival_unlocked", GENLOCK_AUDIO_HOLD_OFF, false, false, false, true);
	a_steady(40);
	h_shift += 70000000;
	a_print("drift", a_packet());
	/* the mixer declared the source stopped (discard_if_stopped: audio_ts 0) and it resumes after a
	 * 500 ms pause: placed at its stamp as in OBS; the rebase counts nothing */
	a_reset("arrival_stopped_500ms", GENLOCK_AUDIO_HOLD_OFF, false, true, false, true);
	a_steady(40);
	h_k += 15;
	h_src.audio_ts = 0;
	a_print("resume", a_packet());
	/* a listen-only source (never mixed) and a source without a resampler (no arrival servo) */
	a_reset("arrival_listen_only", GENLOCK_AUDIO_HOLD_OFF, false, true, true, true);
	a_steady(40);
	h_shift += 70000000;
	a_print("drift", a_packet());
	a_reset("arrival_no_resampler", GENLOCK_AUDIO_HOLD_OFF, false, true, false, false);
	a_steady(40);
	h_shift += 70000000;
	a_print("drift", a_packet());
	/* a directly stamped source (the STAMP basis: mbc, PulseAudio, a media clip, VLC after S4v) */
	a_reset("stamp_basis_70ms", GENLOCK_AUDIO_HOLD_OFF, true, true, false, true);
	a_steady(40);
	h_shift += 70000000;
	a_print("drift", a_packet());
	/* a timecode source (the 1367 stamp leap: a slot never sent, the stamps 47 ms on: 80 ms after an
	 * arrival gap) */
	a_reset("timecode_leap_80ms", GENLOCK_AUDIO_HOLD_TIMECODE, false, true, false, true);
	a_steady(40);
	h_k++;
	h_shift += 47000000;
	a_print("leap", a_packet());
	printf("debug_lines=%d\n", h_debug_lines);
	return 0;
}
