/*
 * camera-box issue 1346 — vk-direct SETUP: load Vulkan, acquire the X output away from X, and build
 * the display-plane surface, the device, the FIFO swapchain and the GL-shareable images (plus the
 * matching teardown). The runtime half — the present thread, the lifecycle API and the GL side — is
 * obs-drm-output-vk.c; the shared state is declared in obs-drm-output-vk-internal.h. Linux-only.
 *
 * The pure helper drm_output_vk_pick_mode is lift-compiled + truth-tabled by
 * tests/drm_output_vk_direct_1346.rs.
 */

#if defined(__linux__)

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <X11/Xlib-xcb.h>
#include <xcb/randr.h>
#include <xcb/xcb.h>

#include "util/base.h"
#include "util/threading.h"
#include "obs-drm-output-vk-internal.h"

/* -------------------------------------------------------------------------------------------------
 * Pure decision helper (Tier-0, lift-compiled + truth-tabled by tests/drm_output_vk_direct_1346.rs).
 * ------------------------------------------------------------------------------------------------- */

/* Pick the display mode to present.
 * camera-box issue 1424: for the PROGRAM view the caller passes the canvas as the wanted mode (want_w x
 * want_h at want_mhz): a mode of exactly that size whose refresh is within 0.2 % of it wins, the
 * closest first (30.000 over 29.970 for a 30 Hz canvas, 29.970 accepted when 30 is absent, never 25),
 * so one render tick = one scanout (1080p30 = the HD-SDI 1.5G rate the vestibule SDI chain carries).
 * No such mode, or no wanted mode (0 = the Multiview view), falls back to the issue-1346 rule: among the
 * modes whose visible size equals the display's native (physical) resolution — or among all modes when
 * the native size is unknown (0) or no mode matches it — the one whose refresh (mHz) is closest to
 * 60 Hz; the first wins a tie. -1 when n <= 0. The lease backend takes the connector's preferred mode;
 * Vulkan exposes no "preferred" flag, so the physical resolution stands in for it. */
static int drm_output_vk_pick_mode(const uint32_t *w, const uint32_t *h, const uint32_t *refresh_mhz, int n,
				   uint32_t native_w, uint32_t native_h, uint32_t want_w, uint32_t want_h,
				   uint32_t want_mhz)
{
	if (want_w != 0 && want_h != 0 && want_mhz != 0) {
		const uint32_t tol = want_mhz / 500u;
		int pick = -1;
		uint32_t pick_dist = 0;
		for (int i = 0; i < n; i++) {
			if (w[i] != want_w || h[i] != want_h)
				continue;
			uint32_t dist = refresh_mhz[i] > want_mhz ? refresh_mhz[i] - want_mhz : want_mhz - refresh_mhz[i];
			if (dist > tol)
				continue;
			if (pick < 0 || dist < pick_dist) {
				pick = i;
				pick_dist = dist;
			}
		}
		if (pick >= 0)
			return pick;
	}
	bool any_native = false;
	for (int i = 0; i < n; i++) {
		if (native_w != 0 && native_h != 0 && w[i] == native_w && h[i] == native_h)
			any_native = true;
	}
	int best = -1;
	uint32_t best_dist = 0;
	for (int i = 0; i < n; i++) {
		if (any_native && (w[i] != native_w || h[i] != native_h))
			continue;
		uint32_t dist = refresh_mhz[i] > 60000u ? refresh_mhz[i] - 60000u : 60000u - refresh_mhz[i];
		if (best < 0 || dist < best_dist) {
			best = i;
			best_dist = dist;
		}
	}
	return best;
}

/* Pick the swapchain format: B8G8R8A8_UNORM, else R8G8B8A8_UNORM, else -1 (refuse). Never an sRGB
 * format: the present blit between two UNORM formats copies the bytes; an sRGB swapchain would encode
 * the already-encoded frame a second time (a gamma step the rig harness would show as grey != 128). */
static int drm_output_vk_pick_surface_format(const VkFormat *fmts, uint32_t n)
{
	for (uint32_t i = 0; i < n; i++)
		if (fmts[i] == VK_FORMAT_B8G8R8A8_UNORM)
			return (int)i;
	for (uint32_t i = 0; i < n; i++)
		if (fmts[i] == VK_FORMAT_R8G8B8A8_UNORM)
			return (int)i;
	return -1;
}

static bool drm_output_vk_has_ext(const VkExtensionProperties *props, uint32_t n, const char *name)
{
	for (uint32_t i = 0; i < n; i++)
		if (strcmp(props[i].extensionName, name) == 0)
			return true;
	return false;
}

/* Load libvulkan.so.1 and the global entry points. */
static bool drm_output_vk_load(struct drm_output_vk_disp *d)
{
	d->dev->lib = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
	if (!d->dev->lib) {
		blog(LOG_WARNING, "drm-output: vk-direct: libvulkan.so.1 not loadable (%s) -- output dormant",
		     dlerror());
		return false;
	}
	d->dev->vk.vkGetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)dlsym(d->dev->lib, "vkGetInstanceProcAddr");
	if (!d->dev->vk.vkGetInstanceProcAddr) {
		blog(LOG_WARNING, "drm-output: vk-direct: libvulkan.so.1 has no vkGetInstanceProcAddr -- output dormant");
		return false;
	}
	d->dev->vk.vkCreateInstance = (PFN_vkCreateInstance)d->dev->vk.vkGetInstanceProcAddr(NULL, "vkCreateInstance");
	d->dev->vk.vkEnumerateInstanceExtensionProperties = (PFN_vkEnumerateInstanceExtensionProperties)
		d->dev->vk.vkGetInstanceProcAddr(NULL, "vkEnumerateInstanceExtensionProperties");
	return d->dev->vk.vkCreateInstance && d->dev->vk.vkEnumerateInstanceExtensionProperties;
}

static bool drm_output_vk_create_instance(struct drm_output_vk_disp *d)
{
	const char *want[] = {VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_DISPLAY_EXTENSION_NAME,
			      VK_EXT_DIRECT_MODE_DISPLAY_EXTENSION_NAME, DRM_OUTPUT_VK_EXT_ACQUIRE_XLIB};
	const uint32_t n_want = (uint32_t)(sizeof(want) / sizeof(want[0]));

	uint32_t n = 0;
	d->dev->vk.vkEnumerateInstanceExtensionProperties(NULL, &n, NULL);
	VkExtensionProperties *props = calloc(n ? n : 1, sizeof(*props));
	if (!props)
		return false;
	d->dev->vk.vkEnumerateInstanceExtensionProperties(NULL, &n, props);
	bool ok = true;
	for (uint32_t i = 0; i < n_want; i++) {
		if (!drm_output_vk_has_ext(props, n, want[i])) {
			blog(LOG_WARNING, "drm-output: vk-direct: the Vulkan loader lacks instance extension %s", want[i]);
			ok = false;
		}
	}
	free(props);
	if (!ok)
		return false;

	VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
				 .pApplicationName = "obs-drm-output vk-direct",
				 .apiVersion = VK_API_VERSION_1_1};
	VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
				    .pApplicationInfo = &app,
				    .enabledExtensionCount = n_want,
				    .ppEnabledExtensionNames = want};
	VkResult r = d->dev->vk.vkCreateInstance(&ici, NULL, &d->dev->instance);
	if (r != VK_SUCCESS) {
		blog(LOG_WARNING, "drm-output: vk-direct: vkCreateInstance FAILED VkResult %d", (int)r);
		return false;
	}

	bool all = true;
#define DRM_OUTPUT_VK_LOAD_I(name)                                                                         \
	d->dev->vk.name = (PFN_##name)d->dev->vk.vkGetInstanceProcAddr(d->dev->instance, #name);                    \
	if (!d->dev->vk.name) {                                                                               \
		blog(LOG_WARNING, "drm-output: vk-direct: instance entry point " #name " missing");       \
		all = false;                                                                               \
	}
	DRM_OUTPUT_VK_INSTANCE_FNS(DRM_OUTPUT_VK_LOAD_I)
#undef DRM_OUTPUT_VK_LOAD_I
	d->dev->vk.vkGetRandROutputDisplayEXT = (drm_output_vk_randr_display_fn)d->dev->vk.vkGetInstanceProcAddr(
		d->dev->instance, "vkGetRandROutputDisplayEXT");
	d->dev->vk.vkAcquireXlibDisplayEXT =
		(drm_output_vk_acquire_xlib_fn)d->dev->vk.vkGetInstanceProcAddr(d->dev->instance, "vkAcquireXlibDisplayEXT");
	if (!d->dev->vk.vkGetRandROutputDisplayEXT || !d->dev->vk.vkAcquireXlibDisplayEXT) {
		blog(LOG_WARNING, "drm-output: vk-direct: VK_EXT_acquire_xlib_display entry points missing");
		all = false;
	}
	return all;
}

/* The RandR output XID named `name` on our own X connection; logs the X-layout precondition. */
static unsigned long drm_output_vk_find_output(struct drm_output_vk_disp *d, const char *name)
{
	xcb_connection_t *conn = XGetXCBConnection(d->dev->dpy);
	xcb_window_t root = (xcb_window_t)DefaultRootWindow(d->dev->dpy);
	xcb_randr_get_screen_resources_current_reply_t *res =
		xcb_randr_get_screen_resources_current_reply(conn, xcb_randr_get_screen_resources_current(conn, root),
							     NULL);
	if (!res) {
		blog(LOG_WARNING, "drm-output: vk-direct: RandR get_screen_resources_current failed");
		return 0;
	}
	xcb_randr_output_t *outputs = xcb_randr_get_screen_resources_current_outputs(res);
	int n_out = xcb_randr_get_screen_resources_current_outputs_length(res);
	size_t name_len = strlen(name);
	unsigned long found = 0;
	for (int i = 0; i < n_out && !found; i++) {
		xcb_randr_get_output_info_reply_t *oi = xcb_randr_get_output_info_reply(
			conn, xcb_randr_get_output_info(conn, outputs[i], res->config_timestamp), NULL);
		if (!oi)
			continue;
		const uint8_t *nm = xcb_randr_get_output_info_name(oi);
		int nlen = xcb_randr_get_output_info_name_length(oi);
		if (nlen >= 0 && (size_t)nlen == name_len && memcmp(nm, name, name_len) == 0) {
			found = outputs[i];
			if (oi->crtc != 0)
				blog(LOG_WARNING,
				     "drm-output: vk-direct: output '%s' is still in the X layout (crtc=0x%x) -- the "
				     "NVIDIA driver refuses the acquire until it is off (strih-obs-start.sh runs "
				     "`xrandr --output %s --off` before the OBS launch)",
				     name, (unsigned)oi->crtc, name);
		}
		free(oi);
	}
	free(res);
	if (!found)
		blog(LOG_WARNING, "drm-output: vk-direct: RandR output '%s' not found", name);
	return found;
}

/* issue 1424 part 2: true when ANOTHER open display presents on display plane `plane` (one plane per
 * display). Open path only, under g_drm_vk_dev.lock (a rebuild relies on currentDisplay instead). */
static bool drm_output_vk_plane_taken(const struct drm_output_vk_disp *d, int plane)
{
	for (int i = 0; i < DRM_OUTPUT_VK_MAX_DISPLAYS; i++) {
		const struct drm_output_vk_disp *o = &g_drm_vk_disp[i];
		if (o != d && o->in_use && o->surface != VK_NULL_HANDLE && o->plane == plane)
			return true;
	}
	return false;
}

/* issue 1424 (hotplug): what a teardown does with a display's acquisition. NVIDIA refuses (-13) to
 * re-acquire a display this process released while the shared VkInstance lives on for another display,
 * so while another display is open the acquisition is PARKED on the device (a re-open reuses it) and
 * released only with the device. A leaked (wedged) teardown touches nothing. Pure. */
static int drm_output_vk_park_choice(bool acquired, bool leak, int others_open, bool park_free)
{
	if (!acquired)
		return DRM_OUTPUT_VK_CHOICE_NONE;
	if (leak)
		return DRM_OUTPUT_VK_CHOICE_LEAK;
	if (others_open > 0 && park_free)
		return DRM_OUTPUT_VK_CHOICE_PARK;
	return DRM_OUTPUT_VK_CHOICE_RELEASE;
}

/* A display a teardown parked on this device (dev->lock held): take it back instead of a new acquire. The
 * handle may differ after a replug or another screen on the connector (not guaranteed stable): an entry of
 * the SAME connector with another handle is released by name and freed (a new acquire follows; never a
 * park slot held forever). */
static bool drm_output_vk_unpark(struct drm_output_vk_disp *d)
{
	for (int i = 0; i < DRM_OUTPUT_VK_MAX_DISPLAYS; i++) {
		if (d->dev->parked[i].display == VK_NULL_HANDLE)
			continue;
		if (d->dev->parked[i].display != d->display) {
			if (strcmp(d->dev->parked[i].output_name, d->output_name) == 0) {
				VkResult r = d->dev->vk.vkReleaseDisplayEXT(d->dev->pd, d->dev->parked[i].display);
				/* INFO: a failed retry's captured cause is its first WARNING (the -13 that may follow) */
				blog(LOG_INFO, "drm-output: vk-direct: '%s' has a new handle after a replug -- released the one kept "
				     "since its close (VkResult %d); a new acquire follows",
				     d->output_name, (int)r);
				d->dev->parked[i].display = VK_NULL_HANDLE;
				d->dev->parked[i].output_name[0] = '\0';
			}
			continue;
		}
		d->dev->parked[i].display = VK_NULL_HANDLE;
		d->dev->parked[i].output_name[0] = '\0';
		d->acquired = true;
		blog(LOG_INFO,
		     "drm-output: vk-direct display re-used output='%s' (kept acquired since its last close -- no "
		     "new acquire)",
		     d->output_name);
		return true;
	}
	return false;
}

/* Acquire the output's display away from X; a later display must be on the first one's GPU (one VkDevice). */
static bool drm_output_vk_acquire(struct drm_output_vk_disp *d, unsigned long output)
{
	uint32_t n = 0;
	if (d->dev->vk.vkEnumeratePhysicalDevices(d->dev->instance, &n, NULL) != VK_SUCCESS || n == 0) {
		blog(LOG_WARNING, "drm-output: vk-direct: no Vulkan physical device");
		return false;
	}
	VkPhysicalDevice pds[8];
	if (n > 8)
		n = 8;
	d->dev->vk.vkEnumeratePhysicalDevices(d->dev->instance, &n, pds);
	VkPhysicalDevice pd = VK_NULL_HANDLE;
	for (uint32_t i = 0; i < n && !pd; i++) {
		VkDisplayKHR disp = VK_NULL_HANDLE;
		if (d->dev->vk.vkGetRandROutputDisplayEXT(pds[i], d->dev->dpy, output, &disp) == VK_SUCCESS &&
		    disp != VK_NULL_HANDLE) {
			pd = pds[i];
			d->display = disp;
		}
	}
	if (!pd) {
		blog(LOG_WARNING, "drm-output: vk-direct: no Vulkan device drives output '%s'", d->output_name);
		return false;
	}
	if (d->dev->pd && pd != d->dev->pd) {
		blog(LOG_WARNING,
		     "drm-output: vk-direct: output '%s' is driven by another GPU than the open output(s) -- two "
		     "outputs on two GPUs are not supported (one shared VkDevice)",
		     d->output_name);
		d->display = VK_NULL_HANDLE;
		return false;
	}
	d->dev->pd = pd;
	const bool reused = drm_output_vk_unpark(d);
	VkPhysicalDeviceProperties pr;
	d->dev->vk.vkGetPhysicalDeviceProperties(pd, &pr);
	VkResult r = reused ? VK_SUCCESS : d->dev->vk.vkAcquireXlibDisplayEXT(pd, d->dev->dpy, d->display);
	if (r != VK_SUCCESS) {
		blog(LOG_WARNING,
		     "drm-output: vk-direct: vkAcquireXlibDisplayEXT('%s') FAILED VkResult %d on %s (-13 = NVIDIA "
		     "refused: the output is still in the X layout, another display of this process is mid mode set, "
		     "or this process released it earlier while another display keeps the Vulkan instance)",
		     d->output_name, (int)r, pr.deviceName);
		return false;
	}
	d->acquired = true;
	drm_output_vk_mark_disruption(d->dev);
	if (!reused)
		blog(LOG_INFO, "drm-output: vk-direct display acquired output='%s' gpu='%s' (off the X desktop)",
		     d->output_name, pr.deviceName);
	return true;
}

/* Free a mode list (struct drm_output_vk_mode_list, obs-drm-output-vk-internal.h). */
void drm_output_vk_mode_list_free(struct drm_output_vk_mode_list *ml)
{
	free(ml->modes);
	free(ml->w);
	free(ml->h);
	free(ml->r);
	memset(ml, 0, sizeof(*ml));
}

/* false = no modes / no memory (named); true = ml filled, ml->best = the pick (-1 = none). */
bool drm_output_vk_mode_list_read(struct drm_output_vk_disp *d, struct drm_output_vk_mode_list *ml)
{
	memset(ml, 0, sizeof(*ml));
	ml->best = -1;
	ml->want_w = (uint32_t)os_atomic_load_long(&d->want_w);
	ml->want_h = (uint32_t)os_atomic_load_long(&d->want_h);
	ml->want_mhz = (uint32_t)os_atomic_load_long(&d->want_mhz);
	uint32_t nd = 0;
	d->dev->vk.vkGetPhysicalDeviceDisplayPropertiesKHR(d->dev->pd, &nd, NULL);
	VkDisplayPropertiesKHR *dprops = calloc(nd ? nd : 1, sizeof(*dprops));
	if (!dprops)
		return false;
	d->dev->vk.vkGetPhysicalDeviceDisplayPropertiesKHR(d->dev->pd, &nd, dprops);
	for (uint32_t i = 0; i < nd; i++)
		if (dprops[i].display == d->display)
			ml->native = dprops[i].physicalResolution;
	free(dprops);

	uint32_t nm = 0;
	if (d->dev->vk.vkGetDisplayModePropertiesKHR(d->dev->pd, d->display, &nm, NULL) != VK_SUCCESS || nm == 0) {
		blog(LOG_WARNING, "drm-output: vk-direct: the display reports no modes");
		return false;
	}
	ml->modes = calloc(nm, sizeof(*ml->modes));
	ml->w = calloc(nm, sizeof(uint32_t));
	ml->h = calloc(nm, sizeof(uint32_t));
	ml->r = calloc(nm, sizeof(uint32_t));
	if (!ml->modes || !ml->w || !ml->h || !ml->r ||
	    d->dev->vk.vkGetDisplayModePropertiesKHR(d->dev->pd, d->display, &nm, ml->modes) != VK_SUCCESS) {
		drm_output_vk_mode_list_free(ml);
		return false;
	}
	ml->n = nm;
	for (uint32_t i = 0; i < nm; i++) {
		ml->w[i] = ml->modes[i].parameters.visibleRegion.width;
		ml->h[i] = ml->modes[i].parameters.visibleRegion.height;
		ml->r[i] = ml->modes[i].parameters.refreshRate;
	}
	ml->best = drm_output_vk_pick_mode(ml->w, ml->h, ml->r, (int)nm, ml->native.width, ml->native.height,
					   ml->want_w, ml->want_h, ml->want_mhz);
	return true;
}

/* The native ~60 Hz mode, a display plane that can show it, and the display-plane surface. The chosen
 * mode is committed to d->mode_* only when the surface exists. `rebuild` (the present thread's
 * rebuild after a lost surface) requires the mode to have the SAME size as the committed one -- the
 * shared images and the OBS intermediate are sized to it, and the graphics thread reads mode_* without
 * a lock -- so a rebuild never changes mode_* at all; a different size refuses by name. */
bool drm_output_vk_create_surface(struct drm_output_vk_disp *d, bool rebuild)
{
	struct drm_output_vk_mode_list ml;
	if (!drm_output_vk_mode_list_read(d, &ml))
		return false;
	const VkExtent2D native = ml.native;
	const uint32_t nm = ml.n;
	bool ok = false;
	{
		const int best = ml.best;
		if (best >= 0) {
			VkDisplayModePropertiesKHR mode = ml.modes[best];
			const uint32_t sel_w = ml.w[best], sel_h = ml.h[best], sel_hz = ml.r[best];
			if (rebuild && (sel_w != d->mode_w || sel_h != d->mode_h)) {
				blog(LOG_WARNING,
				     "drm-output: vk-direct: the display now offers %ux%u instead of %ux%u -- the shared images "
				     "no longer fit (restart OBS)",
				     sel_w, sel_h, d->mode_w, d->mode_h);
				drm_output_vk_mode_list_free(&ml);
				return false;
			}

			uint32_t npl = 0;
			d->dev->vk.vkGetPhysicalDeviceDisplayPlanePropertiesKHR(d->dev->pd, &npl, NULL);
			VkDisplayPlanePropertiesKHR *plp = calloc(npl ? npl : 1, sizeof(*plp));
			int plane = -1;
			if (plp) {
				d->dev->vk.vkGetPhysicalDeviceDisplayPlanePropertiesKHR(d->dev->pd, &npl, plp);
				for (uint32_t p = 0; p < npl && plane < 0; p++) {
					if (plp[p].currentDisplay != VK_NULL_HANDLE && plp[p].currentDisplay != d->display)
						continue;
					if (!rebuild && drm_output_vk_plane_taken(d, (int)p))
						continue; /* issue 1424 part 2: the other display presents on it */
					uint32_t ns = 0;
					d->dev->vk.vkGetDisplayPlaneSupportedDisplaysKHR(d->dev->pd, p, &ns, NULL);
					VkDisplayKHR sup[16];
					if (ns > 16)
						ns = 16;
					d->dev->vk.vkGetDisplayPlaneSupportedDisplaysKHR(d->dev->pd, p, &ns, sup);
					for (uint32_t j = 0; j < ns; j++)
						if (sup[j] == d->display)
							plane = (int)p;
				}
			}
			if (plane < 0) {
				blog(LOG_WARNING, "drm-output: vk-direct: no display plane can show output '%s'",
				     d->output_name);
			} else {
				VkDisplaySurfaceCreateInfoKHR dsci = {
					.sType = VK_STRUCTURE_TYPE_DISPLAY_SURFACE_CREATE_INFO_KHR,
					.displayMode = mode.displayMode,
					.planeIndex = (uint32_t)plane,
					.planeStackIndex = plp[plane].currentStackIndex,
					.transform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
					.globalAlpha = 1.0f,
					.alphaMode = VK_DISPLAY_PLANE_ALPHA_OPAQUE_BIT_KHR,
					.imageExtent = mode.parameters.visibleRegion};
				VkResult r = d->dev->vk.vkCreateDisplayPlaneSurfaceKHR(d->dev->instance, &dsci, NULL, &d->surface);
				if (r == VK_SUCCESS) {
					ok = true;
					d->plane = plane;
					d->mode_w = sel_w;
					d->mode_h = sel_h;
					d->refresh_mhz = sel_hz;
					/* issue 1424 part 2: a committed surface is a new mode set -- unsettled again */
					os_atomic_set_long(&d->presents_since_surface, 0);
					blog(LOG_INFO,
					     "drm-output: vk-direct mode %ux%u@%u.%03uHz on display plane %d (native %ux%u, "
					     "%u modes)",
					     sel_w, sel_h, sel_hz / 1000u, sel_hz % 1000u, plane, native.width, native.height, nm);
				} else {
					blog(LOG_WARNING, "drm-output: vk-direct: vkCreateDisplayPlaneSurfaceKHR FAILED VkResult %d",
					     (int)r);
				}
			}
			free(plp);
		}
	}
	drm_output_vk_mode_list_free(&ml);
	return ok;
}

static bool drm_output_vk_create_device(struct drm_output_vk_disp *d)
{
	uint32_t nq = 0;
	d->dev->vk.vkGetPhysicalDeviceQueueFamilyProperties(d->dev->pd, &nq, NULL);
	VkQueueFamilyProperties qf[16];
	if (nq > 16)
		nq = 16;
	d->dev->vk.vkGetPhysicalDeviceQueueFamilyProperties(d->dev->pd, &nq, qf);
	int qfi = -1;
	for (uint32_t i = 0; i < nq && qfi < 0; i++) {
		VkBool32 present = VK_FALSE;
		d->dev->vk.vkGetPhysicalDeviceSurfaceSupportKHR(d->dev->pd, i, d->surface, &present);
		if (present && (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT))
			qfi = (int)i;
	}
	if (qfi < 0) {
		blog(LOG_WARNING, "drm-output: vk-direct: no graphics queue can present to the display surface");
		return false;
	}
	d->dev->qfi = (uint32_t)qfi;

	const char *want[] = {VK_KHR_SWAPCHAIN_EXTENSION_NAME, VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
			      VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME};
	const uint32_t n_want = (uint32_t)(sizeof(want) / sizeof(want[0]));
	uint32_t n = 0;
	d->dev->vk.vkEnumerateDeviceExtensionProperties(d->dev->pd, NULL, &n, NULL);
	VkExtensionProperties *props = calloc(n ? n : 1, sizeof(*props));
	if (!props)
		return false;
	d->dev->vk.vkEnumerateDeviceExtensionProperties(d->dev->pd, NULL, &n, props);
	bool ok = true;
	for (uint32_t i = 0; i < n_want; i++) {
		if (!drm_output_vk_has_ext(props, n, want[i])) {
			blog(LOG_WARNING, "drm-output: vk-direct: the GPU lacks device extension %s", want[i]);
			ok = false;
		}
	}
	free(props);
	if (!ok)
		return false;

	/* issue 1424 part 2: one queue per display slot, else queue 0 shared under its lock. */
	d->dev->n_queues = qf[qfi].queueCount < DRM_OUTPUT_VK_MAX_DISPLAYS ? qf[qfi].queueCount : DRM_OUTPUT_VK_MAX_DISPLAYS;
	if (d->dev->n_queues < 1)
		d->dev->n_queues = 1;
	const float prio[DRM_OUTPUT_VK_MAX_DISPLAYS] = {1.0f, 1.0f};
	VkDeviceQueueCreateInfo dqci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
					.queueFamilyIndex = d->dev->qfi,
					.queueCount = d->dev->n_queues,
					.pQueuePriorities = prio};
	VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
				  .queueCreateInfoCount = 1,
				  .pQueueCreateInfos = &dqci,
				  .enabledExtensionCount = n_want,
				  .ppEnabledExtensionNames = want};
	VkResult r = d->dev->vk.vkCreateDevice(d->dev->pd, &dci, NULL, &d->dev->device);
	if (r != VK_SUCCESS) {
		blog(LOG_WARNING, "drm-output: vk-direct: vkCreateDevice FAILED VkResult %d", (int)r);
		return false;
	}
	bool all = true;
#define DRM_OUTPUT_VK_LOAD_D(name)                                                                  \
	d->dev->vk.name = (PFN_##name)d->dev->vk.vkGetDeviceProcAddr(d->dev->device, #name);                 \
	if (!d->dev->vk.name) {                                                                        \
		blog(LOG_WARNING, "drm-output: vk-direct: device entry point " #name " missing");  \
		all = false;                                                                        \
	}
	DRM_OUTPUT_VK_DEVICE_FNS(DRM_OUTPUT_VK_LOAD_D)
#undef DRM_OUTPUT_VK_LOAD_D
	if (!all)
		return false;
	for (uint32_t q = 0; q < d->dev->n_queues; q++)
		d->dev->vk.vkGetDeviceQueue(d->dev->device, d->dev->qfi, q, &d->dev->queues[q]);
	blog(LOG_INFO, "drm-output: vk-direct device created (queue family %u, %u queue%s for %d display slots)",
	     d->dev->qfi, d->dev->n_queues, d->dev->n_queues == 1 ? "" : "s", DRM_OUTPUT_VK_MAX_DISPLAYS);
	return true;
}

/* The swapchain (+ its images and per-image present semaphores) at the current mode. Called at open
 * and by the rebuild. */
bool drm_output_vk_create_swapchain_only(struct drm_output_vk_disp *d, VkSwapchainKHR old_swapchain)
{
	VkSurfaceCapabilitiesKHR caps;
	if (d->dev->vk.vkGetPhysicalDeviceSurfaceCapabilitiesKHR(d->dev->pd, d->surface, &caps) != VK_SUCCESS)
		return false;
	if (!(caps.supportedUsageFlags & VK_IMAGE_USAGE_TRANSFER_DST_BIT)) {
		blog(LOG_WARNING, "drm-output: vk-direct: the display surface cannot be a transfer destination");
		return false;
	}
	uint32_t nf = 0;
	d->dev->vk.vkGetPhysicalDeviceSurfaceFormatsKHR(d->dev->pd, d->surface, &nf, NULL);
	VkSurfaceFormatKHR fmts[32];
	VkFormat plain[32];
	if (nf > 32)
		nf = 32;
	if (nf == 0 || d->dev->vk.vkGetPhysicalDeviceSurfaceFormatsKHR(d->dev->pd, d->surface, &nf, fmts) != VK_SUCCESS)
		return false;
	for (uint32_t i = 0; i < nf; i++)
		plain[i] = fmts[i].format;
	int pick = drm_output_vk_pick_surface_format(plain, nf);
	if (pick < 0) {
		blog(LOG_WARNING,
		     "drm-output: vk-direct: the display offers no B8G8R8A8/R8G8B8A8 UNORM format (%u formats) -- refusing "
		     "(an sRGB swapchain would add a gamma step)",
		     nf);
		return false;
	}
	VkSurfaceFormatKHR fmt = fmts[pick];
	VkFormatProperties dstp, srcp;
	d->dev->vk.vkGetPhysicalDeviceFormatProperties(d->dev->pd, fmt.format, &dstp);
	d->dev->vk.vkGetPhysicalDeviceFormatProperties(d->dev->pd, VK_FORMAT_R8G8B8A8_UNORM, &srcp);
	if (!(dstp.optimalTilingFeatures & VK_FORMAT_FEATURE_BLIT_DST_BIT) ||
	    !(srcp.optimalTilingFeatures & VK_FORMAT_FEATURE_BLIT_SRC_BIT)) {
		blog(LOG_WARNING, "drm-output: vk-direct: the GPU cannot blit RGBA8 into swapchain format %d", (int)fmt.format);
		return false;
	}

	uint32_t min_images = caps.minImageCount < 2 ? 2 : caps.minImageCount;
	if (caps.maxImageCount && min_images > caps.maxImageCount)
		min_images = caps.maxImageCount;
	VkSwapchainCreateInfoKHR sci = {.sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
					.surface = d->surface,
					.minImageCount = min_images,
					.imageFormat = fmt.format,
					.imageColorSpace = fmt.colorSpace,
					.imageExtent = {d->mode_w, d->mode_h},
					.imageArrayLayers = 1,
					.imageUsage = VK_IMAGE_USAGE_TRANSFER_DST_BIT,
					.imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
					.preTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
					.compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
					/* FIFO = vblank-locked, never tearing (the only mode the spec guarantees). */
					.presentMode = VK_PRESENT_MODE_FIFO_KHR,
					.clipped = VK_TRUE,
					.oldSwapchain = old_swapchain};
	VkResult r = d->dev->vk.vkCreateSwapchainKHR(d->dev->device, &sci, NULL, &d->swapchain);
	if (r != VK_SUCCESS) {
		d->swapchain = VK_NULL_HANDLE;
		blog(LOG_WARNING, "drm-output: vk-direct: vkCreateSwapchainKHR FAILED VkResult %d", (int)r);
		return false;
	}
	uint32_t n = 0;
	d->dev->vk.vkGetSwapchainImagesKHR(d->dev->device, d->swapchain, &n, NULL);
	if (n == 0 || n > DRM_OUTPUT_VK_MAX_SWAP) {
		blog(LOG_WARNING, "drm-output: vk-direct: unexpected swapchain image count %u", n);
		return false;
	}
	d->dev->vk.vkGetSwapchainImagesKHR(d->dev->device, d->swapchain, &n, d->swap_images);
	d->n_swap = n;
	os_atomic_set_long(&d->settle_images, (long)n); /* issue 1424 part 2: the settle threshold */

	/* The per-image present semaphores live across rebuilds (a retired swapchain's queued present may
	 * still wait one; re-signalling a semaphore whose earlier wait is already submitted is valid): only
	 * the missing slots are created, all of them go in the teardown. */
	VkSemaphoreCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
	for (uint32_t i = 0; i < n; i++)
		if (!d->sem_done[i] &&
		    d->dev->vk.vkCreateSemaphore(d->dev->device, &smci, NULL, &d->sem_done[i]) != VK_SUCCESS) {
			d->sem_done[i] = VK_NULL_HANDLE;
			return false;
		}
	blog(LOG_INFO, "drm-output: vk-direct swapchain %ux%u FIFO images=%u format=%d", d->mode_w,
	     d->mode_h, n, (int)fmt.format);
	/* issue 1424 (design 6164790116): the modes this surface offers, for a later MAILBOX decision */
	VkPresentModeKHR pms[8];
	uint32_t npm = 8;
	char pm_list[64] = "";
	if (d->dev->vk.vkGetPhysicalDeviceSurfacePresentModesKHR(d->dev->pd, d->surface, &npm, pms) >= 0) {
		for (uint32_t i = 0; i < npm && i < 8u; i++) {
			const size_t used = strlen(pm_list);
			snprintf(pm_list + used, sizeof(pm_list) - used, "%s%d", i ? "," : "", (int)pms[i]);
		}
	}
	blog(LOG_INFO, "drm-output: vk-direct present modes offered on '%s': %s (0 immediate, 1 mailbox, 2 fifo, 3 fifo-relaxed)",
	     d->output_name, pm_list[0] ? pm_list : "unknown");
	return true;
}

/* The swapchain + the per-vblank sync objects and command buffer (open only). */
static bool drm_output_vk_create_swapchain(struct drm_output_vk_disp *d)
{
	if (!drm_output_vk_create_swapchain_only(d, VK_NULL_HANDLE))
		return false;
	VkSemaphoreCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
	if (d->dev->vk.vkCreateSemaphore(d->dev->device, &smci, NULL, &d->sem_acquire) != VK_SUCCESS)
		return false;
	VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
	if (d->dev->vk.vkCreateFence(d->dev->device, &fci, NULL, &d->fence) != VK_SUCCESS)
		return false;
	VkCommandPoolCreateInfo cpci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
					.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
					.queueFamilyIndex = d->dev->qfi};
	if (d->dev->vk.vkCreateCommandPool(d->dev->device, &cpci, NULL, &d->pool) != VK_SUCCESS)
		return false;
	VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
					    .commandPool = d->pool,
					    .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
					    .commandBufferCount = 1};
	return d->dev->vk.vkAllocateCommandBuffers(d->dev->device, &cbai, &d->cmd) == VK_SUCCESS;
}

/* Drop the current swapchain (the device must be idle -- the teardown). The present semaphores stay
 * (destroy_all frees them). */
void drm_output_vk_destroy_swapchain(struct drm_output_vk_disp *d)
{
	if (d->swapchain)
		d->dev->vk.vkDestroySwapchainKHR(d->dev->device, d->swapchain, NULL);
	d->swapchain = VK_NULL_HANDLE;
	memset(d->swap_images, 0, sizeof(d->swap_images));
	d->n_swap = 0;
}

void drm_output_vk_free_retired(struct drm_output_vk_disp *d)
{
	if (d->retired_swapchain)
		d->dev->vk.vkDestroySwapchainKHR(d->dev->device, d->retired_swapchain, NULL);
	d->retired_swapchain = VK_NULL_HANDLE;
	d->retire_countdown = 0;
}

/* The shared images: device-local, optimal tiling, exportable as OPAQUE_FD with a DEDICATED allocation
 * (the GL side declares GL_DEDICATED_MEMORY_OBJECT_EXT to match), + an exportable binary semaphore each.
 * RGBA8 UNORM = GL_RGBA8 storage byte-for-byte; the present blit converts to the swapchain's BGRA. */
static bool drm_output_vk_create_shared(struct drm_output_vk_disp *d)
{
	VkPhysicalDeviceMemoryProperties mp;
	d->dev->vk.vkGetPhysicalDeviceMemoryProperties(d->dev->pd, &mp);
	for (int i = 0; i < DRM_OUTPUT_VK_SHARED_IMAGES; i++) {
		struct drm_output_vk_shared *s = &d->shared[i];
		VkExternalMemoryImageCreateInfo emi = {.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
						       .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT};
		VkImageCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
					 .pNext = &emi,
					 .imageType = VK_IMAGE_TYPE_2D,
					 .format = VK_FORMAT_R8G8B8A8_UNORM,
					 .extent = {d->mode_w, d->mode_h, 1},
					 .mipLevels = 1,
					 .arrayLayers = 1,
					 .samples = VK_SAMPLE_COUNT_1_BIT,
					 .tiling = VK_IMAGE_TILING_OPTIMAL,
					 .usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
						  VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
					 .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
					 .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED};
		if (d->dev->vk.vkCreateImage(d->dev->device, &ici, NULL, &s->image) != VK_SUCCESS) {
			blog(LOG_WARNING, "drm-output: vk-direct: shared image %d create failed", i);
			return false;
		}
		VkMemoryRequirements req;
		d->dev->vk.vkGetImageMemoryRequirements(d->dev->device, s->image, &req);
		uint32_t type = UINT32_MAX;
		for (uint32_t t = 0; t < mp.memoryTypeCount && type == UINT32_MAX; t++)
			if ((req.memoryTypeBits & (1u << t)) &&
			    (mp.memoryTypes[t].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
				type = t;
		if (type == UINT32_MAX) {
			blog(LOG_WARNING, "drm-output: vk-direct: no device-local memory type for shared image %d", i);
			return false;
		}
		VkMemoryDedicatedAllocateInfo ded = {.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
						     .image = s->image};
		VkExportMemoryAllocateInfo exp = {.sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
						  .pNext = &ded,
						  .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT};
		VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
					    .pNext = &exp,
					    .allocationSize = req.size,
					    .memoryTypeIndex = type};
		if (d->dev->vk.vkAllocateMemory(d->dev->device, &mai, NULL, &s->memory) != VK_SUCCESS ||
		    d->dev->vk.vkBindImageMemory(d->dev->device, s->image, s->memory, 0) != VK_SUCCESS) {
			blog(LOG_WARNING, "drm-output: vk-direct: shared image %d memory allocation failed", i);
			return false;
		}
		s->size = req.size;
		VkExportSemaphoreCreateInfo esi = {.sType = VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO,
						   .handleTypes = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT};
		VkSemaphoreCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO, .pNext = &esi};
		if (d->dev->vk.vkCreateSemaphore(d->dev->device, &smci, NULL, &s->sem) != VK_SUCCESS) {
			blog(LOG_WARNING, "drm-output: vk-direct: shared semaphore %d create failed", i);
			return false;
		}
	}
	return true;
}

bool drm_output_vk_setup(struct drm_output_vk_disp *d)
{
	struct drm_output_vk_device *dev = d->dev;
	d->plane = -1;
	/* issue 1424 part 2: the X connection, loader + VkInstance come with the first display, the VkDevice
	 * with the first display's surface. */
	if (!dev->dpy) {
		dev->dpy = XOpenDisplay(NULL);
		if (!dev->dpy) {
			blog(LOG_WARNING, "drm-output: vk-direct: cannot open the X display (DISPLAY) -- output dormant");
			return false;
		}
	}
	if (!dev->instance && !(drm_output_vk_load(d) && drm_output_vk_create_instance(d)))
		return false;
	const unsigned long output = drm_output_vk_find_output(d, d->output_name);
	if (output == 0)
		return false;
	/* issue 1424 part 2: never acquire while another display brings its presentation up (NVIDIA -13), and
	 * hold the other displays' re-modes (a new mode set) off until this acquire returned. */
	os_atomic_set_bool(&dev->acquiring, true);
	drm_output_vk_mark_disruption(d->dev); /* issue 1424: the other displays' misses from here are this bring-up */
	drm_output_vk_wait_others_settled(d);
	const bool acquired = drm_output_vk_acquire(d, output);
	os_atomic_set_bool(&dev->acquiring, false);
	if (!acquired || !drm_output_vk_create_surface(d, false))
		return false;
	if (!dev->device) {
		if (!drm_output_vk_create_device(d))
			return false;
	} else {
		VkBool32 present = VK_FALSE;
		dev->vk.vkGetPhysicalDeviceSurfaceSupportKHR(dev->pd, dev->qfi, d->surface, &present);
		if (!present) {
			blog(LOG_WARNING,
			     "drm-output: vk-direct: output '%s' cannot be presented from the shared device's queue family %u",
			     d->output_name, dev->qfi);
			return false;
		}
	}
	const uint32_t q = (uint32_t)d->slot < dev->n_queues ? (uint32_t)d->slot : 0u;
	d->queue = dev->queues[q];
	d->queue_lock = &dev->queue_lock[q];
	if (!drm_output_vk_create_swapchain(d) || !drm_output_vk_create_shared(d))
		return false;
	dev->refs++;
	return true;
}

/* The shared part goes with the last display (dev->lock held). A device a wedged display leaked is
 * forgotten, never destroyed (the driver may still use it); a later open builds a fresh one. */
static void drm_output_vk_destroy_device(struct drm_output_vk_device *dev)
{
	if (dev->wedged) {
		blog(LOG_WARNING,
		     "drm-output: vk-direct: leaking the shared Vulkan device, instance, loader and X connection (a "
		     "display's last copy never completed)");
	} else {
		if (dev->device) {
			dev->vk.vkDeviceWaitIdle(dev->device);
			dev->vk.vkDestroyDevice(dev->device, NULL);
		}
		/* issue 1424 (hotplug): the displays kept acquired for a re-open go with the instance */
		for (int i = 0; i < DRM_OUTPUT_VK_MAX_DISPLAYS; i++) {
			if (dev->parked[i].display == VK_NULL_HANDLE || !dev->pd)
				continue;
			VkResult r = dev->vk.vkReleaseDisplayEXT(dev->pd, dev->parked[i].display);
			blog(LOG_INFO, "drm-output: vk-direct display released (VkResult %d) -- '%s' (kept since its close)",
			     (int)r, dev->parked[i].output_name);
		}
		if (dev->instance)
			dev->vk.vkDestroyInstance(dev->instance, NULL);
		if (dev->dpy)
			XCloseDisplay(dev->dpy);
		if (dev->lib)
			dlclose(dev->lib);
	}
	dev->wedged = false;
	memset(dev->parked, 0, sizeof(dev->parked)); /* released above, or leaked with a wedged device */
	dev->lib = NULL;
	memset(&dev->vk, 0, sizeof(dev->vk));
	memset(&dev->gl, 0, sizeof(dev->gl));
	dev->gl_resolved = false;
	dev->dpy = NULL;
	dev->instance = VK_NULL_HANDLE;
	dev->pd = VK_NULL_HANDLE;
	dev->device = VK_NULL_HANDLE;
	dev->qfi = 0;
	dev->n_queues = 0;
	memset(dev->queues, 0, sizeof(dev->queues));
}

/* Free what setup built for display `d` (idempotent; present thread halted, dev->lock held). */
void drm_output_vk_destroy_display(struct drm_output_vk_disp *d)
{
	struct drm_output_vk_device *dev = d->dev;
	bool leak = false;
	if (dev->device && d->submit_outstanding && d->fence) {
		/* A fence never seen signalled: wait BOUNDED (5 x 1 s); a GPU that never finishes the copy leaks
		 * this display (and, at the last close, the device) instead of hanging the OBS shutdown. */
		VkResult fr = VK_TIMEOUT;
		for (int i = 0; i < 5 && fr == VK_TIMEOUT; i++)
			fr = dev->vk.vkWaitForFences(dev->device, 1, &d->fence, VK_TRUE, DRM_OUTPUT_VK_WAIT_NS);
		if (fr == VK_TIMEOUT) { /* a lost device returns at once: destroy it normally below */
			blog(LOG_WARNING,
			     "drm-output: vk-direct: the last copy never completed (VkResult %d) -- leaking the Vulkan objects "
			     "of '%s' instead of hanging the shutdown",
			     (int)fr, d->output_name);
			leak = true;
			dev->wedged = true;
		}
		d->submit_outstanding = false;
	}
	if (dev->device && !leak && !drm_output_vk_drain_queue_bounded(d)) {
		blog(LOG_WARNING, "drm-output: vk-direct: the queue of '%s' never drained -- leaking its Vulkan objects",
		     d->output_name);
		leak = true;
		dev->wedged = true;
	}
	if (dev->device && !leak) {
		for (int i = 0; i < DRM_OUTPUT_VK_SHARED_IMAGES; i++) {
			struct drm_output_vk_shared *s = &d->shared[i];
			if (s->sem)
				dev->vk.vkDestroySemaphore(dev->device, s->sem, NULL);
			if (s->image)
				dev->vk.vkDestroyImage(dev->device, s->image, NULL);
			if (s->memory)
				dev->vk.vkFreeMemory(dev->device, s->memory, NULL);
		}
		if (d->pool)
			dev->vk.vkDestroyCommandPool(dev->device, d->pool, NULL);
		if (d->fence)
			dev->vk.vkDestroyFence(dev->device, d->fence, NULL);
		if (d->sem_acquire)
			dev->vk.vkDestroySemaphore(dev->device, d->sem_acquire, NULL);
		drm_output_vk_destroy_swapchain(d);
		drm_output_vk_free_retired(d);
		for (uint32_t i = 0; i < DRM_OUTPUT_VK_MAX_SWAP; i++) {
			if (d->sem_done[i])
				dev->vk.vkDestroySemaphore(dev->device, d->sem_done[i], NULL);
		}
	}
	if (!leak && d->surface && dev->instance)
		dev->vk.vkDestroySurfaceKHR(dev->instance, d->surface, NULL);
	/* issue 1424 (hotplug): beside another open display a release could never be undone by this process
	 * (NVIDIA -13 on the re-acquire), so the acquisition is parked for a re-open of the same output */
	int park_slot = -1;
	for (int i = 0; i < DRM_OUTPUT_VK_MAX_DISPLAYS && park_slot < 0; i++) {
		if (dev->parked[i].display == VK_NULL_HANDLE)
			park_slot = i;
	}
	const int choice = drm_output_vk_park_choice(d->acquired, leak, dev->refs - (d->open ? 1 : 0), park_slot >= 0);
	if (choice == DRM_OUTPUT_VK_CHOICE_PARK) {
		dev->parked[park_slot].display = d->display;
		snprintf(dev->parked[park_slot].output_name, sizeof(dev->parked[park_slot].output_name), "%s",
			 d->output_name);
		blog(LOG_INFO,
		     "drm-output: vk-direct display '%s' kept acquired (another output is open; a re-open reuses it, "
		     "the last close releases it)",
		     d->output_name);
	} else if (choice == DRM_OUTPUT_VK_CHOICE_RELEASE) {
		VkResult r = dev->vk.vkReleaseDisplayEXT(dev->pd, d->display);
		blog(LOG_INFO,
		     "drm-output: vk-direct display released (VkResult %d) -- '%s' stays OFF in X, never back on the "
		     "desktop by itself",
		     (int)r, d->output_name);
	}

	memset(d->shared, 0, sizeof(d->shared));
	d->display = VK_NULL_HANDLE;
	d->acquired = false;
	d->surface = VK_NULL_HANDLE;
	d->queue = VK_NULL_HANDLE;
	d->queue_lock = NULL;
	d->swapchain = VK_NULL_HANDLE;
	memset(d->swap_images, 0, sizeof(d->swap_images));
	memset(d->sem_done, 0, sizeof(d->sem_done));
	d->n_swap = 0;
	d->sem_acquire = VK_NULL_HANDLE;
	d->fence = VK_NULL_HANDLE;
	d->pool = VK_NULL_HANDLE;
	d->cmd = VK_NULL_HANDLE;
	d->retired_swapchain = VK_NULL_HANDLE; /* leaked with a wedged device, or already freed */
	d->retire_countdown = 0;
	d->present_queued_since_rebuild = false;
	d->queue_wedged = false;
	d->plane = -1;
	d->front = d->pending = d->ready = -1;
	if (d->open)
		dev->refs--; /* setup counted this display only once it fully succeeded */
	d->open = false;
	d->in_use = false;
	if (dev->refs == 0)
		drm_output_vk_destroy_device(dev);
}

#endif /* defined(__linux__) */
