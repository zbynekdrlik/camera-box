/*
 * camera-box issue 1424 -- the vk-direct core's PRESENTATION CHANGES, split out of
 * obs-drm-output-vk-setup.c (the 1000-line budget): the settle wait an open runs before its acquire, the
 * present thread's re-mode (the Program canvas rate) and the presentation rebuild (a re-mode, an HDMI or
 * sink replug). Same structs, same lock rules as the setup TU (obs-drm-output-vk-internal.h).
 *
 * Two NVIDIA facts the code below exists for (live strih-pp, RTX 3060, 13.10.2026):
 *   - vkAcquireXlibDisplayEXT returns -13 while ANOTHER display of the process brings its presentation up
 *     (its first presents = the mode set on its head): the open waits for every other display to settle,
 *     and a re-mode waits while an open acquires.
 *   - destroying a display-plane surface AFTER its replacement on the same display + plane presents ends
 *     the new presentation (vkQueuePresentKHR -13): a surface rebuild drains the display's queue and
 *     destroys the old presentation FIRST; no surface is ever retired.
 */

#if defined(__linux__)

#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <time.h>

#include "util/base.h"
#include "util/threading.h"
#include "obs-drm-output-vk-internal.h"

/* issue 1424 part 2 (pure, lift-tested by tests/drm_output_vk_two_displays_1424.rs): the presents a
 * display needs on its current surface before its mode set is surely done -- on a FIFO swapchain of N
 * images the (N+1)-th acquire returns only after the first presented image left the screen; never below
 * DRM_OUTPUT_VK_SETTLE_PRESENTS (a 2-image swapchain). */
static unsigned long long drm_output_vk_settle_need(long images)
{
	const unsigned long long need = images > 0 ? (unsigned long long)images + 1ULL : 0ULL;
	return need > DRM_OUTPUT_VK_SETTLE_PRESENTS ? need : DRM_OUTPUT_VK_SETTLE_PRESENTS;
}

/* issue 1424 part 2 (pure, lift-tested): the first OTHER display slot that presents and has not yet
 * presented `need` times on its current surface -- its head is still in the mode set NVIDIA refuses an
 * acquire beside (-13) -- or -1. */
static int drm_output_vk_first_unsettled(const bool *presenting, const unsigned long long *since_surface,
					 const unsigned long long *need, int n, int self)
{
	for (int i = 0; i < n; i++)
		if (i != self && presenting[i] && since_surface[i] < need[i])
			return i;
	return -1;
}

/* issue 1424 part 2 (live strih-pp): OBS opens the two outputs back to back, so the second acquire landed
 * inside the first display's bring-up and failed -13, whichever connector was second. Wait (bounded,
 * g_drm_vk_dev.lock held -- no present thread takes it) until every other presenting display (its frame
 * gate up: a present loop that died on its own clears it) settled. A timeout tries the acquire anyway;
 * its own failure line then names it. */
void drm_output_vk_wait_others_settled(struct drm_output_vk_disp *d)
{
	const int step_ms = 5;
	int waited_ms = 0;
	int other = -1;
	for (;;) {
		bool presenting[DRM_OUTPUT_VK_MAX_DISPLAYS];
		unsigned long long since[DRM_OUTPUT_VK_MAX_DISPLAYS];
		unsigned long long need[DRM_OUTPUT_VK_MAX_DISPLAYS];
		for (int i = 0; i < DRM_OUTPUT_VK_MAX_DISPLAYS; i++) {
			struct drm_output_vk_disp *o = &g_drm_vk_disp[i];
			presenting[i] = o->in_use && o->open && os_atomic_load_bool(&o->want_frames) &&
					!os_atomic_load_bool(&o->rebuilding);
			since[i] = (unsigned long long)os_atomic_load_long(&o->presents_since_surface);
			need[i] = drm_output_vk_settle_need(os_atomic_load_long(&o->settle_images));
		}
		other = drm_output_vk_first_unsettled(presenting, since, need, DRM_OUTPUT_VK_MAX_DISPLAYS, d->slot);
		if (other < 0 || waited_ms >= DRM_OUTPUT_VK_SETTLE_WAIT_MS)
			break;
		const struct timespec pause = {0, (long)step_ms * 1000000L};
		nanosleep(&pause, NULL);
		waited_ms += step_ms;
	}
	if (other >= 0)
		blog(LOG_WARNING,
		     "drm-output: vk-direct: '%s' is still bringing its presentation up after %d ms -- acquiring '%s' "
		     "anyway",
		     g_drm_vk_disp[other].output_name, waited_ms, d->output_name);
	else if (waited_ms > 0)
		blog(LOG_INFO, "drm-output: vk-direct: waited %d ms for the other display to settle before acquiring '%s'",
		     waited_ms, d->output_name);
}

/* camera-box issue 1424 -- present thread, at the top of a vblank: when the graphics thread handed over a
 * new wanted mode (the Program canvas, or none for the Multiview), pick again over the display's modes and
 * rebuild the display-plane surface onto the new mode only when the pick changed and keeps the committed
 * SIZE (the shared images and the OBS intermediate are sized to it; a size change waits for an OBS
 * restart, named). A failed re-mode forgets the wanted mode and rebuilds today's mode once. false =
 * neither came up (the present loop then exits, `present loop exited` names it). */
bool drm_output_vk_remode_if_needed(struct drm_output_vk_disp *d)
{
	if (!os_atomic_set_bool(&d->remode, false))
		return true;
	if (os_atomic_load_bool(&d->dev->acquiring)) {
		/* issue 1424 part 2: another display is being acquired -- a mode set now fails that acquire
		 * (NVIDIA -13). Keep the re-mode pending for a later vblank. */
		os_atomic_set_bool(&d->remode, true);
		return true;
	}
	struct drm_output_vk_mode_list ml;
	if (!drm_output_vk_mode_list_read(d, &ml))
		return true; /* nothing to pick from right now: keep the committed mode */
	const uint32_t want_w = ml.want_w, want_h = ml.want_h, want_mhz = ml.want_mhz;
	bool rebuild = false;
	uint32_t sel_w = 0, sel_h = 0, sel_hz = 0;
	if (ml.best >= 0) {
		sel_w = ml.w[ml.best];
		sel_h = ml.h[ml.best];
		sel_hz = ml.r[ml.best];
		rebuild = sel_hz != d->refresh_mhz && sel_w == d->mode_w && sel_h == d->mode_h;
		if (sel_w != d->mode_w || sel_h != d->mode_h)
			blog(LOG_WARNING,
			     "drm-output: vk-direct: the wanted mode %ux%u@%u.%03uHz needs a size change from %ux%u -- kept "
			     "(the output opens at the native size; a canvas-size open is not supported)",
			     sel_w, sel_h, sel_hz / 1000u, sel_hz % 1000u, d->mode_w, d->mode_h);
	}
	drm_output_vk_mode_list_free(&ml);
	if (!rebuild)
		return true;
	/* issue 1424 part 2: unsettled from here, THEN re-check the acquire flag (both seq-cst atomics): an
	 * open that raised the flag before this reset defers us here, one that raises it after sees the reset
	 * and waits for this presentation to settle -- never a re-mode's mode set inside an acquire. */
	os_atomic_set_long(&d->presents_since_surface, 0);
	if (os_atomic_load_bool(&d->dev->acquiring)) {
		os_atomic_set_bool(&d->remode, true);
		return true;
	}
	if (want_mhz == 0) {
		blog(LOG_INFO, "drm-output: vk-direct: re-mode %ux%u@%u.%03uHz -> @%u.%03uHz (multiview view: native ~60 Hz)",
		     d->mode_w, d->mode_h, d->refresh_mhz / 1000u, d->refresh_mhz % 1000u,
		     sel_hz / 1000u, sel_hz % 1000u);
	} else if (sel_w == want_w && sel_h == want_h && sel_hz == want_mhz) {
		blog(LOG_INFO, "drm-output: vk-direct: re-mode %ux%u@%u.%03uHz -> @%u.%03uHz (program view: exact canvas rate)",
		     d->mode_w, d->mode_h, d->refresh_mhz / 1000u, d->refresh_mhz % 1000u,
		     sel_hz / 1000u, sel_hz % 1000u);
	} else if (sel_w == want_w && sel_h == want_h &&
		   (sel_hz > want_mhz ? sel_hz - want_mhz : want_mhz - sel_hz) <= want_mhz / 500u) {
		/* e.g. 29.970 for a 30.000 canvas: one frame is dropped (or repeated) every 1000 / |diff mHz| s */
		const uint32_t diff = sel_hz > want_mhz ? sel_hz - want_mhz : want_mhz - sel_hz;
		blog(LOG_INFO,
		     "drm-output: vk-direct: re-mode %ux%u@%u.%03uHz -> @%u.%03uHz (program view: near-rate, canvas "
		     "%u.%03u Hz -- one frame %s every %.0f s)",
		     d->mode_w, d->mode_h, d->refresh_mhz / 1000u, d->refresh_mhz % 1000u,
		     sel_hz / 1000u, sel_hz % 1000u, want_mhz / 1000u, want_mhz % 1000u,
		     sel_hz < want_mhz ? "dropped" : "repeated", 1000.0 / (double)diff);
	} else {
		blog(LOG_INFO,
		     "drm-output: vk-direct: re-mode %ux%u@%u.%03uHz -> @%u.%03uHz (program view: no %ux%u mode at the "
		     "canvas rate %u.%03u Hz -- native ~60 Hz)",
		     d->mode_w, d->mode_h, d->refresh_mhz / 1000u, d->refresh_mhz % 1000u,
		     sel_hz / 1000u, sel_hz % 1000u, want_w, want_h, want_mhz / 1000u, want_mhz % 1000u);
	}
	if (drm_output_vk_rebuild_presentation(d, true))
		return true;
	if (d->queue_wedged)
		return false; /* the rebuild named the undrained queue; a native-mode retry cannot run either */
	/* the wanted mode would not come up: forget it and rebuild today's mode once */
	os_atomic_set_long(&d->want_w, 0);
	os_atomic_set_long(&d->want_h, 0);
	os_atomic_set_long(&d->want_mhz, 0);
	blog(LOG_WARNING, "drm-output: vk-direct: the re-mode failed -- back to the native ~60 Hz mode");
	if (drm_output_vk_rebuild_presentation(d, true))
		return true;
	blog(LOG_WARNING, "drm-output: vk-direct: the native mode did not come back after the failed re-mode on '%s'",
	     d->output_name);
	return false;
}

bool drm_output_vk_rebuild_presentation(struct drm_output_vk_disp *d, bool surface_lost)
{
	const uint32_t w = d->mode_w, h = d->mode_h;
	if (d->queue_wedged)
		return false; /* a drain that timed out: the empty submit's fence may still be pending (review) */
	drm_output_vk_mark_disruption(d->dev); /* issue 1424: misses during a rebuild never grow a pick margin */
	/* issue 1424 part 2: a new presentation is unsettled again until it presented (another display's open
	 * waits for it, see drm_output_vk_wait_others_settled) */
	os_atomic_set_long(&d->presents_since_surface, 0);
	VkSwapchainKHR old_for_create = VK_NULL_HANDLE;
	if (surface_lost) {
		/* issue 1424 (live strih-pp, the Program re-mode 60 -> 29.97 Hz): a display-plane surface is NEVER
		 * retired. NVIDIA ends the NEW presentation (vkQueuePresentKHR -13) when the old surface on the same
		 * display + plane is destroyed after the new one presents. So quiesce this display's queue (bounded:
		 * an empty submit's fence, 5 x 1 s), destroy the old swapchain(s) + surface, THEN create the new
		 * surface. A queue that never drains leaves everything in place and gives up by name. */
		if (!drm_output_vk_drain_queue_bounded(d)) {
			/* the empty submit's fence may still be pending: no retry resets or re-submits it, and the
			 * teardown waits it bounded first (submit_outstanding) before it leaks this display */
			d->queue_wedged = true;
			d->submit_outstanding = true;
			blog(LOG_WARNING,
			     "drm-output: vk-direct: the queue of '%s' never drained -- the presentation cannot be rebuilt",
			     d->output_name);
			return false;
		}
		/* the drain signalled the per-vblank fence; the next submit needs it unsignalled */
		d->dev->vk.vkResetFences(d->dev->device, 1, &d->fence);
		d->submit_outstanding = false;
		drm_output_vk_free_retired(d);
		drm_output_vk_destroy_swapchain(d);
		if (d->surface)
			d->dev->vk.vkDestroySurfaceKHR(d->dev->instance, d->surface, NULL);
		d->surface = VK_NULL_HANDLE;
	} else {
		/* A swapchain-only rebuild (out of date, same surface): no device wait (it cannot be bounded and
		 * halt() joins this thread). The replaced swapchain -- whose last present may still be queued,
		 * waiting a present semaphore -- is RETIRED, not destroyed: it is handed to the new swapchain as
		 * oldSwapchain and freed after RETIRE_FRAMES later signalled fences or at the teardown.
		 * Back-to-back rebuilds (a rebuilt swapchain whose very first acquire is out of date again): the
		 * current swapchain never queued a present, so nothing waits on it -- destroy it directly and KEEP
		 * the older retiree, whose present may still be queued. A swapchain that queued a present (even a
		 * rejected one, which still queues its semaphore wait) is retired instead and replaces the older
		 * retiree: its copy's fence signalled after the older retiree's last present was queued. */
		const bool fresh = d->retired_swapchain && !d->present_queued_since_rebuild;
		if (fresh) {
			drm_output_vk_destroy_swapchain(d);
		} else {
			drm_output_vk_free_retired(d);
			d->retired_swapchain = d->swapchain;
			d->swapchain = VK_NULL_HANDLE;
			memset(d->swap_images, 0, sizeof(d->swap_images));
			d->n_swap = 0;
			d->retire_countdown = DRM_OUTPUT_VK_RETIRE_FRAMES;
			old_for_create = d->retired_swapchain; /* a swapchain is retired into at most one */
		}
	}
	d->present_queued_since_rebuild = false;
	if (surface_lost) {
		if (!drm_output_vk_create_surface(d, true))
			return false;
		VkBool32 present = VK_FALSE;
		d->dev->vk.vkGetPhysicalDeviceSurfaceSupportKHR(d->dev->pd, d->dev->qfi, d->surface, &present);
		if (!present) {
			blog(LOG_WARNING, "drm-output: vk-direct: the rebuilt display surface is not presentable from queue family %u",
			     d->dev->qfi);
			return false;
		}
	}
	if (!drm_output_vk_create_swapchain_only(d, old_for_create)) {
		drm_output_vk_destroy_swapchain(d);
		return false;
	}
	drm_output_vk_mark_disruption(d->dev);
	blog(LOG_INFO, "drm-output: vk-direct presentation rebuilt on '%s' (%ux%u)", d->output_name, w, h);
	return true;
}

#endif /* defined(__linux__) */
