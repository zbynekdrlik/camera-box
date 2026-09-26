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
 * The vk-direct backend (OBS side).
 * ------------------------------------------------------------------------------------------------- */
static struct {
	pthread_mutex_t lock; /* active/stopping transitions */
	bool active;
	bool stopping;
	volatile bool owns;        /* the output is ours: the entry points + seam route here (os_atomic) */
	volatile bool program;     /* bind + publish frames (false = the "program": false solid diagnostic) */
	gs_texture_t *mid;         /* the intermediate render target (graphics context) */
	bool gl_ready;             /* mid + the GL import exist (graphics context) */
	char connector[64];
} g_vkd = {
	.lock = PTHREAD_MUTEX_INITIALIZER,
};

static bool vkd_start(const struct obs_drm_output_config *cfg)
{
	pthread_mutex_lock(&g_vkd.lock);
	if (g_vkd.stopping) {
		pthread_mutex_unlock(&g_vkd.lock);
		blog(LOG_WARNING, "drm-output: start rejected — a stop is in progress (vk-direct)");
		return false;
	}
	if (g_vkd.active) {
		pthread_mutex_unlock(&g_vkd.lock);
		blog(LOG_INFO, "drm-output: already active — start ignored (vk-direct)");
		return true;
	}
	if (!drm_output_vk_open(cfg->connector_name, cfg->solid_argb)) {
		pthread_mutex_unlock(&g_vkd.lock);
		blog(LOG_WARNING, "drm-output: start FAILED for '%s' (vk-direct)", cfg->connector_name);
		return false;
	}
	snprintf(g_vkd.connector, sizeof(g_vkd.connector), "%s", cfg->connector_name);
	g_vkd.gl_ready = false;
	g_vkd.active = true;
	os_atomic_set_bool(&g_vkd.program, cfg->program);
	os_atomic_set_bool(&g_vkd.owns, true);
	pthread_mutex_unlock(&g_vkd.lock);
	blog(LOG_INFO, "drm-output: ACTIVE (vk-direct) — presenting solid 0x%06x on '%s' until the first %s",
	     cfg->solid_argb & 0xFFFFFFu, cfg->connector_name,
	     cfg->program ? "rendered frame" : "stop (\"program\": false diagnostic)");
	return true;
}

static void vkd_stop(void)
{
	pthread_mutex_lock(&g_vkd.lock);
	if (!g_vkd.active || g_vkd.stopping) {
		pthread_mutex_unlock(&g_vkd.lock);
		return;
	}
	g_vkd.stopping = true;
	os_atomic_set_bool(&g_vkd.program, false); /* the frame hook stops producing */
	pthread_mutex_unlock(&g_vkd.lock);

	drm_output_vk_halt(); /* no present after this; the display stays acquired */

	/* GL side under the graphics context — a frame hook mid-render finishes first. */
	obs_enter_graphics();
	if (g_vkd.gl_ready)
		drm_output_vk_gl_unbind();
	if (g_vkd.mid) {
		gs_texture_destroy(g_vkd.mid);
		g_vkd.mid = NULL;
	}
	g_vkd.gl_ready = false;
	obs_leave_graphics();
	drm_output_view_gl_teardown();

	drm_output_vk_close(); /* release the display: it returns to X disabled, never onto the desktop */

	pthread_mutex_lock(&g_vkd.lock);
	os_atomic_set_bool(&g_vkd.owns, false);
	g_vkd.active = false;
	g_vkd.stopping = false;
	pthread_mutex_unlock(&g_vkd.lock);
	blog(LOG_INFO, "drm-output: stopped (vk-direct, '%s')", g_vkd.connector);
}

static bool vkd_active(void)
{
	pthread_mutex_lock(&g_vkd.lock);
	bool a = g_vkd.active;
	pthread_mutex_unlock(&g_vkd.lock);
	return a;
}

static bool vkd_owns(void)
{
	return os_atomic_load_bool(&g_vkd.owns);
}

/* The lazy GL bind: the intermediate at the display mode size + the shared-image import. Graphics
 * thread, context held. false = disarm (the display keeps the solid pattern — fail open). */
static bool vkd_gl_bind(void)
{
	uint32_t w = 0, h = 0;
	drm_output_vk_mode_size(&w, &h);
	g_vkd.mid = gs_texture_create(w, h, GS_BGRA, 1, NULL, GS_RENDER_TARGET);
	if (!g_vkd.mid) {
		blog(LOG_WARNING, "drm-output: program bind FAILED (vk-direct: intermediate %ux%u render target) -- "
				  "staying on the solid pattern",
		     w, h);
		return false;
	}
	if (!drm_output_vk_gl_bind()) {
		gs_texture_destroy(g_vkd.mid);
		g_vkd.mid = NULL;
		return false;
	}
	return true;
}

/* The frame hook. true = the vk-direct backend owns the output (the lease hook must not run). */
static bool vkd_on_frame(void)
{
	if (!os_atomic_load_bool(&g_vkd.owns))
		return false;
	if (!os_atomic_load_bool(&g_vkd.program) || !drm_output_vk_wants_frames())
		return true;

	obs_enter_graphics();
	if (!os_atomic_load_bool(&g_vkd.program)) { /* re-check: a stop may have disarmed */
		obs_leave_graphics();
		return true;
	}
	if (!g_vkd.gl_ready) {
		if (!vkd_gl_bind()) {
			os_atomic_set_bool(&g_vkd.program, false);
			obs_leave_graphics();
			return true;
		}
		g_vkd.gl_ready = true;
	}

	/* The selectable view (issue 1346): the Multiview is rendered + published by the view TU through
	 * the seam below; only a PROGRAM tick falls through to the Program copy. */
	if (drm_output_view_frame() != DRM_OUTPUT_TICK_PROGRAM) {
		obs_leave_graphics();
		return true;
	}
	gs_texture_t *program = obs_get_main_texture();
	if (program) {
		int idx = drm_output_claim_render_buf();
		if (idx >= 0 && drm_output_blit_raw(program, idx))
			drm_output_publish_render_buf(idx);
	}
	obs_leave_graphics();
	return true;
}

/* ---- the mailbox seam (graphics thread, context held) ---- */

static int vkd_claim(void)
{
	return drm_output_vk_claim();
}

static void vkd_publish(int idx)
{
	if (!g_vkd.mid)
		return;
	/* libobs-opengl's gs_texture_get_obj returns a pointer to the texture's GLuint name. */
	const unsigned int *name = gs_texture_get_obj(g_vkd.mid);
	if (!name)
		return;
	uint32_t w = 0, h = 0;
	drm_output_vk_mode_size(&w, &h);
	(void)drm_output_vk_publish_gl(idx, *name, w, h);
}

static gs_texture_t *vkd_texture(int idx)
{
	(void)idx; /* one intermediate: claim -> blit -> publish is sequential on the graphics thread */
	return g_vkd.gl_ready ? g_vkd.mid : NULL;
}

static void vkd_mode_size(uint32_t *w, uint32_t *h)
{
	drm_output_vk_mode_size(w, h);
}

const struct drm_output_backend_ops drm_output_vk_direct_backend = {
	.name = "vk-direct",
	.start = vkd_start,
	.stop = vkd_stop,
	.active = vkd_active,
	.on_frame = vkd_on_frame,
	.owns = vkd_owns,
	.claim = vkd_claim,
	.publish = vkd_publish,
	.texture = vkd_texture,
	.mode_size = vkd_mode_size,
};

#endif /* defined(__linux__) */
