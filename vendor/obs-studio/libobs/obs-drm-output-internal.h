#pragma once

/*
 * camera-box issue 1346 / 1424 — internal (non-exported) seam between the DRM output TUs:
 *   obs-drm-output-outputs.c  the config grammar (one output or an "outputs" array of up to
 *                             DRM_OUTPUT_MAX_OUTPUTS), the instance registry, the public start / stop /
 *                             frame-hook entry points and the mailbox seam routing (issue 1424 part 2)
 *   obs-drm-output.c          the LEASE backend: lease, flip thread, GBM scanout buffers (issue 1152)
 *   obs-drm-output-backend.c  the VK-DIRECT backend's OBS side (issue 1346)
 *   obs-drm-output-view.c     the selectable view: the frontend Multiview render + its budget gate,
 *                             the render-cost audit and the persisted "view" key
 *
 * issue 1424 part 2 (owner 13.10.2026: strih-pp drives HDMI-1 = Program + its audio AND HDMI-0 =
 * Multiview at the same time): the module's state is PER INSTANCE. Each `struct drm_output_inst` owns
 * its connector, its backend state (a lease of its own, or its own VkDisplayKHR + swapchain on the
 * shared VkDevice), its view, its present clock and its audio leg. Instances are statically allocated
 * and never freed, so a pointer to one stays valid for the process lifetime.
 *
 * Linux-only, like every TU of the module. The mailbox seam functions run on the graphics thread with
 * the graphics context held (the frame hook).
 */

#include <stdbool.h>
#include <stdint.h>

#include "graphics/graphics.h"
#include "obs-drm-output.h"
#include "obs-drm-output-audio.h"

#ifdef __cplusplus
extern "C" {
#endif

#define DRM_OUTPUT_MAX_OUTPUTS OBS_DRM_OUTPUT_MAX_OUTPUTS
/* The longest connector name an instance holds (X RandR output names are a few bytes). */
#define DRM_OUTPUT_CONNECTOR_MAX 64

/* The view state of one instance (obs-drm-output-view.c). `view` is atomic (UI thread writes, graphics
 * and audio threads read); the renderer, the texrender and the budget/audit fields are guarded by the
 * GRAPHICS CONTEXT (the frame hook holds it for the whole tick, the renderer setter takes it). */
struct drm_output_view_state {
	volatile long view;
	obs_drm_output_view_render_t render;
	void *render_param;
	gs_texrender_t *texrender; /* the Multiview's sRGB render target, created on first render */
	bool warned_no_renderer;
	bool warned_no_texrender;
	bool bind_live_logged;
	uint32_t frame_counter;
	uint32_t consecutive_skips;
	uint64_t ewma_ns;
	/* main design 5840501628: the cost of the last Multiview render and the graphics tick it ran in
	 * (obs_get_total_frames()), so the budget gate can leave this view's own render out of the
	 * previous tick's total (drm_output_view_self_last_ns). last_self_ns = the value the gate last
	 * used (the audit line's self_ns=). */
	uint64_t last_render_ns;
	uint32_t last_render_frame;
	uint64_t last_self_ns;

	uint64_t win_start_ns;
	uint32_t win_renders;
	uint32_t win_skips;
	uint64_t win_sum_ns;
	uint64_t win_max_ns;
};

struct drm_output_inst;

/* A backend (how the instance's connector leaves the X desktop). The lease backend (obs-drm-output.c,
 * issue 1152) and the vk-direct backend (obs-drm-output-backend.c, issue 1346). Every function takes the
 * instance; the backend keeps its own per-instance state in its own TU, indexed by inst->index. */
struct drm_output_backend_ops {
	const char *name;
	bool (*start)(struct drm_output_inst *inst);
	void (*stop)(struct drm_output_inst *inst); /* safe when inactive */
	bool (*active)(struct drm_output_inst *inst);
	void (*on_frame)(struct drm_output_inst *inst); /* graphics thread */
	int (*claim)(struct drm_output_inst *inst);
	void (*publish)(struct drm_output_inst *inst, int idx);
	gs_texture_t *(*texture)(struct drm_output_inst *inst, int idx);
	void (*mode_size)(struct drm_output_inst *inst, uint32_t *w, uint32_t *h);
};

extern const struct drm_output_backend_ops drm_output_lease_backend;     /* "backend": "lease" */
extern const struct drm_output_backend_ops drm_output_vk_direct_backend; /* "backend": "vk-direct" */

/* One fixed HDMI output. Written once by the registry before it is published (the registry's count),
 * read by every thread afterwards; the mutable parts are each guarded where they are declared. */
struct drm_output_inst {
	int index;       /* 0 .. DRM_OUTPUT_MAX_OUTPUTS-1 = the backends' per-instance state slot */
	int config_slot; /* -1 = the single-object config form, else its "outputs" array position */
	char connector[DRM_OUTPUT_CONNECTOR_MAX];
	enum obs_drm_output_backend backend;
	uint32_t solid_argb;
	bool program; /* false = the "program": false M1 solid diagnostic */
	const struct drm_output_backend_ops *ops;
	struct drm_output_view_state view;
	struct drm_output_present_clock clock; /* issue 1424: render tick -> on screen of THIS output */
	struct drm_output_audio *audio;        /* issue 1424: THIS connector's own HDMI audio leg */
	bool audio_enabled;                    /* issue 1424 part 3: false = its PCM is owned elsewhere */
	bool started;                          /* the last start succeeded (registry lock) */
};

/* The instance registry (obs-drm-output-outputs.c). drm_output_inst_at returns NULL outside 0..count-1. */
int drm_output_inst_count(void);
struct drm_output_inst *drm_output_inst_at(int idx);

/* Mailbox CLAIM: take a scanout buffer of `inst` out of every mailbox role (the issue-1152 claim step).
 * Returns its index, or -1 when nothing is writable. */
int drm_output_claim_render_buf(struct drm_output_inst *inst);

/* Mailbox PUBLISH: hand a rendered buffer to the instance's scanout thread (skipped when a stop disarmed
 * the hook mid-render). Call gs_flush() first so the flip finds the implicit fence attached. Notes the
 * render tick on the instance's present clock (issue 1424). */
void drm_output_publish_render_buf(struct drm_output_inst *inst, int idx);

/* The render-target texture of scanout buffer `idx` of `inst` (NULL when not bound). */
gs_texture_t *drm_output_render_buf_texture(struct drm_output_inst *inst, int idx);

/* The instance's set mode's active area (the scanout size). */
void drm_output_mode_size(struct drm_output_inst *inst, uint32_t *w, uint32_t *h);

/* Raw byte-faithful blit of `src` into claimed buffer `idx` of `inst` (sRGB encode off, blending off,
 * aspect-fit), ending with gs_flush(). The ONE copy both views and both backends use. Returns true iff it
 * rendered the buffer — publish only then. */
bool drm_output_blit_raw(struct drm_output_inst *inst, gs_texture_t *src, int idx);

/* Free the instance's view GL objects (the Multiview texrender). Graphics context taken inside. */
void drm_output_view_gl_teardown(struct drm_output_inst *inst);

/* Per-tick view step of `inst`. Returns DRM_OUTPUT_TICK_PROGRAM when the caller must do the Program copy;
 * otherwise the view TU handled the tick (rendered the Multiview, or kept the last frame). */
#define DRM_OUTPUT_TICK_NOTHING 0
#define DRM_OUTPUT_TICK_PROGRAM 1
#define DRM_OUTPUT_TICK_MULTIVIEW 2
int drm_output_view_frame(struct drm_output_inst *inst);

/* The instance's initial view (OBS_DRM_OUTPUT_VIEW_*), set by the registry before the instance is
 * published, plus the one `view=` log line. */
void drm_output_view_configure(struct drm_output_inst *inst, int view);

/* The "view" config grammar (obs-drm-output-view.c): absent/"" or "program" -> 0, "multiview" -> 1, else -1. */
int drm_output_view_from_config(const char *value);

/* The "backend" config grammar (obs-drm-output-backend.c): absent/"" or "lease" -> 0, "vk-direct" -> 1,
 * anything else -> -1 (unknown: the autostart stays dormant). */
int drm_output_backend_from_config(const char *value);

/* Persist `view` ("program" | "multiview") of `inst` into the config file the autostart read
 * (obs-drm-output-outputs.c). NULL on success, else the reason it was NOT persisted. */
const char *drm_output_config_persist_view(const struct drm_output_inst *inst, const char *view);

#ifdef __cplusplus
}
#endif
