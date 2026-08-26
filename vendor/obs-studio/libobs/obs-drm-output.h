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
};

/* Start the DRM-lease output: lease {connector + a free CRTC} out of X and page-flip a solid
 * colour on a dedicated vblank-locked thread. Returns true on success. A second start while
 * already active is a no-op returning true. */
EXPORT bool obs_drm_output_start(const struct obs_drm_output_config *cfg);

/* Stop the flip thread and release the lease; the connector returns to Xorg. Safe when inactive. */
EXPORT void obs_drm_output_stop(void);

/* True while the lease is held and the flip thread is running. */
EXPORT bool obs_drm_output_active(void);

/* One-shot, DEFAULT-OFF autostart: read ~/.camera-box/drm-output.json and, only if it exists and
 * carries "enabled": true, start the output. Absent/disabled => one `drm-output:` log line and no
 * behaviour change. Called once from obs_startup(), under a __linux__ guard at the call site. */
void obs_drm_output_maybe_autostart(void);

/* #1152 M2 — graphics-thread frame hook: called once per tick by obs_graphics_thread_loop()
 * right after the Program is composited (output_frames). When the output is active with the
 * Program binding enabled, it lazily imports the GBM scanout buffers into the OBS GL context
 * (the graphics subsystem does not exist yet at obs_startup autostart time) and renders the
 * Program into the mailbox back buffer; a cheap atomic no-op otherwise. Linux-only. */
void obs_drm_output_on_frame(void);

#ifdef __cplusplus
}
#endif
