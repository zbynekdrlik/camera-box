#pragma once

/*
 * camera-box issue 1346 — the vk-direct CORE: an NVIDIA Vulkan direct-display scanout of one X RandR
 * output, fed from the OBS GL context through GL<->Vulkan external memory + semaphores.
 *
 * Why: strih-lx's built-in HDMI (X output HDMI-0) is driven by the NVIDIA GPU, whose X driver refuses
 * the X RandR lease the issue-1152 lease backend uses. The NVIDIA-supported way to take a display away
 * from X is VK_EXT_acquire_xlib_display + VK_KHR_display (SteamVR/Monado direct mode). Proven live on
 * strih-lx 25.9.2026 (issue 1346 STEP 0): the acquire succeeds once the output has no X CRTC, a FIFO
 * display-plane swapchain presents 60.03 fps, and the release returns the output to X DISABLED.
 *
 * This core has NO libobs graphics (gs_*) dependency: it speaks Vulkan (dlopen'd libvulkan.so.1 — no
 * link-time dependency, only the headers), X (Xlib + xcb RandR for the output lookup) and raw GL entry
 * points resolved at run time for the interop. The OBS side (the frame hook, the gs intermediate
 * texture, the mailbox seam the view TU renders through) is obs-drm-output-backend.c. The split keeps
 * this file compilable + runnable standalone on the rig with a plain EGL context
 * (tests/c/drm_output_vk_rig_harness.c), the only place the GPU path can be exercised off-OBS.
 *
 * Threads: open/halt/close run on the caller (obs_startup / obs_shutdown); gl_bind/gl_unbind/claim/
 * publish_gl on the OBS graphics thread with its GL context current; the present thread is internal.
 * Linux-only, like the rest of the module.
 */

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Number of shared GL<->Vulkan images — the same mailbox triple buffer as the lease backend:
 * front (being scanned out / re-copied each vblank), pending (a copy in flight), ready (newest). */
#define DRM_OUTPUT_VK_SHARED_IMAGES 3

/* Load Vulkan, acquire X RandR output `output_name` (e.g. "HDMI-0") away from X, build a FIFO
 * display-plane swapchain at the display's native ~60 Hz mode and start the present thread, which
 * shows `solid_argb` (0x00RRGGBB) until the first published frame. The output must have NO X CRTC
 * (strih-obs-start.sh runs `xrandr --output <name> --off` before the OBS launch); otherwise the
 * NVIDIA driver refuses the acquire. Returns true when the display is acquired and presenting. */
bool drm_output_vk_open(const char *output_name, uint32_t solid_argb);

/* Stop + join the present thread (idempotent). Nothing is presented afterwards; the display stays
 * acquired until drm_output_vk_close(). */
void drm_output_vk_halt(void);

/* halt + wait idle + release the display (it returns to X DISABLED, never back onto the desktop by
 * itself) + free every Vulkan/X object. Call drm_output_vk_gl_unbind() first when a GL bind exists. */
void drm_output_vk_close(void);

/* True between a successful open and close. */
bool drm_output_vk_is_open(void);

/* True while the present thread runs and wants frames (cleared on halt and on a present-loop death). */
bool drm_output_vk_wants_frames(void);

/* The presented mode's size (the swapchain extent). */
void drm_output_vk_mode_size(uint32_t *w, uint32_t *h);

/* GL context current: import the shared images (GL_EXT_memory_object_fd) + their semaphores
 * (GL_EXT_semaphore_fd) into the current GL context. false = fail open (the display keeps the solid
 * pattern). Idempotent once it succeeded. */
bool drm_output_vk_gl_bind(void);

/* GL context current: delete the GL-side objects (safe when never bound). */
void drm_output_vk_gl_unbind(void);

/* Mailbox CLAIM: a shared image the GL side may write now (never front/pending), -1 when none or while a
 * READY image is still waiting for the present thread (a READY image is never overwritten). */
int drm_output_vk_claim(void);

/* GL context current: copy GL texture `src_gl_name` (w x h, must equal the mode size) into claimed
 * shared image `idx`, signal its semaphore and hand it to the present thread. Returns false (and
 * publishes nothing) when not bound, the size mismatches or the output is stopping. */
bool drm_output_vk_publish_gl(int idx, unsigned int src_gl_name, uint32_t w, uint32_t h);

#ifdef __cplusplus
}
#endif
