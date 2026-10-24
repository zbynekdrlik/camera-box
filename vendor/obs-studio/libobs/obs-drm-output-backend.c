/*
 * camera-box issue 1346 — backend SELECTION + the vk-direct backend's OBS side.
 *
 * The DRM output module (obs-drm-output.c) was born with ONE way to take an HDMI connector off the X
 * desktop: the issue-1152 X RandR LEASE (imag, Intel). strih-lx's built-in HDMI is driven by the NVIDIA
 * GPU, whose X driver refuses that lease; the NVIDIA way is Vulkan direct display (obs-drm-output-vk.c).
 * The config picks the backend — "backend": "lease" (the default, what an absent key means, so imag is
 * unchanged) or "vk-direct" — and obs-drm-output.c routes its public entry points and its mailbox seam
 * to drm_output_vk_direct_backend while that backend owns the output. The lease code itself is
 * untouched; the view renderer (obs-drm-output-view.c), its budget gate, the Tools-menu switch, the
 * config file and the `drm-output:` log family are shared by both.
 *
 * What this TU adds on top of the vk core: the gs intermediate render target the ONE raw blit
 * (drm_output_blit_raw) fills at the display's mode size, the lazy GL bind on the first graphics tick,
 * and the frame hook — the same CLAIM / RENDER / PUBLISH shape as the lease hook, where PUBLISH hands the
 * intermediate's GL texture to drm_output_vk_publish_gl (a GPU copy into the shared Vulkan image).
 *
 * issue 1424 part 2: the state is PER OUTPUT INSTANCE (g_vkd[inst->index]); each instance opens its own
 * display in the vk core, which keeps ONE VkDevice for both (obs-drm-output-vk-internal.h).
 *
 * Lock order (the module's rule): graphics context first; the vk core's own mailbox lock is taken inside
 * claim/publish only. stop(): halt the present thread -> GL teardown under the graphics context ->
 * the view's texrender -> close (release the display). obs_shutdown() stops before stop_video(), so the
 * graphics subsystem is still alive for the teardown.
 *
 * Linux-only, built only via libobs/cmake/os-linux.cmake. The pure grammar drm_output_parse_backend is
 * lift-compiled + truth-tabled by tests/drm_output_vk_direct_1346.rs (the table is shared with the
 * Python mirror, tests/fixtures/drm_output_backend_parity.tsv).
 */


#if defined(__linux__)

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "obs.h"
#include "obs-drm-output.h"
#include "obs-drm-output-internal.h"
#include "obs-drm-output-vk.h"
#include "util/threading.h"

/* -------------------------------------------------------------------------------------------------
 * Pure decision helper (Tier-0, lift-compiled + truth-tabled by tests/drm_output_vk_direct_1346.rs).
 *
 * The "backend" config grammar: absent/empty -> 0 (the issue-1152 lease, imag unchanged), "lease" -> 0,
 * "vk-direct" -> 1, anything else -> -1 (unknown: the caller stays DORMANT with a WARN — a guessed
 * backend fails on the wrong GPU anyway, and a loud dormant output is the honest state). Exact lowercase
 * match only — the same table the Python mirror (strih_scenes.drm_output_backend_of) reads.
 * ------------------------------------------------------------------------------------------------- */
static int drm_output_parse_backend(const char *s)
{
	if (!s || s[0] == '\0')
		return 0;
	if (strcmp(s, "lease") == 0)
		return 0;
	if (strcmp(s, "vk-direct") == 0)
		return 1;
	return -1;
}

_Static_assert(OBS_DRM_OUTPUT_BACKEND_LEASE == 0 && OBS_DRM_OUTPUT_BACKEND_VK_DIRECT == 1,
	       "drm_output_parse_backend returns the enum values as literals");

int drm_output_backend_from_config(const char *value)
{
	return drm_output_parse_backend(value);
}

/* -------------------------------------------------------------------------------------------------
 * The vk-direct backend (OBS side) of ONE output instance (issue 1424 part 2: g_vkd[inst->index]; the
 * displays share one VkDevice inside the vk core).
 * ------------------------------------------------------------------------------------------------- */
struct drm_output_vkd {
	pthread_mutex_t lock; /* active/stopping transitions */
	bool active;
	bool stopping;
	/* issue 1424: `active` mirrored for lock-free readers -- the frontend polls it on the UI thread and
	 * a retried start holds `lock` across a whole open (seconds), so a read must never wait for it */
	volatile bool active_pub;
	volatile bool program;            /* bind + publish frames (false = the "program": false solid diagnostic) */
	struct drm_output_vk_disp *disp;  /* the instance's display in the vk core (NULL before the first open) */
	gs_texture_t *mid;                /* the intermediate render target (graphics context) */
	bool gl_ready;                    /* mid + the GL import exist (graphics context) */
	uint32_t want_w, want_h, want_mhz; /* issue 1424: the last wanted mode handed over (graphics thread) */
};

static struct drm_output_vkd g_vkd[DRM_OUTPUT_MAX_OUTPUTS] = {
	{.lock = PTHREAD_MUTEX_INITIALIZER},
	{.lock = PTHREAD_MUTEX_INITIALIZER},
};
_Static_assert(DRM_OUTPUT_MAX_OUTPUTS == 2, "one g_vkd initializer per output slot");

static struct drm_output_vkd *vkd_of(struct drm_output_inst *inst)
{
	return &g_vkd[inst->index];
}

/* issue 1424: the vk core's present thread reports each on-screen buffer to the instance's clock. */
static void vkd_note_shown(void *param, int idx, uint64_t shown_ns)
{
	struct drm_output_inst *inst = param;
	drm_output_present_note_shown(&inst->clock, idx, shown_ns);
}

static bool vkd_start(struct drm_output_inst *inst)
{
	struct drm_output_vkd *k = vkd_of(inst);
	pthread_mutex_lock(&k->lock);
	if (k->stopping) {
		pthread_mutex_unlock(&k->lock);
		blog(LOG_WARNING, "drm-output: start rejected — a stop is in progress (vk-direct, '%s')", inst->connector);
		return false;
	}
	if (k->active) {
		pthread_mutex_unlock(&k->lock);
		blog(LOG_INFO, "drm-output: already active — start ignored (vk-direct, '%s')", inst->connector);
		return true;
	}
	k->disp = drm_output_vk_open(inst->index, inst->connector, inst->solid_argb, vkd_note_shown, inst);
	if (!k->disp) {
		pthread_mutex_unlock(&k->lock);
		blog(LOG_WARNING, "drm-output: start FAILED for '%s' (vk-direct)", inst->connector);
		return false;
	}
	k->gl_ready = false;
	k->want_w = k->want_h = k->want_mhz = 0; /* issue 1424: the output opened with today's rule */
	k->active = true;
	os_atomic_set_bool(&k->active_pub, true);
	os_atomic_set_bool(&k->program, inst->program);
	pthread_mutex_unlock(&k->lock);
	blog(LOG_INFO, "drm-output: ACTIVE (vk-direct) — presenting solid 0x%06x on '%s' until the first %s",
	     inst->solid_argb & 0xFFFFFFu, inst->connector,
	     inst->program ? "rendered frame" : "stop (\"program\": false diagnostic)");
	return true;
}

static void vkd_stop(struct drm_output_inst *inst)
{
	struct drm_output_vkd *k = vkd_of(inst);
	pthread_mutex_lock(&k->lock);
	if (!k->active || k->stopping) {
		pthread_mutex_unlock(&k->lock);
		return;
	}
	k->stopping = true;
	os_atomic_set_bool(&k->program, false); /* the frame hook stops producing */
	pthread_mutex_unlock(&k->lock);

	drm_output_vk_halt(k->disp); /* no present after this; the display stays acquired */

	/* GL side under the graphics context — a frame hook mid-render finishes first. */
	obs_enter_graphics();
	if (k->gl_ready)
		drm_output_vk_gl_unbind(k->disp);
	if (k->mid) {
		gs_texture_destroy(k->mid);
		k->mid = NULL;
	}
	k->gl_ready = false;
	obs_leave_graphics();
	drm_output_view_gl_teardown(inst);

	drm_output_vk_close(k->disp); /* release the display: it returns to X disabled, never onto the desktop */

	/* k->disp keeps pointing at the (static) display slot: a frame hook that raced the stop reads a
	 * closed display (wants_frames false), never a NULL. */
	pthread_mutex_lock(&k->lock);
	k->active = false;
	os_atomic_set_bool(&k->active_pub, false);
	k->stopping = false;
	pthread_mutex_unlock(&k->lock);
	blog(LOG_INFO, "drm-output: stopped (vk-direct, '%s')", inst->connector);
}

static bool vkd_active(struct drm_output_inst *inst)
{
	return os_atomic_load_bool(&vkd_of(inst)->active_pub);
}

/* issue 1424: the output's scanout really runs -- open, not stopping, and its present loop still wants
 * frames (cleared when the loop died: a lost device, a display that now offers another mode size, a
 * lone display whose rebuild gave up). The retry supervisor releases an output that is active but not
 * alive and retries it. */
static bool vkd_alive(struct drm_output_inst *inst)
{
	struct drm_output_vkd *k = vkd_of(inst);
	return os_atomic_load_bool(&k->active_pub) && k->disp && drm_output_vk_wants_frames(k->disp);
}

/* The lazy GL bind: the intermediate at the display mode size + the shared-image import. Graphics
 * thread, context held. false = disarm (the display keeps the solid pattern — fail open). */
static bool vkd_gl_bind(struct drm_output_inst *inst)
{
	struct drm_output_vkd *k = vkd_of(inst);
	uint32_t w = 0, h = 0;
	drm_output_vk_mode_size(k->disp, &w, &h);
	k->mid = gs_texture_create(w, h, GS_BGRA, 1, NULL, GS_RENDER_TARGET);
	if (!k->mid) {
		blog(LOG_WARNING,
		     "drm-output: program bind FAILED (vk-direct: intermediate %ux%u render target on '%s') -- "
		     "staying on the solid pattern",
		     w, h, inst->connector);
		return false;
	}
	if (!drm_output_vk_gl_bind(k->disp)) {
		gs_texture_destroy(k->mid);
		k->mid = NULL;
		return false;
	}
	return true;
}

/* camera-box issue 1424, graphics thread: the Program view wants the CANVAS size at the CANVAS frame
 * rate (one render tick = one scanout; 1080p30 is what the vestibule's SDI chain carries); the Multiview
 * view wants nothing (today's native ~60 Hz rule, the operator's monitor unchanged). Each output reads
 * ITS OWN view (issue 1424 part 2). Handed over only on a change, so a steady output re-modes never. */
static void vkd_want_program_mode(struct drm_output_inst *inst)
{
	struct drm_output_vkd *k = vkd_of(inst);
	uint32_t w = 0, h = 0, mhz = 0;
	struct obs_video_info ovi;
	if (os_atomic_load_long(&inst->view.view) == OBS_DRM_OUTPUT_VIEW_PROGRAM && obs_get_video_info(&ovi) &&
	    ovi.fps_den != 0) {
		w = ovi.base_width;
		h = ovi.base_height;
		mhz = (uint32_t)((uint64_t)ovi.fps_num * 1000u / ovi.fps_den);
	}
	if (w == k->want_w && h == k->want_h && mhz == k->want_mhz)
		return;
	k->want_w = w;
	k->want_h = h;
	k->want_mhz = mhz;
	drm_output_vk_want_mode(k->disp, w, h, mhz);
}

/* The frame hook of one vk-direct instance. */
static void vkd_on_frame(struct drm_output_inst *inst)
{
	struct drm_output_vkd *k = vkd_of(inst);
	if (!os_atomic_load_bool(&k->program) || !k->disp || !drm_output_vk_wants_frames(k->disp))
		return;
	vkd_want_program_mode(inst); /* issue 1424 */

	obs_enter_graphics();
	if (!os_atomic_load_bool(&k->program)) { /* re-check: a stop may have disarmed */
		obs_leave_graphics();
		return;
	}
	if (!k->gl_ready) {
		if (!vkd_gl_bind(inst)) {
			os_atomic_set_bool(&k->program, false);
			obs_leave_graphics();
			return;
		}
		k->gl_ready = true;
	}

	/* The selectable view (issue 1346): the Multiview is rendered + published by the view TU through
	 * the seam below; only a PROGRAM tick falls through to the Program copy. */
	if (drm_output_view_frame(inst) != DRM_OUTPUT_TICK_PROGRAM) {
		obs_leave_graphics();
		return;
	}
	gs_texture_t *program = obs_get_main_texture();
	if (program) {
		int idx = drm_output_claim_render_buf(inst);
		if (idx >= 0 && drm_output_blit_raw(inst, program, idx))
			drm_output_publish_render_buf(inst, idx);
	}
	obs_leave_graphics();
}

/* ---- the mailbox seam (graphics thread, context held) ---- */

static int vkd_claim(struct drm_output_inst *inst)
{
	return drm_output_vk_claim(vkd_of(inst)->disp);
}

static void vkd_publish(struct drm_output_inst *inst, int idx)
{
	struct drm_output_vkd *k = vkd_of(inst);
	if (!k->mid)
		return;
	/* libobs-opengl's gs_texture_get_obj returns a pointer to the texture's GLuint name. */
	const unsigned int *name = gs_texture_get_obj(k->mid);
	if (!name)
		return;
	uint32_t w = 0, h = 0;
	drm_output_vk_mode_size(k->disp, &w, &h);
	(void)drm_output_vk_publish_gl(k->disp, idx, *name, w, h);
}

static gs_texture_t *vkd_texture(struct drm_output_inst *inst, int idx)
{
	(void)idx; /* one intermediate: claim -> blit -> publish is sequential on the graphics thread */
	struct drm_output_vkd *k = vkd_of(inst);
	return k->gl_ready ? k->mid : NULL;
}

static void vkd_mode_size(struct drm_output_inst *inst, uint32_t *w, uint32_t *h)
{
	struct drm_output_vkd *k = vkd_of(inst);
	if (!k->disp) {
		*w = *h = 0;
		return;
	}
	drm_output_vk_mode_size(k->disp, w, h);
}

const struct drm_output_backend_ops drm_output_vk_direct_backend = {
	.name = "vk-direct",
	.start = vkd_start,
	.stop = vkd_stop,
	.active = vkd_active,
	.on_frame = vkd_on_frame,
	.claim = vkd_claim,
	.publish = vkd_publish,
	.texture = vkd_texture,
	.mode_size = vkd_mode_size,
	.alive = vkd_alive,
};

#endif /* defined(__linux__) */
