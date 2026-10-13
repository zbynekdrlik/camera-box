#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "util/c99defs.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * camera-box #1152 — in-OBS vendored DRM-lease output (M1: lease acquire + solid-color flip).
 *
 * Owner KOREKCIA (2026-08-20): the imag HDMI Program output must leave the Xorg desktop entirely.
 * Instead of an external NDI-fed presenter, our forked OBS itself acquires DRM master of the HDMI
 * connector through an X RandR output LEASE (xcb_randr_create_lease) and page-flips onto it
 * DIRECTLY (drmModePageFlip) — render->scanout, with NO NDI encode/decode hop and NO separate
 * presenter process. M1 proves the mechanism with a solid-color framebuffer; it is NOT yet bound
 * to the OBS Program render texture (that is M2 — see the "M2 HOOK" comment in obs-drm-output.c).
 *
 * DEFAULT-OFF: nothing here runs unless obs_drm_output_start() is called, OR the config file
 * ~/.camera-box/drm-output.json exists with {"enabled":true,...} (read once by
 * obs_drm_output_maybe_autostart() at obs_startup). An absent config is a dormant no-op — the
 * module ships present-but-inert, changing no OBS behaviour until deliberately opted in.
 *
 * Linux-only: the implementation is entirely under #if defined(__linux__) and is built only via
 * libobs/cmake/os-linux.cmake. On Windows/macOS these symbols are neither defined nor referenced
 * (strih+stream are libobs-d3d11, where xcb/libdrm/RandR-lease do not exist).
 */

/* camera-box issue 1346: how the output takes the connector off the X desktop. LEASE = the issue-1152
 * X RandR lease + GBM page-flip (Intel; imag). VK_DIRECT = Vulkan VK_EXT_acquire_xlib_display + a FIFO
 * display-plane swapchain (NVIDIA, whose X driver refuses the lease; strih-lx's built-in HDMI). */
enum obs_drm_output_backend {
	OBS_DRM_OUTPUT_BACKEND_LEASE = 0,
	OBS_DRM_OUTPUT_BACKEND_VK_DIRECT = 1,
};

struct obs_drm_output_config {
	/* X RandR OUTPUT name, as `xrandr` prints it, e.g. "HDMI-1". NOTE: this differs from the
	 * DRM kernel connector name ("HDMI-A-1"); the lease is requested by the RandR output XID,
	 * and the DRM connector id is discovered from the leased fd, not from this string. */
	const char *connector_name;
	/* Solid fill colour for the M1 proof pattern, packed 0x00RRGGBB (XRGB8888). */
	uint32_t solid_argb;
	/* #1152 M2: bind the OBS Program to the leased connector — GBM scanout buffers rendered
	 * on the graphics thread, page-flipped by the flip thread (zero-copy dma-buf path).
	 * false = the M1 solid diagnostic pattern only. The solid pattern also remains the
	 * initial image (before the first rendered Program frame) and the fail-open fallback
	 * when the GL bind fails. The autostart config key "program" defaults this to true. */
	bool program;
	/* camera-box issue 1346: the backend (config key "backend"; absent = LEASE). */
	enum obs_drm_output_backend backend;
};

/* camera-box issue 1424 part 2 (owner 13.10.2026): the module drives up to this many fixed outputs at
 * once -- each its own connector, backend state, view, present clock and audio leg (strih-pp: HDMI-0 =
 * the cutter's Multiview, HDMI-1 = the vestibule Program + its audio). */
#define OBS_DRM_OUTPUT_MAX_OUTPUTS 2

/* Start the output on cfg->connector_name: the instance already configured for that connector, else a new
 * instance (view Program) in a free slot. Returns true on success; a start of an instance that is already
 * active is a no-op returning true; false when it failed or no slot is free. */
EXPORT bool obs_drm_output_start(const struct obs_drm_output_config *cfg);

/* Stop EVERY instance (its audio leg first, then its scanout); each connector returns to Xorg. Safe when
 * inactive. */
EXPORT void obs_drm_output_stop(void);

/* True while ANY instance holds its connector and scans out. */
EXPORT bool obs_drm_output_active(void);

/* One-shot, DEFAULT-OFF autostart: read ~/.camera-box/drm-output.json and, only if it exists and
 * carries "enabled": true, start the output(s) -- the single-object form (one output) or the issue-1424
 * "outputs" array (up to OBS_DRM_OUTPUT_MAX_OUTPUTS, all or none: a malformed array starts NO output and
 * names why). Absent/disabled => one `drm-output:` log line and no behaviour change. Called once from
 * obs_startup(), under a __linux__ guard at the call site. */
void obs_drm_output_maybe_autostart(void);

/* camera-box issue 1424 part 2 -- the per-output API (index 0 .. obs_drm_output_count()-1). */

/* The number of configured outputs (0 when the config is absent, disabled or malformed). */
EXPORT int obs_drm_output_count(void);

/* The X RandR connector name of output `idx` ("" outside the range). */
EXPORT const char *obs_drm_output_connector(int idx);

/* True while output `idx` holds its connector and scans out. */
EXPORT bool obs_drm_output_active_of(int idx);

/* #1152 M2 — graphics-thread frame hook: called once per tick by obs_graphics_thread_loop()
 * right after the Program is composited (output_frames). When the output is active with the
 * Program binding enabled, it lazily imports the GBM scanout buffers into the OBS GL context
 * (the graphics subsystem does not exist yet at obs_startup autostart time) and renders the
 * Program into the mailbox back buffer; a cheap atomic no-op otherwise. Linux-only. */
void obs_drm_output_on_frame(void);

/*
 * camera-box issue 1346 — the selectable VIEW of the DRM-lease HDMI output (owner 24.9.2026).
 *
 * PROGRAM (the default, and what an absent "view" key means — imag stays unchanged) scans out
 * the Program canvas exactly as the issue-1152 M2 path. MULTIVIEW scans out the frontend's
 * BUILT-IN Multiview (labels, PVW/PGM tally), never a custom scene: the frontend registers a
 * renderer and libobs calls it on the graphics thread into an sRGB-capable offscreen texrender at
 * the connector mode size (the same encode a projector window gets), then blits that raw into
 * the leased scanout buffer exactly like the Program. The Multiview render runs ONLY while the view
 * is MULTIVIEW, and it is throttled by the monitoring-surface budget gate
 * (obs_aux_sender_should_skip), so the Program always has priority.
 *
 * The view is read from ~/.camera-box/drm-output.json ("view":"program"|"multiview") at
 * autostart; obs_drm_output_set_view() switches it live AND persists it into that file (every
 * other key kept), so the operator's choice survives an OBS restart.
 */
enum obs_drm_output_view {
	OBS_DRM_OUTPUT_VIEW_PROGRAM = 0,
	OBS_DRM_OUTPUT_VIEW_MULTIVIEW = 1,
};

/* Draws the view into the currently bound render target of size cx x cy (graphics thread,
 * graphics context held, viewport/ortho already set to 0..cx / 0..cy, cleared to black). */
typedef void (*obs_drm_output_view_render_t)(void *param, uint32_t cx, uint32_t cy);

/* Register (render != NULL) or clear (render == NULL) the MULTIVIEW renderer of output `idx` (issue
 * 1424 part 2: each output in the Multiview view renders its own). Serialised with the graphics thread
 * (takes the graphics context), so after it returns the previous renderer is never called again — the
 * caller may free what `param` points to. A no-op outside the range. Linux-only. */
EXPORT void obs_drm_output_set_view_renderer_of(int idx, obs_drm_output_view_render_t render, void *param);

/* The current view of output `idx` (thread-safe; Program outside the range). */
EXPORT enum obs_drm_output_view obs_drm_output_get_view_of(int idx);

/* Switch the view of output `idx` live and persist it into THAT output's entry of the drm-output config
 * the autostart read (the single-object form's "view", or its "outputs" element). Returns true when the
 * choice was persisted; false when it was not (outside the range, or the config no longer names the
 * output) -- the view still switches for this session when the output exists. */
EXPORT bool obs_drm_output_set_view_of(int idx, enum obs_drm_output_view view);

/* The single-output forms (issue 1346) = output 0. */
EXPORT void obs_drm_output_set_view_renderer(obs_drm_output_view_render_t render, void *param);
EXPORT enum obs_drm_output_view obs_drm_output_get_view(void);
EXPORT bool obs_drm_output_set_view(enum obs_drm_output_view view);

#ifdef __cplusplus
}
#endif
