#pragma once

/*
 * camera-box issue 1346 — internal (non-exported) seam between the two DRM-lease output TUs:
 *   obs-drm-output.c       lease, flip thread, GBM scanout buffers, the Program copy (issue 1152)
 *   obs-drm-output-view.c  the selectable view: the frontend Multiview render + its budget gate,
 *                          the render-cost audit and the persisted "view" key
 *
 * Linux-only, like both TUs. Every function here runs on the graphics thread with the graphics
 * context held (the frame hook), except drm_output_view_configure (obs_startup autostart).
 */

#include <stdbool.h>
#include <stdint.h>

#include "graphics/graphics.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Mailbox CLAIM: take a scanout buffer out of every mailbox role under program_lock (the
 * issue-1152 claim step). Returns its index, or -1 when nothing is writable. */
int drm_output_claim_render_buf(void);

/* Mailbox PUBLISH: hand a rendered buffer to the flip thread (skipped when a stop disarmed the
 * hook mid-render). Call gs_flush() first so the flip finds the implicit fence attached. */
void drm_output_publish_render_buf(int idx);

/* The imported render-target texture of scanout buffer `idx` (NULL when not bound). */
gs_texture_t *drm_output_render_buf_texture(int idx);

/* The set mode's active area (the scanout size). */
void drm_output_mode_size(uint32_t *w, uint32_t *h);

/* Raw byte-faithful blit of `src` into claimed buffer `idx` (sRGB encode off, blending off,
 * aspect-fit), ending with gs_flush(). The ONE copy both views use. Returns true iff it rendered
 * the buffer — publish only then. */
bool drm_output_blit_raw(gs_texture_t *src, int idx);

/* Free the view TU's GL objects (the Multiview texrender). Graphics context taken inside. */
void drm_output_view_gl_teardown(void);

/* Per-tick view step. Returns DRM_OUTPUT_TICK_PROGRAM when the caller must do the Program copy;
 * otherwise the view TU handled the tick (rendered the Multiview, or kept the last frame). */
#define DRM_OUTPUT_TICK_NOTHING 0
#define DRM_OUTPUT_TICK_PROGRAM 1
#define DRM_OUTPUT_TICK_MULTIVIEW 2
int drm_output_view_frame(void);

/* Autostart: the config's raw "view" value (NULL/"" = absent) and the config file path the
 * live switch persists into. */
void drm_output_view_configure(const char *view, const char *config_path);

/*
 * camera-box issue 1346 — the SECOND backend. The lease backend (obs-drm-output.c, issue 1152) is the
 * module's built-in default and stays exactly as it was; a backend selected by the config's "backend"
 * key is reached through this table: obs-drm-output.c routes its public entry points (start / stop /
 * active / the frame hook) and the mailbox seam above (claim / publish / texture / mode size) to it
 * while it OWNS the output. Today there is one: vk-direct (obs-drm-output-backend.c + the Vulkan core
 * obs-drm-output-vk*.c), the NVIDIA Vulkan direct-display scanout.
 */
struct obs_drm_output_config;
struct drm_output_backend_ops {
	const char *name;
	bool (*start)(const struct obs_drm_output_config *cfg);
	void (*stop)(void);    /* safe when inactive */
	bool (*active)(void);
	bool (*on_frame)(void); /* graphics thread: true = this backend owns the output (handled the tick) */
	bool (*owns)(void);     /* true = the mailbox seam routes to this backend */
	int (*claim)(void);
	void (*publish)(int idx);
	gs_texture_t *(*texture)(int idx);
	void (*mode_size)(uint32_t *w, uint32_t *h);
};

/* The NVIDIA Vulkan direct-display backend ("backend": "vk-direct"). */
extern const struct drm_output_backend_ops drm_output_vk_direct_backend;

/* The "backend" config grammar: absent/"" or "lease" -> OBS_DRM_OUTPUT_BACKEND_LEASE, "vk-direct" ->
 * OBS_DRM_OUTPUT_BACKEND_VK_DIRECT, anything else -> -1 (unknown: the autostart stays dormant). */
int drm_output_backend_from_config(const char *value);

#ifdef __cplusplus
}
#endif
