/*
 * camera-box issue 1346 — vk-direct CORE: NVIDIA Vulkan direct-display scanout of one X RandR output.
 *
 * See obs-drm-output-vk.h for the contract and why this backend exists. In short: the owner wants
 * strih-lx's BUILT-IN HDMI (NVIDIA, X output HDMI-0) to be a fixed, indestructible output like imag's
 * (never an X window, no mouse), and NVIDIA's X driver refuses the RandR lease the issue-1152 backend
 * uses. VK_EXT_acquire_xlib_display takes the display away from X instead (STEP 0 proof, 25.9.2026).
 *
 * Data path per OBS tick (graphics thread): the frontend's view (Program / built-in Multiview) is
 * blitted raw into a gs intermediate render target (obs-drm-output-backend.c), then
 * drm_output_vk_publish_gl() copies it (glCopyImageSubData — a byte copy, no bound GL state touched)
 * into one of DRM_OUTPUT_VK_SHARED_IMAGES Vulkan images that the GL context imported through
 * GL_EXT_memory_object_fd, signals that image's GL_EXT_semaphore_fd semaphore and marks it READY.
 * The present thread (FIFO = vblank paced by vkAcquireNextImageKHR) blits the newest READY image — or
 * re-blits the FRONT one when nothing new arrived — into the acquired swapchain image and presents.
 *
 * Mailbox + sync (the lease backend's triple buffer, adapted to a copy-based consumer):
 *   - roles front / pending / ready live under g_drm_vk.lock; GL only ever writes a role-free image;
 *   - GL -> Vulkan: the per-image binary semaphore GL signals on publish; the present thread waits it
 *     exactly once (the first copy of that publish). The GL side NEVER overwrites a READY image (it
 *     skips the tick while one is waiting), so every signal is waited by the Vulkan side and no GL-side
 *     wait on the same semaphore exists. (The first design kept a "latest wins" overwrite and consumed
 *     the overwritten signal with a GL-side glWaitSemaphoreEXT; that DEADLOCKED the copy live under a
 *     publish burst on strih-lx, 25.9.2026 -- the likely cause is the GL wait still pending when the
 *     Vulkan side waited the same binary semaphore. Removing every GL-side wait removes the question.)
 *   - Vulkan -> GL: the present thread waits its fence before it changes a role, so when an image
 *     leaves front/pending every Vulkan read of it has completed on the GPU — a later GL write is
 *     ordered after it without a second semaphore. Every copy acquires the image from
 *     VK_QUEUE_FAMILY_EXTERNAL and releases it back. The GL side never does the matching GL-side
 *     acquire/release (GL_EXT_semaphore only carries layouts on the semaphore ops): this relies on the
 *     NVIDIA driver being lenient about external ownership, proven live on the target GPU.
 *   - the waits a wedged GPU could hang are bounded: the present loop's acquire/fence waits (1 s,
 *     re-checked) and the teardown's quiesce of an outstanding copy (a timeout leaks the Vulkan objects
 *     loudly instead of hanging the OBS shutdown). The teardown's vkDeviceWaitIdle runs only once no
 *     submit is outstanding, and the swapchain rebuild never waits on the device at all.
 *
 * The present thread never touches GL; the graphics thread never touches the Vulkan queue. Building
 * the Vulkan/X objects (and tearing them down) is obs-drm-output-vk-setup.c; the shared state type is
 * obs-drm-output-vk-internal.h.
 * Linux-only; built only via libobs/cmake/os-linux.cmake (Vulkan headers only — libvulkan.so.1 is
 * dlopen'd, so a box without the loader just logs and stays dormant). The pure helpers
 * drm_output_vk_present_pick + drm_output_vk_present_done + drm_output_vk_pick_claim are lift-compiled +
 * model-checked by
 * tests/drm_output_vk_direct_1346.rs.
 */

#if defined(__linux__)

#ifndef _GNU_SOURCE
#define _GNU_SOURCE /* RTLD_DEFAULT (the GL entry-point lookup) */
#endif
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "util/base.h"
#include "util/threading.h"
#include "obs-drm-output-vk-internal.h"

/* -------------------------------------------------------------------------------------------------
 * Pure decision helpers (Tier-0, lift-compiled + truth-tabled by tests/drm_output_vk_direct_1346.rs).
 * ------------------------------------------------------------------------------------------------- */

/* The present thread's per-vblank source: the READY image when one is waiting (it moves to pending,
 * *took_new = true -- this submit waits the image's GL signal, exactly once), else the FRONT image (a
 * re-copy that waits nothing), else -1 (nothing published yet — present the solid pattern). Caller holds
 * g_drm_vk.lock. */
static int drm_output_vk_present_pick(int front, int *pending, int *ready, bool *took_new)
{
	*took_new = false;
	if (*ready >= 0) {
		int src = *ready;
		*ready = -1;
		*pending = src;
		*took_new = true;
		return src;
	}
	return front;
}

/* After the copy's fence signalled: a taken image leaves pending and becomes the front (never on a
 * failed/timed-out fence — the copy may still read it). Caller holds g_drm_vk.lock. */
static void drm_output_vk_present_done(int src, bool took_new, int *front, int *pending)
{
	if (!took_new)
		return;
	if (*pending == src)
		*pending = -1;
	*front = src;
}

/* The GL side's claim: -1 while a READY image is waiting (its signal must be waited by the present
 * thread before anything else is published -- never overwritten), else the first image holding no role
 * (never front or pending: the present thread may still read them). With nothing ready this is exactly
 * the lease backend's drm_output_pick_render_buf. A skipped tick drops the NEWER frame where the lease
 * drops the older one: one frame either way, the next tick publishes again. */
static int drm_output_vk_pick_claim(int front, int pending, int ready, int n)
{
	if (ready >= 0)
		return -1;
	for (int i = 0; i < n; i++) {
		if (i != front && i != pending)
			return i;
	}
	return -1;
}

/* -------------------------------------------------------------------------------------------------
 * Module state (single instance; its type is in obs-drm-output-vk-internal.h).
 * ------------------------------------------------------------------------------------------------- */
struct drm_output_vk_state g_drm_vk = {
	.lock = PTHREAD_MUTEX_INITIALIZER,
	.front = -1,
	.pending = -1,
	.ready = -1,
};

static void drm_output_vk_color_barrier(VkImage img, VkImageLayout from, VkImageLayout to, VkAccessFlags src_access,
					VkAccessFlags dst_access, uint32_t src_qfi, uint32_t dst_qfi,
					VkPipelineStageFlags src_stage, VkPipelineStageFlags dst_stage)
{
	VkImageMemoryBarrier b = {.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
				  .srcAccessMask = src_access,
				  .dstAccessMask = dst_access,
				  .oldLayout = from,
				  .newLayout = to,
				  .srcQueueFamilyIndex = src_qfi,
				  .dstQueueFamilyIndex = dst_qfi,
				  .image = img,
				  .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
	g_drm_vk.vk.vkCmdPipelineBarrier(g_drm_vk.cmd, src_stage, dst_stage, 0, 0, NULL, 0, NULL, 1, &b);
}

/* Record this vblank's command buffer: `src` shared image blitted into swapchain image `img`, or the
 * solid pattern when src < 0. */
static bool drm_output_vk_record(uint32_t img, int src)
{
	VkImage dst = g_drm_vk.swap_images[img];
	if (g_drm_vk.vk.vkResetCommandBuffer(g_drm_vk.cmd, 0) != VK_SUCCESS)
		return false;
	VkCommandBufferBeginInfo bi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
				       .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT};
	if (g_drm_vk.vk.vkBeginCommandBuffer(g_drm_vk.cmd, &bi) != VK_SUCCESS)
		return false;
	drm_output_vk_color_barrier(dst, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 0,
				    VK_ACCESS_TRANSFER_WRITE_BIT, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED,
				    VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
	if (src >= 0) {
		VkImage s = g_drm_vk.shared[src].image;
		/* acquire from the GL side (the layout GL signalled: GL_LAYOUT_TRANSFER_SRC_EXT) */
		drm_output_vk_color_barrier(s, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
					    0, VK_ACCESS_TRANSFER_READ_BIT, VK_QUEUE_FAMILY_EXTERNAL, g_drm_vk.qfi,
					    VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
		VkImageBlit region = {
			.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
			.srcOffsets = {{0, 0, 0}, {(int32_t)g_drm_vk.mode_w, (int32_t)g_drm_vk.mode_h, 1}},
			.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
			.dstOffsets = {{0, 0, 0}, {(int32_t)g_drm_vk.mode_w, (int32_t)g_drm_vk.mode_h, 1}},
		};
		/* A 1:1 blit: RGBA8 -> the swapchain's BGRA8, component-wise, bytes unchanged (no sRGB
		 * conversion between two UNORM formats — the same byte-faithful transfer as the lease copy). */
		g_drm_vk.vk.vkCmdBlitImage(g_drm_vk.cmd, s, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, dst,
				       VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region, VK_FILTER_NEAREST);
		/* release back to the GL side */
		drm_output_vk_color_barrier(s, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
					    VK_ACCESS_TRANSFER_READ_BIT, 0, g_drm_vk.qfi, VK_QUEUE_FAMILY_EXTERNAL,
					    VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT);
	} else {
		VkClearColorValue c = {.float32 = {(float)((g_drm_vk.solid_argb >> 16) & 0xFFu) / 255.0f,
						   (float)((g_drm_vk.solid_argb >> 8) & 0xFFu) / 255.0f,
						   (float)(g_drm_vk.solid_argb & 0xFFu) / 255.0f, 1.0f}};
		VkImageSubresourceRange rng = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
		g_drm_vk.vk.vkCmdClearColorImage(g_drm_vk.cmd, dst, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &c, 1, &rng);
	}
	drm_output_vk_color_barrier(dst, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
				    VK_ACCESS_TRANSFER_WRITE_BIT, 0, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED,
				    VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT);
	return g_drm_vk.vk.vkEndCommandBuffer(g_drm_vk.cmd) == VK_SUCCESS;
}

/* Wait the per-vblank fence, bounded so a stop is never blocked by a wedged GPU. true = signalled. */
static bool drm_output_vk_wait_fence(void)
{
	unsigned overdue = 0;
	for (;;) {
		VkResult r = g_drm_vk.vk.vkWaitForFences(g_drm_vk.device, 1, &g_drm_vk.fence, VK_TRUE, DRM_OUTPUT_VK_WAIT_NS);
		if (r == VK_SUCCESS)
			return true;
		if (r != VK_TIMEOUT) {
			blog(LOG_WARNING, "drm-output: vk-direct: vkWaitForFences FAILED VkResult %d -- stopping", (int)r);
			return false;
		}
		if (++overdue % 5u == 0u)
			blog(LOG_WARNING, "drm-output: vk-direct: copy completion overdue (%us) -- possible GPU wedge",
			     overdue);
		if (!os_atomic_load_bool(&g_drm_vk.running) && overdue >= 5u)
			return false;
	}
}

/* An out-of-date / lost display surface (an HDMI replug, a sink re-plug) is NOT the end of the output:
 * rebuild the swapchain (+ the surface when lost) with bounded retries, 1 s apart, so the fixed output
 * comes back by itself. Gives up (the loop exits, `present loop exited` names it) after
 * DRM_OUTPUT_VK_REBUILD_TRIES consecutive failures or when the display's mode size changed (the shared
 * images no longer fit — an OBS restart re-opens at the new size). */
#define DRM_OUTPUT_VK_REBUILD_TRIES 10u
static bool drm_output_vk_rebuild_or_give_up(VkResult why, unsigned *rebuilds)
{
	while (os_atomic_load_bool(&g_drm_vk.running) && *rebuilds < DRM_OUTPUT_VK_REBUILD_TRIES) {
		(*rebuilds)++;
		blog(LOG_WARNING, "drm-output: vk-direct: presentation out of date (VkResult %d) on '%s' -- rebuilding (try %u of %u)",
		     (int)why, g_drm_vk.output_name, *rebuilds, DRM_OUTPUT_VK_REBUILD_TRIES);
		/* a lost surface is rebuilt whole; an out-of-date one keeps its surface once, then escalates */
		if (drm_output_vk_rebuild_presentation(why == VK_ERROR_SURFACE_LOST_KHR || *rebuilds >= 2u))
			return true;
		const struct timespec pause = {1, 0};
		nanosleep(&pause, NULL);
	}
	blog(LOG_WARNING, "drm-output: vk-direct: presentation could not be rebuilt on '%s' -- giving up",
	     g_drm_vk.output_name);
	return false;
}

/* The vblank-paced present loop (FIFO: vkAcquireNextImageKHR blocks until a swapchain image frees). */
static void *drm_output_vk_present_thread(void *arg)
{
	(void)arg;
	unsigned overdue = 0;
	unsigned rebuilds = 0;
	while (os_atomic_load_bool(&g_drm_vk.running)) {
		uint32_t img = 0;
		VkResult r = g_drm_vk.vk.vkAcquireNextImageKHR(g_drm_vk.device, g_drm_vk.swapchain, DRM_OUTPUT_VK_WAIT_NS,
							   g_drm_vk.sem_acquire, VK_NULL_HANDLE, &img);
		if (r == VK_TIMEOUT || r == VK_NOT_READY) {
			if (++overdue % 5u == 0u)
				blog(LOG_WARNING,
				     "drm-output: vk-direct: swapchain image overdue (%us) on '%s' -- possible display wedge",
				     overdue, g_drm_vk.output_name);
			continue;
		}
		if (r == VK_ERROR_OUT_OF_DATE_KHR || r == VK_ERROR_SURFACE_LOST_KHR) {
			if (!drm_output_vk_rebuild_or_give_up(r, &rebuilds))
				break;
			continue;
		}
		if (r != VK_SUCCESS && r != VK_SUBOPTIMAL_KHR) {
			blog(LOG_WARNING, "drm-output: vk-direct: vkAcquireNextImageKHR FAILED VkResult %d -- stopping",
			     (int)r);
			break;
		}
		overdue = 0;

		bool took_new = false;
		pthread_mutex_lock(&g_drm_vk.lock);
		int src = drm_output_vk_present_pick(g_drm_vk.front, &g_drm_vk.pending, &g_drm_vk.ready, &took_new);
		pthread_mutex_unlock(&g_drm_vk.lock);
		const bool wait_gl = took_new; /* a newly taken image carries exactly one GL signal */

		if (!drm_output_vk_record(img, src)) {
			blog(LOG_WARNING, "drm-output: vk-direct: command recording failed -- stopping");
			break;
		}
		VkSemaphore waits[2] = {g_drm_vk.sem_acquire, wait_gl ? g_drm_vk.shared[src].sem : VK_NULL_HANDLE};
		VkPipelineStageFlags stages[2] = {VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT};
		VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
				   .waitSemaphoreCount = wait_gl ? 2u : 1u,
				   .pWaitSemaphores = waits,
				   .pWaitDstStageMask = stages,
				   .commandBufferCount = 1,
				   .pCommandBuffers = &g_drm_vk.cmd,
				   .signalSemaphoreCount = 1,
				   .pSignalSemaphores = &g_drm_vk.sem_done[img]};
		r = g_drm_vk.vk.vkQueueSubmit(g_drm_vk.queue, 1, &si, g_drm_vk.fence);
		if (r != VK_SUCCESS) {
			blog(LOG_WARNING, "drm-output: vk-direct: vkQueueSubmit FAILED VkResult %d -- stopping", (int)r);
			break;
		}
		g_drm_vk.submit_outstanding = true;
		VkPresentInfoKHR pi = {.sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
				       .waitSemaphoreCount = 1,
				       .pWaitSemaphores = &g_drm_vk.sem_done[img],
				       .swapchainCount = 1,
				       .pSwapchains = &g_drm_vk.swapchain,
				       .pImageIndices = &img};
		VkResult pr = g_drm_vk.vk.vkQueuePresentKHR(g_drm_vk.queue, &pi);
		if (!drm_output_vk_wait_fence())
			break; /* roles untouched: the copy may still read the image; the teardown quiesces */
		g_drm_vk.vk.vkResetFences(g_drm_vk.device, 1, &g_drm_vk.fence);
		g_drm_vk.submit_outstanding = false;
		if (g_drm_vk.retire_countdown && --g_drm_vk.retire_countdown == 0u)
			drm_output_vk_free_retired(); /* the replaced swapchain's last present has long completed */

		/* Roles change only after the fence: every Vulkan read of the old front has completed, so the
		 * GL side may overwrite it from now on. */
		pthread_mutex_lock(&g_drm_vk.lock);
		drm_output_vk_present_done(src, took_new, &g_drm_vk.front, &g_drm_vk.pending);
		pthread_mutex_unlock(&g_drm_vk.lock);

		if (pr == VK_ERROR_OUT_OF_DATE_KHR || pr == VK_ERROR_SURFACE_LOST_KHR) {
			if (!drm_output_vk_rebuild_or_give_up(pr, &rebuilds))
				break;
			continue;
		}
		if (pr != VK_SUCCESS && pr != VK_SUBOPTIMAL_KHR) {
			blog(LOG_WARNING, "drm-output: vk-direct: vkQueuePresentKHR FAILED VkResult %d -- stopping", (int)pr);
			break;
		}
		rebuilds = 0;
		g_drm_vk.presents++;
		if (src >= 0) {
			g_drm_vk.program_presents++;
			if (!g_drm_vk.scanout_live_logged) {
				g_drm_vk.scanout_live_logged = true;
				blog(LOG_INFO,
				     "drm-output: program scanout LIVE (vk-direct: a published frame reached '%s')",
				     g_drm_vk.output_name);
			}
			if (g_drm_vk.program_presents == 1ULL || g_drm_vk.program_presents % 3600ULL == 0ULL)
				blog(LOG_INFO, "drm-output: program-present #%llu (vk-direct FIFO)", g_drm_vk.program_presents);
		} else if (g_drm_vk.presents == 1ULL || g_drm_vk.presents % 3600ULL == 0ULL) {
			blog(LOG_INFO, "drm-output: solid-present #%llu (vk-direct FIFO, the pattern before the first frame)", g_drm_vk.presents);
		}
	}
	/* A self-death (display lost, device lost) disarms the frame hook too: nobody drains the mailbox. */
	os_atomic_set_bool(&g_drm_vk.want_frames, false);
	blog(LOG_INFO, "drm-output: vk-direct present loop exited after %llu presents (%llu publishes skipped behind a ready frame)",
	     g_drm_vk.presents, g_drm_vk.gl_skips);
	return NULL;
}

bool drm_output_vk_open(const char *output_name, uint32_t solid_argb)
{
	if (g_drm_vk.open)
		return true;
	if (!output_name || !output_name[0] || strlen(output_name) >= sizeof(g_drm_vk.output_name)) {
		blog(LOG_WARNING, "drm-output: vk-direct: bad output name -- output dormant");
		return false;
	}
	snprintf(g_drm_vk.output_name, sizeof(g_drm_vk.output_name), "%s", output_name);
	g_drm_vk.solid_argb = solid_argb;
	g_drm_vk.presents = 0;
	g_drm_vk.program_presents = 0;
	g_drm_vk.scanout_live_logged = false;
	g_drm_vk.gl_skips = 0;
	g_drm_vk.submit_outstanding = false;
	g_drm_vk.retire_countdown = 0;

	if (!drm_output_vk_setup(output_name)) {
		drm_output_vk_destroy_all();
		blog(LOG_WARNING, "drm-output: vk-direct start FAILED for '%s' -- output dormant", output_name);
		return false;
	}
	g_drm_vk.open = true;
	os_atomic_set_bool(&g_drm_vk.running, true);
	os_atomic_set_bool(&g_drm_vk.want_frames, true);
	if (pthread_create(&g_drm_vk.thread, NULL, drm_output_vk_present_thread, NULL) != 0) {
		os_atomic_set_bool(&g_drm_vk.running, false);
		os_atomic_set_bool(&g_drm_vk.want_frames, false);
		drm_output_vk_destroy_all();
		blog(LOG_WARNING, "drm-output: vk-direct: could not create the present thread");
		return false;
	}
	g_drm_vk.thread_started = true;
	return true;
}

void drm_output_vk_halt(void)
{
	os_atomic_set_bool(&g_drm_vk.want_frames, false);
	os_atomic_set_bool(&g_drm_vk.running, false);
	if (g_drm_vk.thread_started) {
		pthread_join(g_drm_vk.thread, NULL);
		g_drm_vk.thread_started = false;
	}
}

void drm_output_vk_close(void)
{
	drm_output_vk_halt();
	if (g_drm_vk.open || g_drm_vk.dpy || g_drm_vk.lib)
		drm_output_vk_destroy_all();
}

bool drm_output_vk_is_open(void)
{
	return g_drm_vk.open;
}

bool drm_output_vk_wants_frames(void)
{
	return os_atomic_load_bool(&g_drm_vk.want_frames);
}

void drm_output_vk_mode_size(uint32_t *w, uint32_t *h)
{
	*w = g_drm_vk.mode_w;
	*h = g_drm_vk.mode_h;
}

/* -------------------------------------------------------------------------------------------------
 * GL side (graphics thread, the OBS GL context current).
 * ------------------------------------------------------------------------------------------------- */

typedef void *(*drm_output_vk_getproc_fn)(const char *name);

/* Resolve a GL entry point in the CURRENT context's library: eglGetProcAddress (OBS on Linux runs
 * EGL/X11 — "Using EGL/X11" in the OBS log), else glXGetProcAddressARB, else a plain symbol. */
static void *drm_output_vk_gl_proc(const char *name)
{
	static drm_output_vk_getproc_fn egl, glx;
	static bool looked;
	if (!looked) {
		looked = true;
		egl = (drm_output_vk_getproc_fn)dlsym(RTLD_DEFAULT, "eglGetProcAddress");
		glx = (drm_output_vk_getproc_fn)dlsym(RTLD_DEFAULT, "glXGetProcAddressARB");
		/* libobs-opengl may have loaded libEGL/libGLX privately (RTLD_LOCAL): ask the already-loaded
		 * library directly (RTLD_NOLOAD never loads a second copy). */
		if (!egl) {
			void *h = dlopen("libEGL.so.1", RTLD_NOW | RTLD_NOLOAD);
			if (h)
				egl = (drm_output_vk_getproc_fn)dlsym(h, "eglGetProcAddress");
		}
		if (!glx) {
			void *h = dlopen("libGLX.so.0", RTLD_NOW | RTLD_NOLOAD);
			if (h)
				glx = (drm_output_vk_getproc_fn)dlsym(h, "glXGetProcAddressARB");
		}
	}
	void *p = egl ? egl(name) : NULL;
	if (!p && glx)
		p = glx(name);
	if (!p)
		p = dlsym(RTLD_DEFAULT, name);
	return p;
}

static bool drm_output_vk_gl_resolve(void)
{
	struct drm_output_vk_gl *g = &g_drm_vk.gl;
	bool ok = true;
#define DRM_OUTPUT_VK_GL(field, name, type)                                                          \
	g->field = (type)drm_output_vk_gl_proc(name);                                                \
	if (!g->field) {                                                                             \
		blog(LOG_WARNING, "drm-output: vk-direct: GL entry point %s missing", name);         \
		ok = false;                                                                          \
	}
	DRM_OUTPUT_VK_GL(CreateMemoryObjectsEXT, "glCreateMemoryObjectsEXT", PFNGLCREATEMEMORYOBJECTSEXTPROC)
	DRM_OUTPUT_VK_GL(DeleteMemoryObjectsEXT, "glDeleteMemoryObjectsEXT", PFNGLDELETEMEMORYOBJECTSEXTPROC)
	DRM_OUTPUT_VK_GL(MemoryObjectParameterivEXT, "glMemoryObjectParameterivEXT",
			 PFNGLMEMORYOBJECTPARAMETERIVEXTPROC)
	DRM_OUTPUT_VK_GL(ImportMemoryFdEXT, "glImportMemoryFdEXT", PFNGLIMPORTMEMORYFDEXTPROC)
	DRM_OUTPUT_VK_GL(CreateTextures, "glCreateTextures", PFNGLCREATETEXTURESPROC)
	DRM_OUTPUT_VK_GL(DeleteTextures, "glDeleteTextures", drm_output_vk_gl_delete_textures_fn)
	DRM_OUTPUT_VK_GL(TextureParameteri, "glTextureParameteri", PFNGLTEXTUREPARAMETERIPROC)
	DRM_OUTPUT_VK_GL(TextureStorageMem2DEXT, "glTextureStorageMem2DEXT", PFNGLTEXTURESTORAGEMEM2DEXTPROC)
	DRM_OUTPUT_VK_GL(GenSemaphoresEXT, "glGenSemaphoresEXT", PFNGLGENSEMAPHORESEXTPROC)
	DRM_OUTPUT_VK_GL(DeleteSemaphoresEXT, "glDeleteSemaphoresEXT", PFNGLDELETESEMAPHORESEXTPROC)
	DRM_OUTPUT_VK_GL(ImportSemaphoreFdEXT, "glImportSemaphoreFdEXT", PFNGLIMPORTSEMAPHOREFDEXTPROC)
	DRM_OUTPUT_VK_GL(SignalSemaphoreEXT, "glSignalSemaphoreEXT", PFNGLSIGNALSEMAPHOREEXTPROC)
	DRM_OUTPUT_VK_GL(CopyImageSubData, "glCopyImageSubData", PFNGLCOPYIMAGESUBDATAPROC)
	DRM_OUTPUT_VK_GL(Flush, "glFlush", drm_output_vk_gl_flush_fn)
	DRM_OUTPUT_VK_GL(Finish, "glFinish", drm_output_vk_gl_flush_fn)
	DRM_OUTPUT_VK_GL(GetError, "glGetError", drm_output_vk_gl_get_error_fn)
#undef DRM_OUTPUT_VK_GL
	return ok;
}

void drm_output_vk_gl_unbind(void)
{
	struct drm_output_vk_gl *g = &g_drm_vk.gl;
	/* GL copy/signal work may still be in flight on the shared images: finish it before the GL objects
	 * (and then the Vulkan memory behind them) go away. */
	if (g_drm_vk.gl_bound && g->Finish)
		g->Finish();
	for (int i = 0; i < DRM_OUTPUT_VK_SHARED_IMAGES; i++) {
		struct drm_output_vk_shared *s = &g_drm_vk.shared[i];
		if (s->gl_tex && g->DeleteTextures)
			g->DeleteTextures(1, &s->gl_tex);
		if (s->gl_mem && g->DeleteMemoryObjectsEXT)
			g->DeleteMemoryObjectsEXT(1, &s->gl_mem);
		if (s->gl_sem && g->DeleteSemaphoresEXT)
			g->DeleteSemaphoresEXT(1, &s->gl_sem);
		s->gl_tex = 0;
		s->gl_mem = 0;
		s->gl_sem = 0;
	}
	g_drm_vk.gl_bound = false;
}

bool drm_output_vk_gl_bind(void)
{
	if (g_drm_vk.gl_bound)
		return true;
	if (!g_drm_vk.open || !drm_output_vk_gl_resolve()) {
		blog(LOG_WARNING, "drm-output: program bind FAILED (vk-direct: GL interop entry points missing) -- "
				  "staying on the solid pattern");
		return false;
	}
	struct drm_output_vk_gl *g = &g_drm_vk.gl;
	/* drain errors left by earlier OBS GL work so they are not blamed on the import (bounded: a lost
	 * context may report an error forever) */
	for (int drain = 0; drain < 16 && g->GetError() != GL_NO_ERROR; drain++) {
	}
	for (int i = 0; i < DRM_OUTPUT_VK_SHARED_IMAGES; i++) {
		struct drm_output_vk_shared *s = &g_drm_vk.shared[i];
		VkMemoryGetFdInfoKHR mfi = {.sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
					    .memory = s->memory,
					    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT};
		int mem_fd = -1;
		VkSemaphoreGetFdInfoKHR sfi = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR,
					       .semaphore = s->sem,
					       .handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT};
		int sem_fd = -1;
		if (g_drm_vk.vk.vkGetMemoryFdKHR(g_drm_vk.device, &mfi, &mem_fd) != VK_SUCCESS ||
		    g_drm_vk.vk.vkGetSemaphoreFdKHR(g_drm_vk.device, &sfi, &sem_fd) != VK_SUCCESS) {
			if (mem_fd >= 0)
				close(mem_fd);
			blog(LOG_WARNING, "drm-output: program bind FAILED (vk-direct: fd export of shared image %d) -- "
					  "staying on the solid pattern",
			     i);
			drm_output_vk_gl_unbind();
			return false;
		}
		/* GL takes ownership of both fds on a successful import. */
		const GLint dedicated = GL_TRUE;
		g->CreateMemoryObjectsEXT(1, &s->gl_mem);
		g->MemoryObjectParameterivEXT(s->gl_mem, GL_DEDICATED_MEMORY_OBJECT_EXT, &dedicated);
		g->ImportMemoryFdEXT(s->gl_mem, (GLuint64)s->size, GL_HANDLE_TYPE_OPAQUE_FD_EXT, mem_fd);
		/* DSA: no texture binding changes behind the libobs GL state cache. */
		g->CreateTextures(GL_TEXTURE_2D, 1, &s->gl_tex);
		g->TextureParameteri(s->gl_tex, GL_TEXTURE_TILING_EXT, GL_OPTIMAL_TILING_EXT);
		g->TextureStorageMem2DEXT(s->gl_tex, 1, GL_RGBA8, (GLsizei)g_drm_vk.mode_w, (GLsizei)g_drm_vk.mode_h, s->gl_mem,
					  0);
		g->GenSemaphoresEXT(1, &s->gl_sem);
		g->ImportSemaphoreFdEXT(s->gl_sem, GL_HANDLE_TYPE_OPAQUE_FD_EXT, sem_fd);
		GLenum err = g->GetError();
		if (err != GL_NO_ERROR) {
			blog(LOG_WARNING,
			     "drm-output: program bind FAILED (vk-direct: GL import of shared image %d, GL error 0x%x) -- "
			     "staying on the solid pattern",
			     i, (unsigned)err);
			drm_output_vk_gl_unbind();
			return false;
		}
	}
	g_drm_vk.gl_bound = true;
	blog(LOG_INFO,
	     "drm-output: program bind ready (vk-direct: %d shared images %ux%u, GL memory-object + semaphore import)",
	     DRM_OUTPUT_VK_SHARED_IMAGES, g_drm_vk.mode_w, g_drm_vk.mode_h);
	return true;
}

int drm_output_vk_claim(void)
{
	pthread_mutex_lock(&g_drm_vk.lock);
	int idx = drm_output_vk_pick_claim(g_drm_vk.front, g_drm_vk.pending, g_drm_vk.ready, DRM_OUTPUT_VK_SHARED_IMAGES);
	if (idx < 0)
		g_drm_vk.gl_skips++;
	pthread_mutex_unlock(&g_drm_vk.lock);
	return idx;
}

bool drm_output_vk_publish_gl(int idx, unsigned int src_gl_name, uint32_t w, uint32_t h)
{
	if (!g_drm_vk.gl_bound || idx < 0 || idx >= DRM_OUTPUT_VK_SHARED_IMAGES || w != g_drm_vk.mode_w || h != g_drm_vk.mode_h ||
	    !os_atomic_load_bool(&g_drm_vk.want_frames))
		return false;
	struct drm_output_vk_gl *g = &g_drm_vk.gl;
	struct drm_output_vk_shared *s = &g_drm_vk.shared[idx];
	GLenum layout = GL_LAYOUT_TRANSFER_SRC_EXT;


	g->CopyImageSubData((GLuint)src_gl_name, GL_TEXTURE_2D, 0, 0, 0, 0, s->gl_tex, GL_TEXTURE_2D, 0, 0, 0, 0,
			    (GLsizei)w, (GLsizei)h, 1);
	g->SignalSemaphoreEXT(s->gl_sem, 0, NULL, 1, &s->gl_tex, &layout);
	g->Flush(); /* the signal must be submitted before the present thread waits on it */

	/* INVARIANT: want_frames is a one-way latch within one open (only halt() and a present-loop death clear
	 * it; only open() sets it). A publish that loses the race below leaves a signalled image with no role
	 * -- safe ONLY because nothing can publish again before the next open recreates every semaphore. A
	 * future "restart the present loop" must first recreate (or drain) the shared semaphores. */
	pthread_mutex_lock(&g_drm_vk.lock);
	if (os_atomic_load_bool(&g_drm_vk.want_frames))
		g_drm_vk.ready = idx;
	pthread_mutex_unlock(&g_drm_vk.lock);
	return true;
}

#endif /* defined(__linux__) */
