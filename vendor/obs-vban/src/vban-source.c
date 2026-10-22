/*
 * OBS VBAN Audio Plugin
 * Copyright (C) 2022 Norihiro Kamae <norihiro@nagater.net>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#include <obs-module.h>
#include <util/platform.h>
#include <util/threading.h>
#include <util/deque.h>
#include "plugin-macros.generated.h"
#include "vban-udp.h"
#include "vban.h"
#include "vban-receive-timeline.h"
#include "vban-conceal.h"
#include "vban-sample-io.h"
#ifdef __linux__
#include <sys/prctl.h>
#endif

/* camera-box issue 1439 (ROZHODNUTE 6347872706): one in-time packet stored for its slot until the worker
 * hands it to libobs; its audio planes follow this header in the store */
struct vban_src_slot {
	uint64_t pos;
	uint32_t nu;  /* its nuFrame (the loss line names the packet that ends a run) */
	uint32_t frames;
	uint32_t samples_per_sec;
	int format;   /* enum audio_format */
	int speakers; /* enum speaker_layout */
	uint32_t planes;
	uint32_t plane_bytes;
	/* ROZHODNUTE 6352253147 (b): its first frame per channel on the [-1, 1) scale -- a concealed slot right
	 * before it fades toward it */
	double first[MAX_AV_PLANES];
};

struct vban_src_s
{
	obs_source_t *context;

	// properties
	int port;
	/* camera-box issue 1439: the port for the receive thread's status line (s->port and s->stream_name
	 * belong to the UI thread; update() frees the old stream name) */
	volatile long log_port;
	char *stream_name;
	char *ip_from;
	/* camera-box issue 1439: the fixed "Receive Buffer" (R, ms), read at create/update only */
	volatile long receive_buffer_ms;
	/* camera-box issue 1439: the next packet starts a new timeline (another port / stream / host) */
	volatile bool timeline_reset;

	vban_udp_t *vban;

	DARRAY(float) buffer;
	/* camera-box issue 1439 (ROZHODNUTE 6347872706): the source emits on its own clock, never on a
	 * packet's arrival. The receive thread decides and stores; the playout worker hands every slot to
	 * libobs by its deadline. MUTEX guards everything both touch and is held only for a decision and a
	 * copy: no output, no log line, no wait under it. */
	pthread_mutex_t mutex;
	struct vban_recv_timeline timeline; /* under MUTEX */
	struct deque store;                 /* under MUTEX: the in-time packets, struct vban_src_slot + planes, in
					     * slot order (ROZHODNUTE 6352253147 a: a reordered packet goes in
					     * before the later slots) */
	size_t store_count;                 /* under MUTEX */
	uint64_t store_back_pos;            /* under MUTEX: the last stored packet's slot */
	DARRAY(uint8_t) rot;                /* under MUTEX: one stored packet while a reorder is put in place */
	bool store_full;                    /* under MUTEX: a packet was dropped on a full store (logged once) */
	enum audio_format out_format;       /* under MUTEX: the stream's sample format (the silence's) */
	enum speaker_layout out_speakers;   /* under MUTEX */
	uint32_t out_rate;                  /* under MUTEX */
	bool worker_idle;                   /* under MUTEX: nothing to emit until a packet gives work */
	int64_t emit_late_max_ns;           /* under MUTEX: the window's latest emission past a deadline */
	bool emit_late_seen;                /* under MUTEX */
	/* camera-box issue 1439 (review round 5): update() runs whole under it -- the UI thread and the
	 * obs-websocket thread can both call it, and two calls must never join one worker twice */
	pthread_mutex_t update_mutex;
	pthread_t worker;                   /* create / update / destroy only */
	bool worker_running;                /* create / update / destroy only */
	volatile bool worker_stop;
	os_event_t *worker_wake;
	DARRAY(uint8_t) out;     /* worker only: the packet copied out of the store */
	DARRAY(uint8_t) silence; /* worker only: one chunk of silence in the stream's own sample format */
	/* worker only (ROZHODNUTE 6352253147 a): the slots of the current run handed to libobs without their
	 * packet, logged once when the run ends; whether the run was concealed */
	uint64_t run_slots;
	bool run_concealed;
	/* worker only (ROZHODNUTE 6352253147 b): the last packet handed to libobs (what a concealment repeats),
	 * its header, the last frame handed to libobs per channel (where a concealment fades in from), and the
	 * concealed slot being built */
	DARRAY(uint8_t) prev;
	struct vban_src_slot prev_slot;
	bool prev_valid;
	double tail[MAX_AV_PLANES];
	DARRAY(uint8_t) conceal;
	uint64_t status_ns;      /* the last status line, 0 = none yet */
	int64_t window_late_max_ns; /* the largest arrival - stamp since the last status line */
	bool window_late_seen;
	/* camera-box issue 1439 (ROZHODNUTE 6345255813 item 2): a run of late (dropped) packets is logged once */
	bool late_run;
	/* camera-box issue 1439 (ROZHODNUTE 6343922049): the (R, effective R) pair last warned about, so a
	 * Receive Buffer lifted to one packet + 10 ms is named once, not at every re-anchor */
	uint64_t warned_reserve_ns;
	uint64_t warned_reserve_eff_ns;
	/* camera-box issue 1439 (review round 5): a `format` re-anchor line is written at most once per status
	 * period (a sender whose packet size varies re-anchors on every packet); the count not written */
	uint64_t format_line_ns;
	uint64_t format_unlogged;
};

static const char *vban_src_get_name(void *type_data)
{
	UNUSED_PARAMETER(type_data);
	return obs_module_text("VBAN.src");
}

static void vban_src_callback(const char *buf, size_t buf_len, void *data);
static void vban_src_worker_start(struct vban_src_s *s);
static void vban_src_worker_stop(struct vban_src_s *s, const char *why);

static void update_port(struct vban_src_s *s, int port)
{
	vban_udp_t *old_vban = s->vban;

	s->vban = vban_udp_find_or_create(port);

	s->port = port;
	os_atomic_set_long(&s->log_port, port);

	if (old_vban) {
		vban_udp_remove_callback(old_vban, vban_src_callback, s);
		vban_udp_release(old_vban);
	}

	vban_udp_add_callback(s->vban, vban_src_callback, s);
}

/* camera-box issue 1439: whether update_string() would replace CUR with VAL */
static bool vban_src_string_changes(const char *cur, const char *val)
{
	return !cur || (val && strcmp(val, cur) != 0);
}

bool update_string(char **opt, obs_data_t *settings, const char *name)
{
	const char *val = obs_data_get_string(settings, name);
	if (vban_src_string_changes(*opt, val)) {
		bfree(*opt);
		*opt = bstrdup(val);
		return true;
	}
	return false;
}

static void vban_src_update(void *data, obs_data_t *settings)
{
	struct vban_src_s *s = data;

	pthread_mutex_lock(&s->update_mutex);
	/* camera-box issue 1439 (ROZHODNUTE 6347872706): the playout worker is stopped and joined before
	 * anything here changes, and started again at the end */
	vban_src_worker_stop(s, "update");

	bool port_changed = false;
	bool ip_changed = false;
	bool name_changed = false;

	int port = (int)obs_data_get_int(settings, "port");

	/* camera-box issue 1439: R is fixed from create/update; another value re-anchors the timeline at
	 * its next packet (a counted, logged config re-anchor), never a runtime adaptation. It is in place
	 * before any sender is let in below (on create, a packet could otherwise anchor with R = 0). */
	os_atomic_set_long(&s->receive_buffer_ms,
			   (long)vban_recv_clamp_reserve_ms((int64_t)obs_data_get_int(settings, "receive_buffer_ms")));

	/* camera-box issue 1439: another port / stream / host starts a new timeline. The reset is asked for
	 * BEFORE the new sender is let in below, so its first packet already finds it (asked only after,
	 * that packet could land on the old timeline: a gap read as lost slots, or a counted re-anchor). It
	 * is asked again at the end, in case the old sender's last packet took this one in between. */
	if (port != s->port || vban_src_string_changes(s->stream_name, obs_data_get_string(settings, "stream_name")) ||
	    vban_src_string_changes(s->ip_from, obs_data_get_string(settings, "ip_from")))
		os_atomic_set_bool(&s->timeline_reset, true);

	if (port != s->port) {
		update_port(s, port);
		port_changed = true;
	}

	if (update_string(&s->stream_name, settings, "stream_name"))
		name_changed = true;

	if (update_string(&s->ip_from, settings, "ip_from"))
		ip_changed = true;

	if (port_changed || name_changed)
		vban_udp_set_name(s->vban, vban_src_callback, s, s->stream_name);

	if (port_changed || ip_changed)
		vban_udp_set_host(s->vban, vban_src_callback, s, s->ip_from);

	if (port_changed || name_changed || ip_changed)
		os_atomic_set_bool(&s->timeline_reset, true);

	vban_src_worker_start(s);
	pthread_mutex_unlock(&s->update_mutex);
}

static obs_properties_t *vban_src_get_properties(void *data)
{
	UNUSED_PARAMETER(data);

	obs_properties_t *props = obs_properties_create();

	obs_properties_add_int(props, "port", obs_module_text("VBAN.src.prop.port"), 1, 65535, 1);
	obs_properties_add_text(props, "stream_name", obs_module_text("VBAN.src.prop.stream_name"), OBS_TEXT_DEFAULT);
	obs_properties_add_text(props, "ip_from", obs_module_text("VBAN.src.prop.ip_from"), OBS_TEXT_DEFAULT);
	obs_properties_add_int(props, "receive_buffer_ms", obs_module_text("VBAN.src.prop.receive_buffer_ms"),
			       VBAN_RECV_RESERVE_MS_MIN, VBAN_RECV_RESERVE_MS_MAX, 1);

	return props;
}

static void vban_src_get_defaults(obs_data_t *data)
{
	obs_data_set_default_int(data, "port", 6980);
	obs_data_set_default_int(data, "receive_buffer_ms", VBAN_RECV_RESERVE_MS_DEFAULT);
}

static void *vban_src_create(obs_data_t *settings, obs_source_t *source)
{
	struct vban_src_s *s = bzalloc(sizeof(struct vban_src_s));
	s->context = source;
	pthread_mutex_init(&s->mutex, NULL);
	pthread_mutex_init(&s->update_mutex, NULL);
	os_event_init(&s->worker_wake, OS_EVENT_TYPE_AUTO);
	vban_recv_init(&s->timeline);

	/* starts the playout worker at its end */
	vban_src_update(s, settings);

	return s;
}

static void vban_src_destroy(void *data)
{
	struct vban_src_s *s = data;

	if (s->vban) {
		vban_udp_remove_callback(s->vban, vban_src_callback, s);
		vban_udp_release(s->vban);
	}
	/* camera-box issue 1439: no packet comes in any more; the worker is joined before the timeline is
	 * read or freed */
	vban_src_worker_stop(s, "destroy");

	const struct vban_recv_timeline *t = &s->timeline;
	blog(t->lost || t->re_anchors ? LOG_ERROR : LOG_INFO,
	     "source '%s': received %" PRIu64 " packets, lost %" PRIu64 " (%" PRIu64 " concealed), late %" PRIu64
	     ", reordered %" PRIu64 ", stale %" PRIu64 ", %" PRIu64 " re-anchor(s)",
	     obs_source_get_name(s->context), t->received, t->lost, t->concealed, t->late, t->reordered, t->stale,
	     t->re_anchors);

	deque_free(&s->store);
	os_event_destroy(s->worker_wake);
	pthread_mutex_destroy(&s->mutex);
	pthread_mutex_destroy(&s->update_mutex);
	bfree(s->stream_name);
	bfree(s->ip_from);
	da_free(s->buffer);
	da_free(s->out);
	da_free(s->silence);
	da_free(s->rot);
	da_free(s->prev);
	da_free(s->conceal);
	bfree(s);
}

const struct obs_source_info vban_source_info = {
	.id = ID_PREFIX "source",
	.type = OBS_SOURCE_TYPE_INPUT,
	.output_flags = OBS_SOURCE_AUDIO | OBS_SOURCE_DO_NOT_DUPLICATE,
	.get_name = vban_src_get_name,
	.create = vban_src_create,
	.destroy = vban_src_destroy,
	.update = vban_src_update,
	.get_properties = vban_src_get_properties,
	.get_defaults = vban_src_get_defaults,
	.icon_type = OBS_ICON_TYPE_AUDIO_INPUT,
};

static void convert_24le_to_fltp(struct vban_src_s *s, struct obs_source_audio *audio, const char *buf)
{
	da_resize(s->buffer, audio->speakers * audio->frames);
	float *dst = s->buffer.array;
	for (int ch = 0; ch < (int)audio->speakers; ch++) {
		audio->data[ch] = (void *)dst;
		const char *src = buf + ch * 3;
		for (uint32_t i = 0; i < audio->frames; i++) {
			int x = (src[0] & 0xFF) | ((src[1] & 0xFF) << 8) | ((src[2] & 0xFF) << 16) |
				((src[2] & 0x80) ? 0xFF000000 : 0);
			*dst++ = x * (1.0f / 8388608.0f);
			src += 3 * audio->speakers;
		}
	}
}

static const char *vban_recv_reason_name(int reason)
{
	switch (reason) {
	case VBAN_RECV_REASON_STARVE:
		return "starve";
	case VBAN_RECV_REASON_BACKWARD:
		return "backward";
	case VBAN_RECV_REASON_JUMP:
		return "jump";
	case VBAN_RECV_REASON_FORMAT:
		return "format";
	case VBAN_RECV_REASON_CONFIG:
		return "config";
	case VBAN_RECV_REASON_FIT:
		return "fit";
	default:
		return "none";
	}
}

/* camera-box issue 1439: what the receive thread logs, copied from the timeline under the lock (the worker
 * moves the counters too). */
struct vban_src_snapshot {
	uint64_t received, lost, late, re_anchors, stale, silence_samples, missed, reordered, concealed;
	uint64_t reserve_ns, reserve_eff_ns;
	uint32_t rate, channels;
	bool fit_valid, fit_frozen;
	int64_t fit_ppb, phase_ns;
	bool emit_late_seen;
	int64_t emit_late_max_ns;
};

static struct vban_src_snapshot vban_src_snapshot_locked(const struct vban_src_s *s)
{
	const struct vban_recv_timeline *t = &s->timeline;
	struct vban_src_snapshot n = {
		.received = t->received,
		.lost = t->lost,
		.late = t->late,
		.re_anchors = t->re_anchors,
		.stale = t->stale,
		.silence_samples = t->silence_samples,
		.missed = t->missed,
		.reordered = t->reordered,
		.concealed = t->concealed,
		.reserve_ns = t->reserve_ns,
		.reserve_eff_ns = t->reserve_eff_ns,
		.rate = t->rate,
		.channels = t->channels,
		.fit_valid = t->fit_valid,
		.fit_frozen = vban_recv_fit_frozen(t),
		.fit_ppb = t->fit_ppb,
		.phase_ns = t->phase_ns,
		.emit_late_seen = s->emit_late_seen,
		.emit_late_max_ns = s->emit_late_max_ns,
	};
	return n;
}

/* camera-box issue 1439: one packet as the receive thread reports it. */
struct vban_src_arrival {
	uint32_t nu_frame;
	uint32_t frames;
	uint64_t now;
	bool first_output;   /* this packet started the stretch's output */
	bool emitted_before; /* the stretch was already emitting */
	/* the stretch's format before this packet (a `format` re-anchor names the change) */
	uint32_t frames_before, rate_before, channels_before;
};

/* camera-box issue 1439: the anchor / re-anchor / late / loss lines, and the lateness record of the
 * window. */
static void vban_src_report(struct vban_src_s *s, const struct vban_recv_decision *d,
			    const struct vban_src_snapshot *n, const struct vban_src_arrival *a)
{
	const uint32_t nu_frame = a->nu_frame;
	const uint32_t frames = a->frames;
	const bool emitted_before = a->emitted_before;
	/* a new line: is R lifted to one packet + the margin (R is measured from the packet's start)? */
	if ((d->event == VBAN_RECV_ANCHOR || d->event == VBAN_RECV_REANCHOR) && n->reserve_eff_ns != n->reserve_ns &&
	    (n->reserve_ns != s->warned_reserve_ns || n->reserve_eff_ns != s->warned_reserve_eff_ns)) {
		s->warned_reserve_ns = n->reserve_ns;
		s->warned_reserve_eff_ns = n->reserve_eff_ns;
		blog(LOG_WARNING,
		     "obs-vban receive buffer: source '%s' receive_buffer_ms=%" PRIu64
		     " is not above one packet (%.2f ms) + %d ms: using %.2f ms",
		     obs_source_get_name(s->context), (uint64_t)(n->reserve_ns / VBAN_RECV_NS_PER_MS),
		     (double)vban_recv_samples_to_ns(frames, n->rate) / 1e6, VBAN_RECV_RESERVE_MARGIN_MS,
		     (double)n->reserve_eff_ns / 1e6);
	}
	if (d->event == VBAN_RECV_ANCHOR || d->event == VBAN_RECV_REANCHOR)
		s->late_run = false;
	if (a->first_output) {
		/* the hold is over: the stretch's first in-time packet; the worker sends its lead-in now and
		 * every slot by its deadline, on the anchor's final (earliest-arrival) line */
		blog(LOG_INFO,
		     "obs-vban receive emitting: source '%s' frame=%u margin_ms=%.1f after the first second's hold "
		     "-- every slot goes out by %d ms before its stamp",
		     obs_source_get_name(s->context), nu_frame, (double)-d->lateness_ns / 1e6,
		     VBAN_RECV_DEADLINE_MARGIN_MS);
	}
	if (d->event == VBAN_RECV_ANCHOR) {
		blog(LOG_INFO,
		     "obs-vban receive anchored: source '%s' frame=%u rate=%u ch=%u receive_buffer_ms=%" PRIu64
		     " -- holding its first second",
		     obs_source_get_name(s->context), nu_frame, n->rate, n->channels,
		     (uint64_t)(n->reserve_ns / VBAN_RECV_NS_PER_MS));
	} else if (d->event == VBAN_RECV_REANCHOR) {
		/* a new timeline from this packet, then the hold; a starve (no packet in time for a second)
		 * names how long that was. A `format` one names the change and is written at most once per
		 * status period (review round 5: a sender whose packet size varies re-anchors on every
		 * packet; a slot is one packet, so a varying size is not supported). */
		const bool format = d->reason == VBAN_RECV_REASON_FORMAT;
		char what[192] = "";
		if (format && s->format_line_ns && a->now - s->format_line_ns < VBAN_RECV_LOG_INTERVAL_NS) {
			s->format_unlogged++;
		} else {
			if (format) {
				snprintf(what, sizeof(what),
					 " (frames %u -> %u, rate %u -> %u, ch %u -> %u: a varying packet size is not "
					 "supported; %" PRIu64 " more not logged in the last period)",
					 a->frames_before, frames, a->rate_before, n->rate, a->channels_before,
					 n->channels, s->format_unlogged);
				s->format_line_ns = a->now ? a->now : 1;
				s->format_unlogged = 0;
			}
			blog(LOG_WARNING,
			     "obs-vban receive re-anchor: source '%s' reason=%s frame=%u starve_ms=%.1f"
			     " re_anchors=%" PRIu64 " late=%" PRIu64 " -- holding its first second%s",
			     obs_source_get_name(s->context), vban_recv_reason_name(d->reason), nu_frame,
			     (double)d->starve_ns / 1e6, n->re_anchors, n->late, what);
		}
	}
	if (d->event == VBAN_RECV_LATE && !s->late_run) {
		/* ROZHODNUTE 6345255813 item 2: a late packet is lost, never re-timed; one line per late run.
		 * Before the stretch's first output nothing is sent for it (review round 6): the line says so. */
		s->late_run = true;
		blog(LOG_WARNING,
		     "obs-vban receive late: source '%s' frame=%u late_ms=%.1f -- dropped, %s (late=%" PRIu64 ")",
		     obs_source_get_name(s->context), nu_frame, (double)d->late_ns / 1e6,
		     emitted_before ? "its slot is silence on the anchored line"
				    : "before the stretch's first output: nothing filled",
		     n->late);
	} else if (d->event == VBAN_RECV_PACKET) {
		s->late_run = false;
	}
	/* the lateness record includes the late packets that were dropped and the reorders (the measure R is
	 * chosen by) */
	if ((d->event == VBAN_RECV_PACKET || d->event == VBAN_RECV_LATE || d->event == VBAN_RECV_REORDER) &&
	    (!s->window_late_seen || d->lateness_ns > s->window_late_max_ns)) {
		s->window_late_max_ns = d->lateness_ns;
		s->window_late_seen = true;
	}
}

/* camera-box issue 1439: FRAMES of silence stamped STAMP_NS (every piece STAMP_NS + its nominal offset),
 * in pieces of at most VBAN_RECV_SILENCE_CHUNK_FRAMES frames, in the SAME sample format and layout as the
 * stream's packets: libobs resets the source's resampler, and with it the genlock ASRC, whenever the
 * format of the audio it is handed changes. Unsigned 8-bit silence is 0x80, every other format's is 0.
 * Worker thread only. */
static void vban_src_output_silence(struct vban_src_s *s, enum audio_format format, enum speaker_layout speakers,
				    uint32_t rate, uint64_t frames, uint64_t stamp_ns)
{
	const bool planar = is_audio_planar(format);
	const size_t channels = (size_t)speakers;
	const size_t plane_bytes =
		VBAN_RECV_SILENCE_CHUNK_FRAMES * get_audio_bytes_per_channel(format) * (planar ? 1 : channels);
	da_resize(s->silence, plane_bytes);
	memset(s->silence.array, format == AUDIO_FORMAT_U8BIT ? 0x80 : 0, plane_bytes);
	struct obs_source_audio silence = {
		.speakers = speakers,
		.format = format,
		.samples_per_sec = rate,
	};
	for (size_t ch = 0; ch < (planar ? channels : 1) && ch < MAX_AV_PLANES; ch++)
		silence.data[ch] = s->silence.array;
	for (uint64_t done = 0; done < frames;) {
		uint64_t n = frames - done;
		if (n > VBAN_RECV_SILENCE_CHUNK_FRAMES)
			n = VBAN_RECV_SILENCE_CHUNK_FRAMES;
		silence.frames = (uint32_t)n;
		silence.timestamp = stamp_ns + vban_recv_samples_to_ns(done, rate);
		obs_source_output_audio(s->context, &silence);
		done += n;
	}
}

/* camera-box issue 1439: one status line every VBAN_RECV_LOG_INTERVAL_NS while packets arrive. The
 * counters are cumulative since the source started (or was reconfigured); max_late_ms is the window's
 * lateness record (arrival - stamp; negative = the margin R left), the measure R is chosen by;
 * emit_late_ms is the window's latest slot against its deadline (negative = that far ahead of it: a stored
 * packet goes out at the worker's next look; a lost slot's silence at its deadline plus the worker's wake
 * jitter); emit_missed counts the slots handed to libobs after their own stamp (the worker woke later
 * than the margin, review round 5). The stream name is the packet's own: s->stream_name is the UI
 * thread's, and update() may free it. */
static bool vban_src_status_due(struct vban_src_s *s, uint64_t now)
{
	if (s->status_ns == 0) {
		s->status_ns = now;
		return false;
	}
	if (now - s->status_ns < VBAN_RECV_LOG_INTERVAL_NS)
		return false;
	s->status_ns = now;
	return true;
}

static void vban_src_log_status(struct vban_src_s *s, const struct VBanHeader *header,
				const struct vban_src_snapshot *n)
{
	const double rate = n->rate ? (double)n->rate : 1.0;
	blog(LOG_INFO,
	     "obs-vban receive: received=%" PRIu64 " lost=%" PRIu64 " late=%" PRIu64 " re_anchors=%" PRIu64
	     " stale=%" PRIu64 " reordered=%" PRIu64 " concealed=%" PRIu64 " silence_ms=%.1f max_late_ms=%.1f"
	     " sender_ppm=%+.3f"
	     " phase_ms=%+.2f fit=%s"
	     " emit_late_ms=%.1f emit_missed=%" PRIu64 " receive_buffer_ms=%" PRIu64
	     " rate=%u ch=%u port=%ld stream='%.*s' source='%s'",
	     n->received, n->lost, n->late, n->re_anchors, n->stale, n->reordered, n->concealed,
	     (double)n->silence_samples * 1000.0 / rate,
	     s->window_late_seen ? (double)s->window_late_max_ns / 1e6 : 0.0,
	     n->fit_valid ? -(double)n->fit_ppb / 1000.0 : 0.0, (double)n->phase_ns / 1e6,
	     n->fit_frozen ? "frozen" : (n->fit_valid ? "locked" : "acquiring"),
	     n->emit_late_seen ? (double)n->emit_late_max_ns / 1e6 : 0.0, n->missed,
	     (uint64_t)(n->reserve_ns / VBAN_RECV_NS_PER_MS), n->rate, n->channels,
	     os_atomic_load_long(&s->log_port), (int)VBAN_STREAM_NAME_SIZE, header->streamname,
	     obs_source_get_name(s->context));
	s->window_late_seen = false;
	s->window_late_max_ns = 0;
}

/* The store holds at most this many packets (21 s of 256-frame packets): one that does not fit is
 * dropped and logged once, the worker has stalled. */
#define VBAN_SRC_STORE_MAX 4096u
/* The worker never sleeps longer than this at a time, so a stop is prompt. */
#define VBAN_SRC_WORKER_SLEEP_MAX_NS 20000000ULL
/* With nothing to emit the worker waits this long for a packet to wake it. */
#define VBAN_SRC_WORKER_IDLE_MS 100

/* Under the lock: a new timeline (another port / stream / host), the stored packets gone with it. */
static void vban_src_reset_locked(struct vban_src_s *s)
{
	vban_recv_init(&s->timeline);
	if (s->store.size)
		deque_pop_front(&s->store, NULL, s->store.size);
	s->store_count = 0;
}

static void vban_src_store_clear_locked(struct vban_src_s *s)
{
	if (s->store.size)
		deque_pop_front(&s->store, NULL, s->store.size);
	s->store_count = 0;
}

/* Under the lock: store AUDIO (the packet NU) for the slot at POS, keeping the store in slot order. A packet
 * after every stored one is appended; a REORDERED one (ROZHODNUTE 6352253147 a: its slot is before the last
 * stored one) goes in by turning every stored packet once through the deque, putting it in before the first
 * later slot -- a rare event, at most about R / one packet entries, no allocation once the rotation buffer
 * has grown. Returns false when the store is full. */
static bool vban_src_store_push_locked(struct vban_src_s *s, uint64_t pos, uint32_t nu,
				       const struct obs_source_audio *audio)
{
	if (s->store_count >= VBAN_SRC_STORE_MAX)
		return false;
	const bool planar = is_audio_planar(audio->format);
	const size_t channels = (size_t)audio->speakers;
	const size_t plane_bytes = audio->frames * get_audio_bytes_per_channel(audio->format) * (planar ? 1 : channels);
	struct vban_src_slot slot = {
		.pos = pos,
		.nu = nu,
		.frames = audio->frames,
		.samples_per_sec = audio->samples_per_sec,
		.format = (int)audio->format,
		.speakers = (int)audio->speakers,
		.planes = (uint32_t)(planar ? channels : 1),
		.plane_bytes = (uint32_t)plane_bytes,
	};
	for (size_t ch = 0; ch < channels && ch < MAX_AV_PLANES; ch++)
		slot.first[ch] = vban_src_sample_get(audio, ch, 0);
	const size_t n = s->store_count;
	const bool append = n == 0 || pos > s->store_back_pos;
	bool placed = false;
	for (size_t k = 0; k < (append ? 0 : n); k++) {
		struct vban_src_slot e;
		deque_pop_front(&s->store, &e, sizeof(e));
		const size_t bytes = (size_t)e.planes * e.plane_bytes;
		da_resize(s->rot, bytes);
		deque_pop_front(&s->store, s->rot.array, bytes);
		if (!placed && e.pos > pos) {
			deque_push_back(&s->store, &slot, sizeof(slot));
			for (uint32_t i = 0; i < slot.planes; i++)
				deque_push_back(&s->store, audio->data[i], slot.plane_bytes);
			placed = true;
		}
		deque_push_back(&s->store, &e, sizeof(e));
		deque_push_back(&s->store, s->rot.array, bytes);
	}
	if (append || !placed) {
		deque_push_back(&s->store, &slot, sizeof(slot));
		for (uint32_t i = 0; i < slot.planes; i++)
			deque_push_back(&s->store, audio->data[i], slot.plane_bytes);
		s->store_back_pos = pos;
	}
	s->store_count++;
	return true;
}

/* camera-box issue 1439 (ROZHODNUTE 6352253147 a): the run of slots handed to libobs without their packet,
 * counted at their deadline (the timeline's lost) and logged ONCE when the run ends -- at the next packet's
 * slot, or when the stretch stops emitting -- never when the receive side first sees a gap, which a reorder
 * may still fill. Worker thread only, after the lock. */
static void vban_src_loss_run(struct vban_src_s *s, const struct vban_recv_due *d, uint32_t next_nu)
{
	if (d->kind == VBAN_RECV_DUE_SILENCE || d->kind == VBAN_RECV_DUE_CONCEAL) {
		/* a run is concealed or silent as a whole: the timeline decides it at its first slot */
		if (!s->run_slots)
			s->run_concealed = d->kind == VBAN_RECV_DUE_CONCEAL;
		s->run_slots++;
		return;
	}
	if (!s->run_slots || (d->kind == VBAN_RECV_DUE_NONE && d->next_ns != 0))
		return;
	const char *what = s->run_concealed ? "concealed: the previous packet repeated, crossfaded"
					    : "silence for those slots";
	if (d->kind == VBAN_RECV_DUE_PACKET)
		blog(LOG_WARNING, "source '%s': missing %" PRIu64 " packet(s) before frame %u -- %s",
		     obs_source_get_name(s->context), s->run_slots, next_nu, what);
	else
		blog(LOG_WARNING, "source '%s': missing %" PRIu64 " packet(s) before the stretch ended -- %s",
		     obs_source_get_name(s->context), s->run_slots, what);
	s->run_slots = 0;
}

/* camera-box issue 1439 (ROZHODNUTE 6352253147 b): the packet AUDIO (HEAD's) just handed to libobs is kept as
 * the previous packet a concealment repeats -- the output buffer and the kept one swap, no copy -- and its
 * last frame as where the next concealment fades in from. Worker thread only. */
static void vban_src_keep_prev(struct vban_src_s *s, const struct vban_src_slot *head,
			       const struct obs_source_audio *audio)
{
	for (size_t ch = 0; ch < (size_t)audio->speakers && ch < MAX_AV_PLANES; ch++)
		s->tail[ch] = vban_src_sample_get(audio, ch, audio->frames - 1);
	const struct darray kept = s->prev.da;
	s->prev.da = s->out.da;
	s->out.da = kept;
	s->prev_slot = *head;
	s->prev_valid = true;
}

/* camera-box issue 1439 (ROZHODNUTE 6352253147 b): a CONCEAL slot -- the previous packet's samples, faded in
 * from the last frame handed to libobs and (TOWARD_NEXT) out toward NEXT_FIRST, the first frame of the packet
 * that ends the run (vban-conceal.h) -- in the previous packet's own format, at the slot's stamp. Returns false
 * when there is no previous packet of the slot's length (the timeline conceals only right after one of this
 * stretch's packets went out, so never): the caller sends silence. Worker thread only, after the lock. */
static bool vban_src_output_conceal(struct vban_src_s *s, const struct vban_recv_due *d, const double *next_first)
{
	const struct vban_src_slot *p = &s->prev_slot;
	if (!s->prev_valid || (uint64_t)p->frames != d->frames)
		return false;
	struct obs_source_audio prev = {
		.frames = p->frames,
		.speakers = (enum speaker_layout)p->speakers,
		.format = (enum audio_format)p->format,
		.samples_per_sec = p->samples_per_sec,
		.timestamp = d->stamp_ns,
	};
	struct obs_source_audio audio = prev;
	da_resize(s->conceal, (size_t)p->planes * p->plane_bytes);
	for (uint32_t k = 0; k < p->planes && k < MAX_AV_PLANES; k++) {
		prev.data[k] = s->prev.array + (size_t)k * p->plane_bytes;
		audio.data[k] = s->conceal.array + (size_t)k * p->plane_bytes;
	}
	const uint32_t fade = vban_conceal_fade_frames(p->frames, p->samples_per_sec);
	const size_t channels = (size_t)p->speakers;
	for (size_t ch = 0; ch < channels && ch < MAX_AV_PLANES; ch++) {
		for (uint32_t i = 0; i < p->frames; i++) {
			const double v = vban_conceal_sample(vban_src_sample_get(&prev, ch, i), s->tail[ch],
							     next_first[ch], d->toward_next != 0, i, p->frames, fade);
			vban_src_sample_put(prev.format, channels, s->conceal.array, p->plane_bytes, ch, i, v);
		}
		s->tail[ch] = vban_src_sample_get(&audio, ch, p->frames - 1);
	}
	obs_source_output_audio(s->context, &audio);
	return true;
}

/* camera-box issue 1439 (ROZHODNUTE 6347872706): the playout worker's step: everything due at NOW is
 * decided under the lock (vban_recv_due) and handed to libobs outside it -- the lead-in, a slot's stored
 * packet, or a slot of silence. Returns the next deadline, 0 when there is none (idle). The worker
 * thread calls it in its loop; the lift-and-run test driver calls it on simulated time. */
static uint64_t vban_src_worker_step(struct vban_src_s *s, uint64_t now)
{
	for (;;) {
		struct vban_src_slot head = {0};
		pthread_mutex_lock(&s->mutex);
		if (os_atomic_exchange_bool(&s->timeline_reset, false))
			vban_src_reset_locked(s);
		/* a stored packet whose slot already went out cannot exist (only an in-time packet is stored,
		 * for a slot not yet handed out); dropped all the same, so it can never block the store */
		while (s->store_count > 0) {
			deque_peek_front(&s->store, &head, sizeof(head));
			if (!s->timeline.emitted || head.pos >= s->timeline.emit_pos)
				break;
			deque_pop_front(&s->store, NULL, sizeof(head) + (size_t)head.planes * head.plane_bytes);
			s->store_count--;
		}
		const bool has_head = s->store_count > 0;
		if (!has_head)
			head.pos = 0;
		const struct vban_recv_due d = vban_recv_due(&s->timeline, now, has_head, head.pos);
		if (d.kind == VBAN_RECV_DUE_PACKET) {
			const size_t bytes = (size_t)head.planes * head.plane_bytes;
			deque_pop_front(&s->store, NULL, sizeof(head));
			da_resize(s->out, bytes);
			deque_pop_front(&s->store, s->out.array, bytes);
			s->store_count--;
		}
		if ((d.kind == VBAN_RECV_DUE_PACKET || d.kind == VBAN_RECV_DUE_SILENCE ||
		     d.kind == VBAN_RECV_DUE_CONCEAL) &&
		    (!s->emit_late_seen || d.late_ns > s->emit_late_max_ns)) {
			s->emit_late_max_ns = d.late_ns;
			s->emit_late_seen = true;
		}
		const enum audio_format format = s->out_format;
		const enum speaker_layout speakers = s->out_speakers;
		const uint32_t rate = s->out_rate;
		s->worker_idle = d.kind == VBAN_RECV_DUE_NONE && d.next_ns == 0;
		pthread_mutex_unlock(&s->mutex);

		vban_src_loss_run(s, &d, head.nu);
		if (d.kind == VBAN_RECV_DUE_NONE)
			return d.next_ns;
		if (d.kind == VBAN_RECV_DUE_PACKET) {
			struct obs_source_audio audio = {
				.frames = head.frames,
				.speakers = (enum speaker_layout)head.speakers,
				.format = (enum audio_format)head.format,
				.samples_per_sec = head.samples_per_sec,
				.timestamp = d.stamp_ns,
			};
			for (uint32_t i = 0; i < head.planes && i < MAX_AV_PLANES; i++)
				audio.data[i] = s->out.array + (size_t)i * head.plane_bytes;
			obs_source_output_audio(s->context, &audio);
			vban_src_keep_prev(s, &head, &audio);
		} else if (d.kind != VBAN_RECV_DUE_CONCEAL || !vban_src_output_conceal(s, &d, head.first)) {
			/* the lead-in, or a slot whose packet is missing and not concealed (a run of more than
			 * VBAN_RECV_CONCEAL_MAX_SLOTS, or nothing stored behind it at its deadline): silence of
			 * exactly that length. A concealment fades toward the store's head, the packet that ends
			 * its run (ROZHODNUTE 6352253147 b). */
			vban_src_output_silence(s, format, speakers, rate, d.frames, d.stamp_ns);
		}
	}
}

#ifndef VBAN_SRC_SPAWN_WORKER
static void *vban_src_worker_main(void *data);
/* the worker thread's spawn and join (the lift-and-run test driver runs the step on simulated time) */
#define VBAN_SRC_SPAWN_WORKER(s) (pthread_create(&(s)->worker, NULL, vban_src_worker_main, (s)) == 0)
#define VBAN_SRC_JOIN_WORKER(s) pthread_join((s)->worker, NULL)
#endif

/* camera-box issue 1439 (ROZHODNUTE 6347872706): the playout worker, one per source. It sleeps to its
 * next deadline (an absolute target on the OBS clock, at most VBAN_SRC_WORKER_SLEEP_MAX_NS at a time) and
 * waits on WORKER_WAKE while it has nothing to emit; it never holds the lock across an output. */
static void *vban_src_worker_main(void *data)
{
	struct vban_src_s *s = data;
	os_set_thread_name("vban-src-out");
	/* camera-box issue 1439 (review round 5): wake on time -- the kernel's default 50 us timer slack would
	 * batch every look of this thread */
#ifdef __linux__
	prctl(PR_SET_TIMERSLACK, 1UL, 0UL, 0UL, 0UL);
#endif
	while (!os_atomic_load_bool(&s->worker_stop)) {
		const uint64_t next = vban_src_worker_step(s, os_gettime_ns());
		if (os_atomic_load_bool(&s->worker_stop))
			break;
		if (next == 0) {
			os_event_timedwait(s->worker_wake, VBAN_SRC_WORKER_IDLE_MS);
			continue;
		}
		const uint64_t cap = os_gettime_ns() + VBAN_SRC_WORKER_SLEEP_MAX_NS;
		os_sleepto_ns(next < cap ? next : cap);
	}
	return NULL;
}

/* create / update only */
static void vban_src_worker_start(struct vban_src_s *s)
{
	if (s->worker_running)
		return;
	os_atomic_set_bool(&s->worker_stop, false);
	s->worker_running = VBAN_SRC_SPAWN_WORKER(s);
	if (!s->worker_running)
		blog(LOG_ERROR, "obs-vban receive worker: source '%s' could not start its playout worker",
		     obs_source_get_name(s->context));
}

/* update / destroy only: the worker is stopped and JOINED before anything it reads changes or is freed */
static void vban_src_worker_stop(struct vban_src_s *s, const char *why)
{
	if (!s->worker_running)
		return;
	os_atomic_set_bool(&s->worker_stop, true);
	os_event_signal(s->worker_wake);
	VBAN_SRC_JOIN_WORKER(s);
	s->worker_running = false;
	blog(LOG_INFO, "obs-vban receive worker: source '%s' stopped (%s)", obs_source_get_name(s->context), why);
}

static void vban_src_callback(const char *buf, size_t buf_len, void *data)
{
	struct vban_src_s *s = data;

	const struct VBanHeader *header = (const struct VBanHeader *)buf;
	const char *payload = buf + VBAN_HEADER_SIZE;
	size_t payload_len = buf_len - VBAN_HEADER_SIZE;

	if (header->format_nbc + 1 > 8) {
		blog(LOG_ERROR, "Too many number of channels: %d", header->format_nbc + 1);
		return;
	}

	struct obs_source_audio audio = {
		.frames = header->format_nbs + 1,
		.speakers = header->format_nbc + 1,
		.samples_per_sec = VBanSRList[header->format_SR & VBAN_SR_MASK],
	};

	size_t len_exp =
		VBanBitResolutionSize[header->format_bit & VBAN_BIT_RESOLUTION_MASK] * audio.frames * audio.speakers;
	if (payload_len < len_exp) {
		blog(LOG_ERROR, "Too small payload size %d, expected %d", (int)payload_len,
		     VBanBitResolutionSize[header->format_bit & VBAN_BIT_RESOLUTION_MASK] * audio.frames *
			     (int)audio.speakers);
		return;
	}

	switch (header->format_bit) {
	case VBAN_BITFMT_8_INT:
		audio.format = AUDIO_FORMAT_U8BIT;
		audio.data[0] = (const uint8_t *)payload;
		break;
	case VBAN_BITFMT_16_INT:
		audio.format = AUDIO_FORMAT_16BIT;
		audio.data[0] = (const uint8_t *)payload;
		break;
	case VBAN_BITFMT_24_INT:
		audio.format = AUDIO_FORMAT_FLOAT_PLANAR;
		convert_24le_to_fltp(s, &audio, payload);
		break;
	case VBAN_BITFMT_32_INT:
		audio.format = AUDIO_FORMAT_32BIT;
		audio.data[0] = (const uint8_t *)payload;
		break;
	case VBAN_BITFMT_32_FLOAT:
		audio.format = AUDIO_FORMAT_FLOAT;
		audio.data[0] = (const uint8_t *)payload;
		break;
	default:
		blog(LOG_ERROR, "Unsupported format %d", header->format_bit);
		return;
	}

	/* camera-box issue 1439: the receive timeline (vban-receive-timeline.h) decides what the packet is --
	 * held, in time (stored for its slot), late, stale or a new anchor -- and never outputs anything: the
	 * playout worker hands every slot to libobs by its deadline (ROZHODNUTE 6347872706), on a FIXED line
	 * that follows the sender's clock, R ahead of its fitted arrival. Only the decision and the copy are
	 * under the lock; the lines are logged after it. */
	const uint64_t now = os_gettime_ns();
	const uint64_t reserve_ns = (uint64_t)os_atomic_load_long(&s->receive_buffer_ms) * VBAN_RECV_NS_PER_MS;
	pthread_mutex_lock(&s->mutex);
	if (os_atomic_exchange_bool(&s->timeline_reset, false))
		vban_src_reset_locked(s);
	const bool emitted_before = s->timeline.emitted;
	const uint32_t frames_before = s->timeline.frames;
	const uint32_t rate_before = s->timeline.rate;
	const uint32_t channels_before = s->timeline.channels;
	const struct vban_recv_decision d = vban_recv_packet(&s->timeline, now, header->nuFrame, audio.frames,
							     audio.samples_per_sec, (uint32_t)audio.speakers,
							     reserve_ns);
	bool store_full = false, wake = false;
	if (d.event == VBAN_RECV_ANCHOR || d.event == VBAN_RECV_REANCHOR)
		vban_src_store_clear_locked(s);
	if (d.event != VBAN_RECV_STALE) {
		s->out_format = audio.format;
		s->out_speakers = audio.speakers;
		s->out_rate = audio.samples_per_sec;
	}
	/* an in-time packet, or a reordered one whose slot is still ahead (ROZHODNUTE 6352253147 a) */
	if (d.event == VBAN_RECV_PACKET || d.event == VBAN_RECV_REORDER) {
		if (vban_src_store_push_locked(s, d.pos, header->nuFrame, &audio)) {
			wake = s->worker_idle;
			s->store_full = false;
		} else {
			store_full = !s->store_full;
			s->store_full = true;
		}
	}
	const bool first_output = !emitted_before && s->timeline.emitted;
	const bool status = vban_src_status_due(s, now);
	struct vban_src_snapshot n = vban_src_snapshot_locked(s);
	if (status) {
		s->emit_late_seen = false;
		s->emit_late_max_ns = 0;
	}
	pthread_mutex_unlock(&s->mutex);

	if (wake)
		os_event_signal(s->worker_wake);
	if (store_full)
		blog(LOG_ERROR,
		     "obs-vban receive store: source '%s' frame=%u -- full (%u packets): the playout worker is not "
		     "emitting, the packet is dropped",
		     obs_source_get_name(s->context), header->nuFrame, VBAN_SRC_STORE_MAX);
	if (d.event != VBAN_RECV_STALE) {
		const struct vban_src_arrival a = {
			.nu_frame = header->nuFrame,
			.frames = audio.frames,
			.now = now,
			.first_output = first_output,
			.emitted_before = emitted_before,
			.frames_before = frames_before,
			.rate_before = rate_before,
			.channels_before = channels_before,
		};
		vban_src_report(s, &d, &n, &a);
	}
	if (status)
		vban_src_log_status(s, header, &n);
}
