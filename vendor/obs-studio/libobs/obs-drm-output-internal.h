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
 * aspect-fit), ending with gs_flush(). The ONE copy both views use. */
void drm_output_blit_raw(gs_texture_t *src, int idx);

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

#ifdef __cplusplus
}
#endif
