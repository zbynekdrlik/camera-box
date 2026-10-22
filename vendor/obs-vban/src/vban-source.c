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
#include "plugin-macros.generated.h"
#include "vban-udp.h"
#include "vban.h"
#include "vban-receive-timeline.h"

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
	/* camera-box issue 1439: one chunk of silence in the stream's own sample format (receive thread) */
	DARRAY(uint8_t) silence;
	/* camera-box issue 1439: the receive timeline; touched only by the receive callback */
	struct vban_recv_timeline timeline;
	uint64_t status_ns;        /* the last status line, 0 = none yet */
	int64_t window_late_max_ns; /* the largest arrival - stamp since the last status line */
	bool window_late_seen;
	/* camera-box issue 1439 (ROZHODNUTE 6345255813): the source emits since its anchor's first second */
	bool emitting;
	/* camera-box issue 1439 (ROZHODNUTE 6345255813 item 2): a run of late (dropped) packets is logged once */
	bool late_run;
	/* camera-box issue 1439 (ROZHODNUTE 6343922049): the (R, effective R) pair last warned about, so a
	 * Receive Buffer lifted to one packet + 10 ms is named once, not at every re-anchor */
	uint64_t warned_reserve_ns;
	uint64_t warned_reserve_eff_ns;
};

static const char *vban_src_get_name(void *type_data)
{
	UNUSED_PARAMETER(type_data);
	return obs_module_text("VBAN.src");
}

static void vban_src_callback(const char *buf, size_t buf_len, void *data);

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
	vban_recv_init(&s->timeline);

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

	const struct vban_recv_timeline *t = &s->timeline;
	blog(t->lost || t->re_anchors ? LOG_ERROR : LOG_INFO,
	     "source '%s': received %" PRIu64 " packets, lost %" PRIu64 ", late %" PRIu64 ", %" PRIu64
	     " re-anchor(s)",
	     obs_source_get_name(s->context), t->received, t->lost, t->late, t->re_anchors);

	bfree(s->stream_name);
	bfree(s->ip_from);
	da_free(s->buffer);
	da_free(s->silence);
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

/* camera-box issue 1439: the anchor / re-anchor / loss lines, and the lateness record of the window. */
static void vban_src_report(struct vban_src_s *s, const struct vban_recv_decision *d, uint32_t nu_frame,
			    uint32_t frames)
{
	const struct vban_recv_timeline *t = &s->timeline;
	/* a new line: is R lifted to one packet + the margin (R is measured from the packet's start)? */
	if ((d->event == VBAN_RECV_ANCHOR || d->event == VBAN_RECV_REANCHOR) && t->reserve_eff_ns != t->reserve_ns &&
	    (t->reserve_ns != s->warned_reserve_ns || t->reserve_eff_ns != s->warned_reserve_eff_ns)) {
		s->warned_reserve_ns = t->reserve_ns;
		s->warned_reserve_eff_ns = t->reserve_eff_ns;
		blog(LOG_WARNING,
		     "obs-vban receive buffer: source '%s' receive_buffer_ms=%" PRIu64
		     " is not above one packet (%.2f ms) + %d ms: using %.2f ms",
		     obs_source_get_name(s->context), (uint64_t)(t->reserve_ns / VBAN_RECV_NS_PER_MS),
		     (double)vban_recv_samples_to_ns(frames, t->rate) / 1e6, VBAN_RECV_RESERVE_MARGIN_MS,
		     (double)t->reserve_eff_ns / 1e6);
	}
	if (d->event == VBAN_RECV_ANCHOR || d->event == VBAN_RECV_REANCHOR) {
		s->emitting = false;
		s->late_run = false;
	}
	if ((d->event == VBAN_RECV_PACKET || (d->event == VBAN_RECV_LATE && d->silence_samples > 0)) && !s->emitting) {
		/* the hold is over: the first output (a packet, or a late packet's silence), on the anchor's
		 * final (earliest-arrival) line */
		s->emitting = true;
		blog(LOG_INFO,
		     "obs-vban receive emitting: source '%s' frame=%u margin_ms=%.1f after the first second's hold",
		     obs_source_get_name(s->context), nu_frame, (double)-d->lateness_ns / 1e6);
	}
	if (d->event == VBAN_RECV_ANCHOR) {
		blog(LOG_INFO,
		     "obs-vban receive anchored: source '%s' frame=%u rate=%u ch=%u receive_buffer_ms=%" PRIu64
		     " -- holding its first second",
		     obs_source_get_name(s->context), nu_frame, t->rate, t->channels,
		     (uint64_t)(t->reserve_ns / VBAN_RECV_NS_PER_MS));
	} else if (d->event == VBAN_RECV_REANCHOR) {
		/* a new timeline from this packet, then the hold; a starve (no packet in time for a second)
		 * names how long that was */
		blog(LOG_WARNING,
		     "obs-vban receive re-anchor: source '%s' reason=%s frame=%u starve_ms=%.1f re_anchors=%" PRIu64
		     " late=%" PRIu64 " -- holding its first second",
		     obs_source_get_name(s->context), vban_recv_reason_name(d->reason), nu_frame,
		     (double)d->late_ns / 1e6, t->re_anchors, t->late);
	}
	if (d->event == VBAN_RECV_LATE && !s->late_run) {
		/* ROZHODNUTE 6345255813 item 2: a late packet is lost, never re-timed; one line per late run */
		s->late_run = true;
		blog(LOG_WARNING,
		     "obs-vban receive late: source '%s' frame=%u late_ms=%.1f -- dropped, its slot is silence on the "
		     "anchored line (late=%" PRIu64 ")",
		     obs_source_get_name(s->context), nu_frame, (double)d->late_ns / 1e6, t->late);
	} else if (d->event == VBAN_RECV_PACKET) {
		s->late_run = false;
	}
	/* the lost slots (a LATE packet's own slot is in the silence too, logged above) */
	const uint64_t lost_slots = d->silence_samples / (frames ? frames : 1u) - (d->event == VBAN_RECV_LATE ? 1u : 0u);
	if (d->silence_samples > 0 && lost_slots > 0)
		blog(LOG_WARNING, "source '%s': missing %" PRIu64 " packet(s) at frame %u -- silence for those slots",
		     obs_source_get_name(s->context), lost_slots, nu_frame);
	/* the lateness record includes the late packets that were dropped (the measure R is chosen by) */
	if ((d->event == VBAN_RECV_PACKET || d->event == VBAN_RECV_LATE) &&
	    (!s->window_late_seen || d->lateness_ns > s->window_late_max_ns)) {
		s->window_late_max_ns = d->lateness_ns;
		s->window_late_seen = true;
	}
}

/* camera-box issue 1439: the lost slots as silence on the timeline, in pieces of at most
 * VBAN_RECV_SILENCE_CHUNK_FRAMES frames, in the SAME sample format and layout as the stream's packets
 * (LIKE): libobs resets the source's resampler, and with it the genlock ASRC, whenever the format of
 * the audio it is handed changes. Unsigned 8-bit silence is 0x80, every other format's is 0. */
static void vban_src_output_silence(struct vban_src_s *s, const struct vban_recv_decision *d,
				    const struct obs_source_audio *like)
{
	const bool planar = is_audio_planar(like->format);
	const size_t channels = (size_t)like->speakers;
	const size_t plane_bytes = VBAN_RECV_SILENCE_CHUNK_FRAMES * get_audio_bytes_per_channel(like->format) *
				   (planar ? 1 : channels);
	da_resize(s->silence, plane_bytes);
	memset(s->silence.array, like->format == AUDIO_FORMAT_U8BIT ? 0x80 : 0, plane_bytes);
	struct obs_source_audio silence = {
		.speakers = like->speakers,
		.format = like->format,
		.samples_per_sec = like->samples_per_sec,
	};
	for (size_t ch = 0; ch < (planar ? channels : 1) && ch < MAX_AV_PLANES; ch++)
		silence.data[ch] = s->silence.array;
	for (uint64_t done = 0; done < d->silence_samples;) {
		uint64_t n = d->silence_samples - done;
		if (n > VBAN_RECV_SILENCE_CHUNK_FRAMES)
			n = VBAN_RECV_SILENCE_CHUNK_FRAMES;
		silence.frames = (uint32_t)n;
		silence.timestamp = vban_recv_stamp_at(&s->timeline, d->silence_pos + done);
		obs_source_output_audio(s->context, &silence);
		done += n;
	}
}

/* camera-box issue 1439: one status line every VBAN_RECV_LOG_INTERVAL_NS while packets arrive. The
 * counters are cumulative since the source started (or was reconfigured); max_late_ms is the window's
 * lateness record (arrival - stamp; negative = the margin R left), the measure R is chosen by. The
 * stream name is the packet's own: s->stream_name is the UI thread's, and update() may free it. */
static void vban_src_log_status(struct vban_src_s *s, const struct VBanHeader *header, uint64_t now)
{
	if (s->status_ns == 0) {
		s->status_ns = now;
		return;
	}
	if (now - s->status_ns < VBAN_RECV_LOG_INTERVAL_NS)
		return;
	s->status_ns = now;
	const struct vban_recv_timeline *t = &s->timeline;
	const double rate = t->rate ? (double)t->rate : 1.0;
	blog(LOG_INFO,
	     "obs-vban receive: received=%" PRIu64 " lost=%" PRIu64 " late=%" PRIu64 " re_anchors=%" PRIu64
	     " stale=%" PRIu64 " silence_ms=%.1f max_late_ms=%.1f sender_ppm=%+.3f phase_ms=%+.2f fit=%s"
	     " receive_buffer_ms=%" PRIu64
	     " rate=%u ch=%u port=%ld stream='%.*s' source='%s'",
	     t->received, t->lost, t->late, t->re_anchors, t->stale, (double)t->silence_samples * 1000.0 / rate,
	     s->window_late_seen ? (double)s->window_late_max_ns / 1e6 : 0.0,
	     t->fit_valid ? -(double)t->fit_ppb / 1000.0 : 0.0, (double)t->phase_ns / 1e6,
	     vban_recv_fit_frozen(t) ? "frozen" : (t->fit_valid ? "locked" : "acquiring"), (uint64_t)(t->reserve_ns / VBAN_RECV_NS_PER_MS), t->rate, t->channels,
	     os_atomic_load_long(&s->log_port), (int)VBAN_STREAM_NAME_SIZE, header->streamname,
	     obs_source_get_name(s->context));
	s->window_late_seen = false;
	s->window_late_max_ns = 0;
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

	/* camera-box issue 1439: the stamp comes from the receive timeline (vban-receive-timeline.h), never
	 * from the arrival: a fixed timeline that follows the sender's clock, R ahead of its fitted
	 * arrival, so the audio is in OBS's buffer before the mix needs it. A lost slot is silence. */
	if (os_atomic_exchange_bool(&s->timeline_reset, false))
		vban_recv_init(&s->timeline);
	const uint64_t now = os_gettime_ns();
	const uint64_t reserve_ns = (uint64_t)os_atomic_load_long(&s->receive_buffer_ms) * VBAN_RECV_NS_PER_MS;
	const struct vban_recv_decision d = vban_recv_packet(&s->timeline, now, header->nuFrame, audio.frames,
							     audio.samples_per_sec, (uint32_t)audio.speakers, reserve_ns);
	if (d.event == VBAN_RECV_STALE) {
		/* a duplicate / reordered packet whose slot already went out (as silence): dropped */
		vban_src_log_status(s, header, now);
		return;
	}
	vban_src_report(s, &d, header->nuFrame, audio.frames);
	/* ROZHODNUTE 6345255813: an anchor, a re-anchor and every packet of their first second are HELD
	 * (never output), so libobs never sees the earliest-arrival refinement as a stamp step (item 1); a
	 * LATE packet's audio is dropped and its slot goes out as silence on the unchanged line (item 2) */
	if (d.event == VBAN_RECV_PACKET || d.event == VBAN_RECV_LATE) {
		if (d.silence_samples > 0)
			vban_src_output_silence(s, &d, &audio);
		if (d.event == VBAN_RECV_PACKET) {
			audio.timestamp = d.stamp_ns;
			obs_source_output_audio(s->context, &audio);
		}
	}
	vban_src_log_status(s, header, now);
}
