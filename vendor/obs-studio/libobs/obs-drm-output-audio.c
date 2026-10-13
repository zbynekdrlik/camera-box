/*
 * camera-box issue 1424 -- the fixed HDMI output's audio leg. See obs-drm-output-audio.h for the
 * contract; the pure decisions live in obs-drm-output-audio-match.h (Rust twin + parity gate:
 * src/drm_output_audio_match.rs, tests/drm_output_audio_1424.rs).
 *
 * Threads:
 *   - the resolver (one per open leg): reads the ELDs, re-reading for at most
 *     DRM_OUTPUT_AUDIO_RESOLVE_WINDOW_MS (the NVIDIA HDA driver publishes a connector's ELD only
 *     once a mode is driven on it), turns on the matched PCM's IEC958 switch, opens + configures
 *     hw:<card>,<device>, waits for libobs's audio output and connects the mix-0 callback, then stays
 *     as the leg's watcher (re-checks every 5 s that the PCM still carries the connector's monitor);
 *   - libobs's audio thread: drm_output_audio_on_mix() places every block and writes it into the
 *     PCM (non-blocking: the audio thread never waits on the device);
 *   - the output's scanout thread + the graphics thread: the present clock notes.
 * close() joins the resolver first, then disconnects the callback (audio_output_disconnect waits
 * for a running callback), then closes the PCM: nothing touches the leg after it returns.
 */

#if defined(__linux__)

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <sound/asound.h>
#include <xcb/randr.h>
#include <xcb/xcb.h>

#include "obs.h"
#include "obs-drm-output.h"
#include "obs-drm-output-audio.h"
#include "obs-drm-output-audio-match.h"
#include "util/bmem.h"
#include "util/platform.h"
#include "util/threading.h"

/* -------------------------------------------------------------------------------------------------
 * The present clock (render tick -> on screen) of one output (issue 1424 part 2: each instance owns
 * one, struct drm_output_inst::clock).
 * ------------------------------------------------------------------------------------------------- */

void drm_output_present_note_published(struct drm_output_present_clock *c, int idx)
{
	if (!c || idx < 0 || idx >= DRM_OUTPUT_PRESENT_SLOTS)
		return;
	os_atomic_set_long(&c->tick_ns[idx], (long)obs_get_video_frame_time());
}

void drm_output_present_note_shown(struct drm_output_present_clock *c, int idx, uint64_t shown_ns)
{
	if (!c || idx < 0 || idx >= DRM_OUTPUT_PRESENT_SLOTS)
		return;
	/* consume the tick: a buffer re-shown with no new publish counts nothing */
	const long tick = os_atomic_exchange_long(&c->tick_ns[idx], 0);
	if (tick <= 0 || shown_ns <= (uint64_t)tick)
		return;
	const uint64_t prev = (uint64_t)os_atomic_load_long(&c->delay_ns);
	os_atomic_set_long(&c->delay_ns, (long)drm_output_present_ewma(prev, shown_ns - (uint64_t)tick));
}

/* -------------------------------------------------------------------------------------------------
 * The two match sources: the connector's EDID (X RandR) and every PCM's ELD (ALSA control).
 * ------------------------------------------------------------------------------------------------- */

size_t drm_output_audio_read_edid(const char *output_name, uint8_t *buf, size_t cap)
{
	if (!output_name || !buf || cap == 0)
		return 0;
	xcb_connection_t *conn = xcb_connect(NULL, NULL);
	if (!conn || xcb_connection_has_error(conn)) {
		if (conn)
			xcb_disconnect(conn);
		return 0;
	}
	size_t got = 0;
	const size_t name_len = strlen(output_name);
	xcb_intern_atom_reply_t *atom = xcb_intern_atom_reply(conn, xcb_intern_atom(conn, 1, 4, "EDID"), NULL);
	if (atom && atom->atom != XCB_ATOM_NONE) {
		xcb_screen_iterator_t it = xcb_setup_roots_iterator(xcb_get_setup(conn));
		for (; it.rem > 0 && got == 0; xcb_screen_next(&it)) {
			xcb_randr_get_screen_resources_current_reply_t *res = xcb_randr_get_screen_resources_current_reply(
				conn, xcb_randr_get_screen_resources_current(conn, it.data->root), NULL);
			if (!res)
				continue;
			xcb_randr_output_t *outs = xcb_randr_get_screen_resources_current_outputs(res);
			const int n = xcb_randr_get_screen_resources_current_outputs_length(res);
			for (int i = 0; i < n && got == 0; i++) {
				xcb_randr_get_output_info_reply_t *info = xcb_randr_get_output_info_reply(
					conn, xcb_randr_get_output_info(conn, outs[i], res->config_timestamp), NULL);
				if (!info)
					continue;
				const int nl = xcb_randr_get_output_info_name_length(info);
				const bool hit = nl >= 0 && (size_t)nl == name_len &&
						 memcmp(xcb_randr_get_output_info_name(info), output_name, name_len) == 0;
				free(info);
				if (!hit)
					continue;
				/* 128 x 32 bit = 512 bytes: the base block + 3 extensions */
				xcb_randr_get_output_property_reply_t *prop = xcb_randr_get_output_property_reply(
					conn,
					xcb_randr_get_output_property(conn, outs[i], atom->atom, XCB_ATOM_ANY, 0, 128, 0, 0),
					NULL);
				if (prop) {
					const int len = xcb_randr_get_output_property_data_length(prop);
					if (prop->format == 8 && len > 0) {
						got = (size_t)len < cap ? (size_t)len : cap;
						memcpy(buf, xcb_randr_get_output_property_data(prop), got);
					}
					free(prop);
				}
				break; /* the named output answered (with or without an EDID) */
			}
			free(res);
		}
	}
	free(atom);
	xcb_disconnect(conn);
	return got;
}

#define DRM_OUTPUT_AUDIO_MAX_PCMS 32

/* Every PCM's ELD from the per-PCM ALSA control (iface PCM, name "ELD", device = the PCM number):
 * the kernel's own pcm -> pin map. An empty ELD (no monitor, or no mode driven yet) is recorded
 * with an invalid id, so "no monitor at all" stays distinguishable from "no matching monitor". */
static size_t drm_output_audio_read_elds(struct drm_output_audio_pcm *out, size_t cap)
{
	size_t n = 0;
	for (int card = 0; card < 32 && n < cap; card++) {
		char path[64];
		snprintf(path, sizeof(path), "/dev/snd/controlC%d", card);
		const int fd = open(path, O_RDONLY | O_CLOEXEC);
		if (fd < 0)
			continue;
		struct snd_ctl_elem_list list;
		memset(&list, 0, sizeof(list));
		if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_LIST, &list) < 0 || list.count == 0) {
			close(fd);
			continue;
		}
		struct snd_ctl_elem_id *ids = bzalloc(sizeof(*ids) * list.count);
		list.space = list.count;
		list.pids = ids;
		if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_LIST, &list) == 0) {
			for (unsigned int i = 0; i < list.used && n < cap; i++) {
				if (ids[i].iface != SNDRV_CTL_ELEM_IFACE_PCM || strcmp((const char *)ids[i].name, "ELD") != 0)
					continue;
				struct snd_ctl_elem_info info;
				memset(&info, 0, sizeof(info));
				info.id = ids[i];
				if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_INFO, &info) < 0 || info.type != SNDRV_CTL_ELEM_TYPE_BYTES)
					continue;
				struct snd_ctl_elem_value *v = bzalloc(sizeof(*v));
				v->id = ids[i];
				if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_READ, v) == 0) {
					size_t len = info.count;
					if (len > sizeof(v->value.bytes.data))
						len = sizeof(v->value.bytes.data);
					out[n].card = card;
					out[n].device = (int)ids[i].device;
					(void)drm_output_eld_monitor_id(v->value.bytes.data, len, &out[n].id);
					n++;
				}
				bfree(v);
			}
		}
		bfree(ids);
		close(fd);
	}
	return n;
}

/* How many "IEC958 Playback Switch" controls the card on `fd` has (0 on a read failure). */
static size_t drm_output_audio_iec958_count(int fd)
{
	struct snd_ctl_elem_list list;
	memset(&list, 0, sizeof(list));
	if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_LIST, &list) < 0 || list.count == 0)
		return 0;
	struct snd_ctl_elem_id *ids = bzalloc(sizeof(*ids) * list.count);
	list.space = list.count;
	list.pids = ids;
	size_t n = 0;
	if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_LIST, &list) == 0) {
		for (unsigned int i = 0; i < list.used; i++)
			if (ids[i].iface == SNDRV_CTL_ELEM_IFACE_MIXER &&
			    strcmp((const char *)ids[i].name, "IEC958 Playback Switch") == 0)
				n++;
	}
	bfree(ids);
	return n;
}

/* Turn on the "IEC958 Playback Switch" of PCM `device` on `card` (index = the PCM's rank among the
 * card's HDMI PCMs, drm_output_audio_iec958_index) -- only when the card's switches are exactly its
 * HDMI PCMs; an extra switch (an analog codec's S/PDIF on the same card) leaves it untouched and named.
 * No sound server unmutes it any more, so a switch left off would make the leg write silence. Writes
 * a short note for the open line into `note` (size `cap`). */
static void drm_output_audio_iec958_on(int card, const struct drm_output_audio_pcm *pcms, size_t n, int device,
				       char *note, size_t cap)
{
	int devices[DRM_OUTPUT_AUDIO_MAX_PCMS];
	size_t nd = 0;
	for (size_t i = 0; i < n; i++) {
		if (pcms[i].card == card)
			devices[nd++] = pcms[i].device;
	}
	char path[64];
	snprintf(path, sizeof(path), "/dev/snd/controlC%d", card);
	const int fd = open(path, O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		snprintf(note, cap, "IEC958 switch not writable");
		return;
	}
	const size_t switches = drm_output_audio_iec958_count(fd);
	const int index = drm_output_audio_iec958_index(devices, nd, device, switches);
	if (index < 0) {
		snprintf(note, cap, "IEC958 switch ambiguous (%zu switches, %zu HDMI PCMs) -- untouched", switches, nd);
		close(fd);
		return;
	}
	snprintf(note, cap, "IEC958 switch not found");
	struct snd_ctl_elem_id id;
	memset(&id, 0, sizeof(id));
	id.iface = SNDRV_CTL_ELEM_IFACE_MIXER;
	snprintf((char *)id.name, sizeof(id.name), "%s", "IEC958 Playback Switch");
	id.index = (unsigned int)index;
	struct snd_ctl_elem_info info;
	memset(&info, 0, sizeof(info));
	info.id = id;
	if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_INFO, &info) == 0 && info.type == SNDRV_CTL_ELEM_TYPE_BOOLEAN) {
		struct snd_ctl_elem_value *v = bzalloc(sizeof(*v));
		v->id = info.id;
		if (ioctl(fd, SNDRV_CTL_IOCTL_ELEM_READ, v) == 0) {
			bool all_on = true;
			for (unsigned int c = 0; c < info.count && c < 128; c++) {
				all_on = all_on && v->value.integer.value[c] != 0;
				v->value.integer.value[c] = 1;
			}
			if (all_on)
				snprintf(note, cap, "IEC958 switch on");
			else
				snprintf(note, cap, "%s",
					 ioctl(fd, SNDRV_CTL_IOCTL_ELEM_WRITE, v) == 0 ? "IEC958 switch was off -> on"
										       : "IEC958 switch off, write FAILED");
		}
		bfree(v);
	}
	close(fd);
}

/* -------------------------------------------------------------------------------------------------
 * alsa-lib, dlopen'd (the vk-direct Vulkan loader pattern): no link dependency, a box without
 * libasound.so.2 keeps the picture and logs why there is no audio.
 * ------------------------------------------------------------------------------------------------- */

/* alsa-lib's prototypes are declared here, not included: <alsa/asoundlib.h> clashes with the kernel
 * uapi <sound/asound.h> the control ioctls above need, and the library is dlopen'd anyway. The
 * signatures are alsa-lib's (enums travel as int); the values are alsa-lib's / the kernel's. */
struct drm_output_snd_pcm;
struct drm_output_snd_hw;
#define DRM_OUTPUT_SND_PCM_STREAM_PLAYBACK 0  /* SND_PCM_STREAM_PLAYBACK */
#define DRM_OUTPUT_SND_PCM_NONBLOCK 0x0001    /* SND_PCM_NONBLOCK */
#define DRM_OUTPUT_SND_PCM_ACCESS_RW_INTERLEAVED 3
#define DRM_OUTPUT_SND_PCM_FORMAT_S16_LE 2
#define DRM_OUTPUT_SND_PCM_FORMAT_S32_LE 10

struct drm_output_alsa {
	void *lib;
	int (*pcm_open)(struct drm_output_snd_pcm **pcm, const char *name, int stream, int mode);
	int (*pcm_close)(struct drm_output_snd_pcm *pcm);
	size_t (*hw_sizeof)(void);
	int (*hw_any)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw);
	int (*hw_access)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw, int access);
	int (*hw_test_format)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw, int format);
	int (*hw_format)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw, int format);
	int (*hw_channels)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw, unsigned int channels);
	int (*hw_rate)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw, unsigned int *rate, int *dir);
	int (*hw_period)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw, unsigned long *frames, int *dir);
	int (*hw_buffer)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw, unsigned long *frames);
	int (*hw_apply)(struct drm_output_snd_pcm *pcm, struct drm_output_snd_hw *hw);
	long (*writei)(struct drm_output_snd_pcm *pcm, const void *buf, unsigned long frames);
	int (*delay)(struct drm_output_snd_pcm *pcm, long *frames);
	int (*recover)(struct drm_output_snd_pcm *pcm, int err, int silent);
	const char *(*strerror_fn)(int err);
};

static bool drm_output_alsa_load(struct drm_output_alsa *al)
{
	memset(al, 0, sizeof(*al));
	al->lib = dlopen("libasound.so.2", RTLD_NOW | RTLD_LOCAL);
	if (!al->lib)
		return false;
#define DRM_OUTPUT_ALSA_SYM(field, name)                                  \
	do {                                                              \
		*(void **)(&al->field) = dlsym(al->lib, name);            \
		if (!al->field) {                                         \
			dlclose(al->lib);                                 \
			al->lib = NULL;                                   \
			return false;                                     \
		}                                                         \
	} while (0)
	DRM_OUTPUT_ALSA_SYM(pcm_open, "snd_pcm_open");
	DRM_OUTPUT_ALSA_SYM(pcm_close, "snd_pcm_close");
	DRM_OUTPUT_ALSA_SYM(hw_sizeof, "snd_pcm_hw_params_sizeof");
	DRM_OUTPUT_ALSA_SYM(hw_any, "snd_pcm_hw_params_any");
	DRM_OUTPUT_ALSA_SYM(hw_access, "snd_pcm_hw_params_set_access");
	DRM_OUTPUT_ALSA_SYM(hw_test_format, "snd_pcm_hw_params_test_format");
	DRM_OUTPUT_ALSA_SYM(hw_format, "snd_pcm_hw_params_set_format");
	DRM_OUTPUT_ALSA_SYM(hw_channels, "snd_pcm_hw_params_set_channels");
	DRM_OUTPUT_ALSA_SYM(hw_rate, "snd_pcm_hw_params_set_rate_near");
	DRM_OUTPUT_ALSA_SYM(hw_period, "snd_pcm_hw_params_set_period_size_near");
	DRM_OUTPUT_ALSA_SYM(hw_buffer, "snd_pcm_hw_params_set_buffer_size_near");
	DRM_OUTPUT_ALSA_SYM(hw_apply, "snd_pcm_hw_params");
	DRM_OUTPUT_ALSA_SYM(writei, "snd_pcm_writei");
	DRM_OUTPUT_ALSA_SYM(delay, "snd_pcm_delay");
	DRM_OUTPUT_ALSA_SYM(recover, "snd_pcm_recover");
	DRM_OUTPUT_ALSA_SYM(strerror_fn, "snd_strerror");
#undef DRM_OUTPUT_ALSA_SYM
	return true;
}

/* -------------------------------------------------------------------------------------------------
 * The leg.
 * ------------------------------------------------------------------------------------------------- */

/* The bounded re-read window after the backend start, and its step. */
#define DRM_OUTPUT_AUDIO_RESOLVE_WINDOW_MS 10000
#define DRM_OUTPUT_AUDIO_RESOLVE_STEP_MS 250
/* The PCM: 1024-frame periods in an 8192-frame ring; the placement keeps about three blocks queued. */
#define DRM_OUTPUT_AUDIO_PERIOD_FRAMES 1024
#define DRM_OUTPUT_AUDIO_BUFFER_FRAMES 8192
#define DRM_OUTPUT_AUDIO_FLOOR_BLOCKS 3
#define DRM_OUTPUT_AUDIO_REPORT_NS 60000000000ULL
/* The drift walk: outside +-5 ms of the floor one frame per block is repeated / dropped (inaudible). */
#define DRM_OUTPUT_AUDIO_BAND_NS 5000000ULL
/* How often the open PCM is re-checked against the connector's monitor (a replug or a re-mode can move
 * the NVIDIA pin -> PCM map). */
#define DRM_OUTPUT_AUDIO_CHECK_NS 5000000000ULL

struct drm_output_audio {
	char connector[64];
	struct drm_output_monitor_id want;
	struct drm_output_present_clock *clock;
	const volatile long *view; /* the output's own view (OBS_DRM_OUTPUT_VIEW_*), read atomically */
	pthread_t thread;
	bool thread_started;
	volatile bool stop;
	volatile bool connected;

	/* resolved (written by the resolver before `connected`, read-only afterwards) */
	struct drm_output_alsa alsa;
	struct drm_output_snd_pcm *pcm;
	int card;
	int device;
	unsigned int rate;
	size_t frame_bytes;
	char iec958[96];

	/* audio thread only */
	bool open_logged;
	bool view_program;
	uint64_t blocks;
	uint64_t holds;
	uint64_t trims;
	uint64_t xruns;
	uint64_t nudges;
	uint64_t dropped_frames; /* frames the full ring refused (-EAGAIN / a short write) */
	bool write_err_logged;
	uint64_t last_report_ns;
	volatile bool pcm_lost; /* the watcher's verdict (os_atomic): the PCM is no longer the connector's */
	uint8_t *silence; /* preallocated at open: the audio thread never allocates */
	size_t silence_frames;
};

static void drm_output_audio_no_audio(const struct drm_output_audio *a, const char *why, int card, int device)
{
	if (card >= 0)
		blog(LOG_WARNING, "drm-output audio: NO AUDIO on '%s' -- %s (hw:%d,%d); never another device",
		     a->connector, why, card, device);
	else
		blog(LOG_WARNING, "drm-output audio: NO AUDIO on '%s' -- %s; never another device", a->connector, why);
}

/* Write `frames` frames, recovering one xrun (the ring drained: the next block re-places itself). A full
 * ring (-EAGAIN, a short write) drops what did not fit -- counted, the next placement trims back to the
 * floor; any other error is named once. */
static void drm_output_audio_write(struct drm_output_audio *a, const void *buf, size_t frames)
{
	long w = a->alsa.writei(a->pcm, buf, (unsigned long)frames);
	if (w == -EPIPE || w == -ESTRPIPE) {
		a->xruns++;
		if (a->alsa.recover(a->pcm, (int)w, 1) == 0)
			w = a->alsa.writei(a->pcm, buf, (unsigned long)frames);
	}
	if (w >= 0) {
		if ((size_t)w < frames)
			a->dropped_frames += frames - (size_t)w;
	} else if (w == -EAGAIN) {
		a->dropped_frames += frames;
	} else if (!a->write_err_logged) {
		a->write_err_logged = true;
		blog(LOG_WARNING, "drm-output audio: '%s' write to hw:%d,%d failed (%s)", a->connector, a->card,
		     a->device, a->alsa.strerror_fn((int)w));
	}
}

/* The resolver thread (the leg's watcher), every DRM_OUTPUT_AUDIO_CHECK_NS: the open PCM still carries
 * the connector's monitor (its ELD still picks the same card + device). Never on libobs's audio thread:
 * it opens the control devices and allocates. */
static bool drm_output_audio_pcm_still_matches(const struct drm_output_audio *a)
{
	struct drm_output_audio_pcm pcms[DRM_OUTPUT_AUDIO_MAX_PCMS];
	memset(pcms, 0, sizeof(pcms));
	size_t idx = 0;
	const size_t n = drm_output_audio_read_elds(pcms, DRM_OUTPUT_AUDIO_MAX_PCMS);
	return drm_output_audio_pick_pcm(&a->want, pcms, n, &idx) == DRM_OUTPUT_AUDIO_MATCH &&
	       pcms[idx].card == a->card && pcms[idx].device == a->device;
}

/* One mix-0 block on libobs's audio thread: place it against the picture and write it. */
static void drm_output_audio_on_mix(void *param, size_t mix_idx, struct audio_data *data)
{
	UNUSED_PARAMETER(mix_idx);
	struct drm_output_audio *a = param;
	if (!data || data->frames == 0 || !data->data[0])
		return;

	/* the program audio belongs on the screen that shows the Program, never on the Multiview monitor
	 * (issue 1424 part 2: THIS output's view -- the other output may show the Multiview) */
	const bool program = os_atomic_load_long(a->view) == OBS_DRM_OUTPUT_VIEW_PROGRAM;
	if (program != a->view_program) {
		a->view_program = program;
		blog(LOG_INFO, "drm-output audio: '%s' %s", a->connector,
		     program ? "plays the program mix (view program)"
			     : "muted (view multiview: the program audio stays off this monitor)");
	}
	if (!program)
		return;

	if (os_atomic_load_bool(&a->pcm_lost)) /* the watcher found the PCM no longer the connector's */
		return;

	const uint64_t rate = a->rate;
	const uint64_t block_ns = (uint64_t)data->frames * 1000000000ULL / rate;
	const uint64_t floor_ns = DRM_OUTPUT_AUDIO_FLOOR_BLOCKS * block_ns;
	const uint64_t threshold_ns = 2 * block_ns;
	const uint64_t present_ns = (uint64_t)os_atomic_load_long(&a->clock->delay_ns);

	long queued = 0;
	if (a->alsa.delay(a->pcm, &queued) < 0 || queued < 0)
		queued = 0; /* a fresh or drained PCM: nothing queued */
	const uint64_t pcm_ns = (uint64_t)queued * 1000000000ULL / rate;
	const uint64_t now = os_gettime_ns();
	const struct drm_output_audio_plan plan = drm_output_audio_plan_block(
		data->timestamp, present_ns, now, pcm_ns, floor_ns, threshold_ns, block_ns, DRM_OUTPUT_AUDIO_BAND_NS);

	if (plan.silence_ns > 0) {
		size_t frames = (size_t)(plan.silence_ns * rate / 1000000000ULL);
		if (frames > a->silence_frames)
			frames = a->silence_frames; /* never more than the ring holds */
		drm_output_audio_write(a, a->silence, frames);
		a->holds++;
	}
	size_t skip = (size_t)(plan.drop_ns * rate / 1000000000ULL);
	if (plan.nudge_frames < 0)
		skip++; /* the drift walk: one frame dropped */
	if (skip > data->frames)
		skip = data->frames;
	if (plan.drop_ns > 0)
		a->trims++;
	if (plan.nudge_frames != 0)
		a->nudges++;
	if (plan.nudge_frames > 0)
		drm_output_audio_write(a, data->data[0], 1); /* the drift walk: the first frame repeated */
	if (skip < data->frames)
		drm_output_audio_write(a, data->data[0] + skip * a->frame_bytes, data->frames - skip);

	a->blocks++;
	if (!a->open_logged) {
		a->open_logged = true;
		a->last_report_ns = now;
		blog(LOG_INFO,
		     "drm-output audio: pcm hw:%d,%d card %d device %d delay %.1f ms (on '%s'; audio %.1f ms behind the "
		     "picture, present %.1f ms, pcm delay %.1f ms, %u Hz, %s)",
		     a->card, a->device, a->card, a->device, (double)plan.silence_ns / 1e6, a->connector,
		     (double)plan.offset_ns / 1e6, (double)present_ns / 1e6, (double)pcm_ns / 1e6, a->rate, a->iec958);
	} else if (now - a->last_report_ns >= DRM_OUTPUT_AUDIO_REPORT_NS) {
		a->last_report_ns = now;
		blog(LOG_INFO,
		     "drm-output audio: '%s' offset %.1f ms (audio behind the picture), present %.1f ms, pcm %.1f ms, "
		     "blocks %llu holds %llu trims %llu nudges %llu xruns %llu dropped_frames %llu",
		     a->connector, (double)plan.offset_ns / 1e6, (double)present_ns / 1e6, (double)pcm_ns / 1e6,
		     (unsigned long long)a->blocks, (unsigned long long)a->holds, (unsigned long long)a->trims,
		     (unsigned long long)a->nudges, (unsigned long long)a->xruns,
		     (unsigned long long)a->dropped_frames);
	}
}

/* Open + configure hw:<card>,<device>: interleaved stereo, S32_LE (else S16_LE), 48 kHz near.
 * Returns NULL on success, else the named reason. Sets the OBS conversion format. */
static const char *drm_output_audio_open_pcm(struct drm_output_audio *a, enum audio_format *fmt)
{
	char name[32];
	snprintf(name, sizeof(name), "hw:%d,%d", a->card, a->device);
	int err = a->alsa.pcm_open(&a->pcm, name, DRM_OUTPUT_SND_PCM_STREAM_PLAYBACK, DRM_OUTPUT_SND_PCM_NONBLOCK);
	if (err < 0) {
		a->pcm = NULL;
		return err == -EBUSY ? "the PCM is busy (another program holds it)" : a->alsa.strerror_fn(err);
	}
	struct drm_output_snd_hw *hw = bzalloc(a->alsa.hw_sizeof());
	const char *why = NULL;
	int sf = DRM_OUTPUT_SND_PCM_FORMAT_S32_LE;
	unsigned int rate = 48000;
	unsigned long period = DRM_OUTPUT_AUDIO_PERIOD_FRAMES;
	unsigned long buffer = DRM_OUTPUT_AUDIO_BUFFER_FRAMES;
	if (a->alsa.hw_any(a->pcm, hw) < 0 || a->alsa.hw_access(a->pcm, hw, DRM_OUTPUT_SND_PCM_ACCESS_RW_INTERLEAVED) < 0)
		why = "the PCM refused interleaved access";
	if (!why && a->alsa.hw_test_format(a->pcm, hw, sf) < 0)
		sf = DRM_OUTPUT_SND_PCM_FORMAT_S16_LE;
	if (!why && a->alsa.hw_format(a->pcm, hw, sf) < 0)
		why = "the PCM takes neither S32_LE nor S16_LE";
	if (!why && a->alsa.hw_channels(a->pcm, hw, 2) < 0)
		why = "the PCM refused 2 channels";
	if (!why && a->alsa.hw_rate(a->pcm, hw, &rate, NULL) < 0)
		why = "the PCM refused a rate";
	if (!why && (a->alsa.hw_period(a->pcm, hw, &period, NULL) < 0 || a->alsa.hw_buffer(a->pcm, hw, &buffer) < 0))
		why = "the PCM refused the period / buffer size";
	if (!why && a->alsa.hw_apply(a->pcm, hw) < 0)
		why = "the PCM refused the hardware parameters";
	bfree(hw);
	if (why) {
		a->alsa.pcm_close(a->pcm);
		a->pcm = NULL;
		return why;
	}
	a->rate = rate;
	a->frame_bytes = sf == DRM_OUTPUT_SND_PCM_FORMAT_S32_LE ? 8 : 4;
	/* the hold's silence, sized to the ring once here: the audio thread never allocates */
	a->silence_frames = (size_t)buffer;
	a->silence = bzalloc(a->silence_frames * a->frame_bytes);
	*fmt = sf == DRM_OUTPUT_SND_PCM_FORMAT_S32_LE ? AUDIO_FORMAT_32BIT : AUDIO_FORMAT_16BIT;
	return NULL;
}

/* The resolver: the PCM inside the bounded window, its IEC958 switch, the PCM, the mix-0 callback. */
static void *drm_output_audio_thread(void *param)
{
	struct drm_output_audio *a = param;
	os_set_thread_name("drm-output-audio");

	if (!a->want.valid) {
		drm_output_audio_no_audio(a, drm_output_audio_reason(DRM_OUTPUT_AUDIO_NO_EDID), -1, -1);
		return NULL;
	}
	if (!drm_output_alsa_load(&a->alsa)) {
		drm_output_audio_no_audio(a, "libasound.so.2 could not be loaded", -1, -1);
		return NULL;
	}

	struct drm_output_audio_pcm *pcms = bzalloc(sizeof(*pcms) * DRM_OUTPUT_AUDIO_MAX_PCMS);
	const uint64_t deadline = os_gettime_ns() + (uint64_t)DRM_OUTPUT_AUDIO_RESOLVE_WINDOW_MS * 1000000ULL;
	int result = DRM_OUTPUT_AUDIO_NO_MONITOR;
	size_t n = 0, idx = 0;
	for (;;) {
		n = drm_output_audio_read_elds(pcms, DRM_OUTPUT_AUDIO_MAX_PCMS);
		result = drm_output_audio_pick_pcm(&a->want, pcms, n, &idx);
		if (result == DRM_OUTPUT_AUDIO_MATCH || !drm_output_audio_retryable(result) ||
		    os_gettime_ns() >= deadline || os_atomic_load_bool(&a->stop))
			break;
		for (int t = 0; t < DRM_OUTPUT_AUDIO_RESOLVE_STEP_MS / 50 && !os_atomic_load_bool(&a->stop); t++)
			os_sleep_ms(50);
	}
	if (result != DRM_OUTPUT_AUDIO_MATCH) {
		if (!os_atomic_load_bool(&a->stop))
			drm_output_audio_no_audio(a, drm_output_audio_reason(result), -1, -1);
		goto done;
	}
	a->card = pcms[idx].card;
	a->device = pcms[idx].device;
	drm_output_audio_iec958_on(a->card, pcms, n, a->device, a->iec958, sizeof(a->iec958));

	enum audio_format fmt = AUDIO_FORMAT_32BIT;
	const char *why = drm_output_audio_open_pcm(a, &fmt);
	if (why) {
		drm_output_audio_no_audio(a, why, a->card, a->device);
		goto done;
	}

	/* libobs's audio output exists only after the frontend's audio reset, after obs_startup */
	while (!obs_get_audio() && !os_atomic_load_bool(&a->stop))
		os_sleep_ms(100);
	if (os_atomic_load_bool(&a->stop))
		goto done;
	struct audio_convert_info conv = {.samples_per_sec = a->rate, .format = fmt, .speakers = SPEAKERS_STEREO};
	a->view_program = os_atomic_load_long(a->view) == OBS_DRM_OUTPUT_VIEW_PROGRAM;
	obs_add_raw_audio_callback(0, &conv, drm_output_audio_on_mix, a);
	os_atomic_set_bool(&a->connected, true);
	blog(LOG_INFO, "drm-output audio: '%s' matched hw:%d,%d (%u Hz, %s, %s) -- mix 0 connected%s", a->connector,
	     a->card, a->device, a->rate, fmt == AUDIO_FORMAT_32BIT ? "S32_LE" : "S16_LE", a->iec958,
	     a->view_program ? "" : " (muted while the view is multiview)");

	/* the watcher: an HDMI replug or a re-mode can move the NVIDIA pin -> PCM map, and then hw:C,D is
	 * another connector's -- muted (never written) and named until it is the connector's again */
	while (!os_atomic_load_bool(&a->stop)) {
		for (uint64_t t = 0; t < DRM_OUTPUT_AUDIO_CHECK_NS / 50000000ULL && !os_atomic_load_bool(&a->stop); t++)
			os_sleep_ms(50);
		if (os_atomic_load_bool(&a->stop))
			break;
		const bool lost = !drm_output_audio_pcm_still_matches(a);
		if (lost == os_atomic_load_bool(&a->pcm_lost))
			continue;
		os_atomic_set_bool(&a->pcm_lost, lost);
		if (lost)
			blog(LOG_WARNING,
			     "drm-output audio: '%s' hw:%d,%d no longer carries the connector's monitor (an HDMI replug or a "
			     "re-mode moved the PCM map) -- muted",
			     a->connector, a->card, a->device);
		else
			blog(LOG_INFO, "drm-output audio: '%s' hw:%d,%d carries the connector's monitor again", a->connector,
			     a->card, a->device);
	}
done:
	bfree(pcms);
	return NULL;
}

struct drm_output_audio *drm_output_audio_open(const char *connector, const uint8_t *edid, size_t edid_len,
					       struct drm_output_present_clock *clock, const volatile long *view)
{
	if (!connector || !clock || !view)
		return NULL;
	struct drm_output_audio *a = bzalloc(sizeof(*a));
	snprintf(a->connector, sizeof(a->connector), "%s", connector);
	a->clock = clock;
	a->view = view;
	a->card = -1;
	a->device = -1;
	snprintf(a->iec958, sizeof(a->iec958), "IEC958 switch untouched");
	(void)drm_output_edid_monitor_id(edid, edid_len, &a->want);
	if (a->want.valid)
		blog(LOG_INFO, "drm-output audio: '%s' EDID %02x%02x %02x%02x '%s' -- resolving its HDMI PCM", a->connector,
		     a->want.vendor[0], a->want.vendor[1], a->want.vendor[3], a->want.vendor[2], a->want.name);
	if (pthread_create(&a->thread, NULL, drm_output_audio_thread, a) == 0)
		a->thread_started = true;
	else
		drm_output_audio_no_audio(a, "the resolver thread could not start", -1, -1);
	return a;
}

void drm_output_audio_close(struct drm_output_audio *a)
{
	if (!a)
		return;
	os_atomic_set_bool(&a->stop, true);
	if (a->thread_started)
		pthread_join(a->thread, NULL);
	const bool connected = os_atomic_load_bool(&a->connected);
	if (connected)
		obs_remove_raw_audio_callback(0, drm_output_audio_on_mix, a);
	if (a->pcm) {
		a->alsa.pcm_close(a->pcm);
		a->pcm = NULL;
	}
	if (a->alsa.lib)
		dlclose(a->alsa.lib);
	if (connected)
		blog(LOG_INFO,
		     "drm-output audio: '%s' closed (blocks %llu holds %llu trims %llu nudges %llu xruns %llu dropped_frames "
		     "%llu)",
		     a->connector, (unsigned long long)a->blocks, (unsigned long long)a->holds,
		     (unsigned long long)a->trims, (unsigned long long)a->nudges, (unsigned long long)a->xruns,
		     (unsigned long long)a->dropped_frames);
	bfree(a->silence);
	bfree(a);
}

#endif /* defined(__linux__) */
