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

#include <X11/Xlib-xcb.h>
#include <xcb/randr.h>
#include <xcb/xcb.h>

#include "util/base.h"
#include "obs-drm-output-vk-internal.h"

/* -------------------------------------------------------------------------------------------------
 * Pure decision helper (Tier-0, lift-compiled + truth-tabled by tests/drm_output_vk_direct_1346.rs).
 * ------------------------------------------------------------------------------------------------- */

/* Pick the display mode to present: among the modes whose visible size equals the display's native
 * (physical) resolution — or among all modes when the native size is unknown (0) or no mode matches
 * it — the one whose refresh (mHz) is closest to 60 Hz; the first wins a tie. -1 when n <= 0. The
 * lease backend takes the connector's preferred mode; Vulkan exposes no "preferred" flag, so the
 * physical resolution stands in for it, and the rig's genlock cadence is 60 Hz. */
static int drm_output_vk_pick_mode(const uint32_t *w, const uint32_t *h, const uint32_t *refresh_mhz, int n,
				   uint32_t native_w, uint32_t native_h)
{
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
static bool drm_output_vk_load(void)
{
	g_drm_vk.lib = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
	if (!g_drm_vk.lib) {
		blog(LOG_WARNING, "drm-output: vk-direct: libvulkan.so.1 not loadable (%s) -- output dormant",
		     dlerror());
		return false;
	}
	g_drm_vk.vk.vkGetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)dlsym(g_drm_vk.lib, "vkGetInstanceProcAddr");
	if (!g_drm_vk.vk.vkGetInstanceProcAddr) {
		blog(LOG_WARNING, "drm-output: vk-direct: libvulkan.so.1 has no vkGetInstanceProcAddr -- output dormant");
		return false;
	}
	g_drm_vk.vk.vkCreateInstance = (PFN_vkCreateInstance)g_drm_vk.vk.vkGetInstanceProcAddr(NULL, "vkCreateInstance");
	g_drm_vk.vk.vkEnumerateInstanceExtensionProperties = (PFN_vkEnumerateInstanceExtensionProperties)
		g_drm_vk.vk.vkGetInstanceProcAddr(NULL, "vkEnumerateInstanceExtensionProperties");
	return g_drm_vk.vk.vkCreateInstance && g_drm_vk.vk.vkEnumerateInstanceExtensionProperties;
}

static bool drm_output_vk_create_instance(void)
{
	const char *want[] = {VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_DISPLAY_EXTENSION_NAME,
			      VK_EXT_DIRECT_MODE_DISPLAY_EXTENSION_NAME, DRM_OUTPUT_VK_EXT_ACQUIRE_XLIB};
	const uint32_t n_want = (uint32_t)(sizeof(want) / sizeof(want[0]));

	uint32_t n = 0;
	g_drm_vk.vk.vkEnumerateInstanceExtensionProperties(NULL, &n, NULL);
	VkExtensionProperties *props = calloc(n ? n : 1, sizeof(*props));
	if (!props)
		return false;
	g_drm_vk.vk.vkEnumerateInstanceExtensionProperties(NULL, &n, props);
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
	VkResult r = g_drm_vk.vk.vkCreateInstance(&ici, NULL, &g_drm_vk.instance);
	if (r != VK_SUCCESS) {
		blog(LOG_WARNING, "drm-output: vk-direct: vkCreateInstance FAILED VkResult %d", (int)r);
		return false;
	}

	bool all = true;
#define DRM_OUTPUT_VK_LOAD_I(name)                                                                         \
	g_drm_vk.vk.name = (PFN_##name)g_drm_vk.vk.vkGetInstanceProcAddr(g_drm_vk.instance, #name);                    \
	if (!g_drm_vk.vk.name) {                                                                               \
		blog(LOG_WARNING, "drm-output: vk-direct: instance entry point " #name " missing");       \
		all = false;                                                                               \
	}
	DRM_OUTPUT_VK_INSTANCE_FNS(DRM_OUTPUT_VK_LOAD_I)
#undef DRM_OUTPUT_VK_LOAD_I
	g_drm_vk.vk.vkGetRandROutputDisplayEXT = (drm_output_vk_randr_display_fn)g_drm_vk.vk.vkGetInstanceProcAddr(
		g_drm_vk.instance, "vkGetRandROutputDisplayEXT");
	g_drm_vk.vk.vkAcquireXlibDisplayEXT =
		(drm_output_vk_acquire_xlib_fn)g_drm_vk.vk.vkGetInstanceProcAddr(g_drm_vk.instance, "vkAcquireXlibDisplayEXT");
	if (!g_drm_vk.vk.vkGetRandROutputDisplayEXT || !g_drm_vk.vk.vkAcquireXlibDisplayEXT) {
		blog(LOG_WARNING, "drm-output: vk-direct: VK_EXT_acquire_xlib_display entry points missing");
		all = false;
	}
	return all;
}

/* The RandR output XID named `name` on our own X connection; logs the X-layout precondition. */
static unsigned long drm_output_vk_find_output(const char *name)
{
	xcb_connection_t *conn = XGetXCBConnection(g_drm_vk.dpy);
	xcb_window_t root = (xcb_window_t)DefaultRootWindow(g_drm_vk.dpy);
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

/* Pick the GPU that owns the output, and acquire the display away from X. */
static bool drm_output_vk_acquire(unsigned long output)
{
	uint32_t n = 0;
	if (g_drm_vk.vk.vkEnumeratePhysicalDevices(g_drm_vk.instance, &n, NULL) != VK_SUCCESS || n == 0) {
		blog(LOG_WARNING, "drm-output: vk-direct: no Vulkan physical device");
		return false;
	}
	VkPhysicalDevice pds[8];
	if (n > 8)
		n = 8;
	g_drm_vk.vk.vkEnumeratePhysicalDevices(g_drm_vk.instance, &n, pds);
	for (uint32_t i = 0; i < n && !g_drm_vk.pd; i++) {
		VkDisplayKHR d = VK_NULL_HANDLE;
		if (g_drm_vk.vk.vkGetRandROutputDisplayEXT(pds[i], g_drm_vk.dpy, output, &d) == VK_SUCCESS && d != VK_NULL_HANDLE) {
			g_drm_vk.pd = pds[i];
			g_drm_vk.display = d;
		}
	}
	if (!g_drm_vk.pd) {
		blog(LOG_WARNING, "drm-output: vk-direct: no Vulkan device drives output '%s'", g_drm_vk.output_name);
		return false;
	}
	VkPhysicalDeviceProperties pr;
	g_drm_vk.vk.vkGetPhysicalDeviceProperties(g_drm_vk.pd, &pr);
	VkResult r = g_drm_vk.vk.vkAcquireXlibDisplayEXT(g_drm_vk.pd, g_drm_vk.dpy, g_drm_vk.display);
	if (r != VK_SUCCESS) {
		blog(LOG_WARNING,
		     "drm-output: vk-direct: vkAcquireXlibDisplayEXT('%s') FAILED VkResult %d on %s (-13 = the output "
		     "is still in the X layout)",
		     g_drm_vk.output_name, (int)r, pr.deviceName);
		return false;
	}
	g_drm_vk.acquired = true;
	blog(LOG_INFO, "drm-output: vk-direct display acquired output='%s' gpu='%s' (off the X desktop)",
	     g_drm_vk.output_name, pr.deviceName);
	return true;
}

/* The native ~60 Hz mode, a display plane that can show it, and the display-plane surface. The chosen
 * mode is committed to g_drm_vk.mode_* only when the surface exists. `rebuild` (the present thread's
 * rebuild after a lost surface) requires the mode to have the SAME size as the committed one -- the
 * shared images and the OBS intermediate are sized to it, and the graphics thread reads mode_* without
 * a lock -- so a rebuild never changes mode_* at all; a different size refuses by name. */
static bool drm_output_vk_create_surface(bool rebuild)
{
	VkExtent2D native = {0, 0};
	uint32_t nd = 0;
	g_drm_vk.vk.vkGetPhysicalDeviceDisplayPropertiesKHR(g_drm_vk.pd, &nd, NULL);
	VkDisplayPropertiesKHR *dprops = calloc(nd ? nd : 1, sizeof(*dprops));
	if (!dprops)
		return false;
	g_drm_vk.vk.vkGetPhysicalDeviceDisplayPropertiesKHR(g_drm_vk.pd, &nd, dprops);
	for (uint32_t i = 0; i < nd; i++)
		if (dprops[i].display == g_drm_vk.display)
			native = dprops[i].physicalResolution;
	free(dprops);

	uint32_t nm = 0;
	if (g_drm_vk.vk.vkGetDisplayModePropertiesKHR(g_drm_vk.pd, g_drm_vk.display, &nm, NULL) != VK_SUCCESS || nm == 0) {
		blog(LOG_WARNING, "drm-output: vk-direct: the display reports no modes");
		return false;
	}
	VkDisplayModePropertiesKHR *modes = calloc(nm, sizeof(*modes));
	uint32_t *mw = calloc(nm, sizeof(uint32_t));
	uint32_t *mh = calloc(nm, sizeof(uint32_t));
	uint32_t *mr = calloc(nm, sizeof(uint32_t));
	bool ok = false;
	if (modes && mw && mh && mr && g_drm_vk.vk.vkGetDisplayModePropertiesKHR(g_drm_vk.pd, g_drm_vk.display, &nm, modes) == VK_SUCCESS) {
		for (uint32_t i = 0; i < nm; i++) {
			mw[i] = modes[i].parameters.visibleRegion.width;
			mh[i] = modes[i].parameters.visibleRegion.height;
			mr[i] = modes[i].parameters.refreshRate;
		}
		int best = drm_output_vk_pick_mode(mw, mh, mr, (int)nm, native.width, native.height);
		if (best >= 0) {
			VkDisplayModePropertiesKHR mode = modes[best];
			const uint32_t sel_w = mw[best], sel_h = mh[best], sel_hz = mr[best];
			if (rebuild && (sel_w != g_drm_vk.mode_w || sel_h != g_drm_vk.mode_h)) {
				blog(LOG_WARNING,
				     "drm-output: vk-direct: the display now offers %ux%u instead of %ux%u -- the shared images "
				     "no longer fit (restart OBS)",
				     sel_w, sel_h, g_drm_vk.mode_w, g_drm_vk.mode_h);
				free(modes);
				free(mw);
				free(mh);
				free(mr);
				return false;
			}

			uint32_t npl = 0;
			g_drm_vk.vk.vkGetPhysicalDeviceDisplayPlanePropertiesKHR(g_drm_vk.pd, &npl, NULL);
			VkDisplayPlanePropertiesKHR *plp = calloc(npl ? npl : 1, sizeof(*plp));
			int plane = -1;
			if (plp) {
				g_drm_vk.vk.vkGetPhysicalDeviceDisplayPlanePropertiesKHR(g_drm_vk.pd, &npl, plp);
				for (uint32_t p = 0; p < npl && plane < 0; p++) {
					if (plp[p].currentDisplay != VK_NULL_HANDLE && plp[p].currentDisplay != g_drm_vk.display)
						continue;
					uint32_t ns = 0;
					g_drm_vk.vk.vkGetDisplayPlaneSupportedDisplaysKHR(g_drm_vk.pd, p, &ns, NULL);
					VkDisplayKHR sup[16];
					if (ns > 16)
						ns = 16;
					g_drm_vk.vk.vkGetDisplayPlaneSupportedDisplaysKHR(g_drm_vk.pd, p, &ns, sup);
					for (uint32_t j = 0; j < ns; j++)
						if (sup[j] == g_drm_vk.display)
							plane = (int)p;
				}
			}
			if (plane < 0) {
				blog(LOG_WARNING, "drm-output: vk-direct: no display plane can show output '%s'",
				     g_drm_vk.output_name);
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
				VkResult r = g_drm_vk.vk.vkCreateDisplayPlaneSurfaceKHR(g_drm_vk.instance, &dsci, NULL, &g_drm_vk.surface);
				if (r == VK_SUCCESS) {
					ok = true;
					g_drm_vk.mode_w = sel_w;
					g_drm_vk.mode_h = sel_h;
					g_drm_vk.refresh_mhz = sel_hz;
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
	free(modes);
	free(mw);
	free(mh);
	free(mr);
	return ok;
}

static bool drm_output_vk_create_device(void)
{
	uint32_t nq = 0;
	g_drm_vk.vk.vkGetPhysicalDeviceQueueFamilyProperties(g_drm_vk.pd, &nq, NULL);
	VkQueueFamilyProperties qf[16];
	if (nq > 16)
		nq = 16;
	g_drm_vk.vk.vkGetPhysicalDeviceQueueFamilyProperties(g_drm_vk.pd, &nq, qf);
	int qfi = -1;
	for (uint32_t i = 0; i < nq && qfi < 0; i++) {
		VkBool32 present = VK_FALSE;
		g_drm_vk.vk.vkGetPhysicalDeviceSurfaceSupportKHR(g_drm_vk.pd, i, g_drm_vk.surface, &present);
		if (present && (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT))
			qfi = (int)i;
	}
	if (qfi < 0) {
		blog(LOG_WARNING, "drm-output: vk-direct: no graphics queue can present to the display surface");
		return false;
	}
	g_drm_vk.qfi = (uint32_t)qfi;

	const char *want[] = {VK_KHR_SWAPCHAIN_EXTENSION_NAME, VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
			      VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME};
	const uint32_t n_want = (uint32_t)(sizeof(want) / sizeof(want[0]));
	uint32_t n = 0;
	g_drm_vk.vk.vkEnumerateDeviceExtensionProperties(g_drm_vk.pd, NULL, &n, NULL);
	VkExtensionProperties *props = calloc(n ? n : 1, sizeof(*props));
	if (!props)
		return false;
	g_drm_vk.vk.vkEnumerateDeviceExtensionProperties(g_drm_vk.pd, NULL, &n, props);
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

	float prio = 1.0f;
	VkDeviceQueueCreateInfo dqci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
					.queueFamilyIndex = g_drm_vk.qfi,
					.queueCount = 1,
					.pQueuePriorities = &prio};
	VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
				  .queueCreateInfoCount = 1,
				  .pQueueCreateInfos = &dqci,
				  .enabledExtensionCount = n_want,
				  .ppEnabledExtensionNames = want};
	VkResult r = g_drm_vk.vk.vkCreateDevice(g_drm_vk.pd, &dci, NULL, &g_drm_vk.device);
	if (r != VK_SUCCESS) {
		blog(LOG_WARNING, "drm-output: vk-direct: vkCreateDevice FAILED VkResult %d", (int)r);
		return false;
	}
	bool all = true;
#define DRM_OUTPUT_VK_LOAD_D(name)                                                                  \
	g_drm_vk.vk.name = (PFN_##name)g_drm_vk.vk.vkGetDeviceProcAddr(g_drm_vk.device, #name);                 \
	if (!g_drm_vk.vk.name) {                                                                        \
		blog(LOG_WARNING, "drm-output: vk-direct: device entry point " #name " missing");  \
		all = false;                                                                        \
	}
	DRM_OUTPUT_VK_DEVICE_FNS(DRM_OUTPUT_VK_LOAD_D)
#undef DRM_OUTPUT_VK_LOAD_D
	if (!all)
		return false;
	g_drm_vk.vk.vkGetDeviceQueue(g_drm_vk.device, g_drm_vk.qfi, 0, &g_drm_vk.queue);
	return true;
}

/* The swapchain (+ its images and per-image present semaphores) at the current mode. Called at open
 * and by the rebuild. */
static bool drm_output_vk_create_swapchain_only(VkSwapchainKHR old_swapchain)
{
	VkSurfaceCapabilitiesKHR caps;
	if (g_drm_vk.vk.vkGetPhysicalDeviceSurfaceCapabilitiesKHR(g_drm_vk.pd, g_drm_vk.surface, &caps) != VK_SUCCESS)
		return false;
	if (!(caps.supportedUsageFlags & VK_IMAGE_USAGE_TRANSFER_DST_BIT)) {
		blog(LOG_WARNING, "drm-output: vk-direct: the display surface cannot be a transfer destination");
		return false;
	}
	uint32_t nf = 0;
	g_drm_vk.vk.vkGetPhysicalDeviceSurfaceFormatsKHR(g_drm_vk.pd, g_drm_vk.surface, &nf, NULL);
	VkSurfaceFormatKHR fmts[32];
	VkFormat plain[32];
	if (nf > 32)
		nf = 32;
	if (nf == 0 || g_drm_vk.vk.vkGetPhysicalDeviceSurfaceFormatsKHR(g_drm_vk.pd, g_drm_vk.surface, &nf, fmts) != VK_SUCCESS)
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
	g_drm_vk.vk.vkGetPhysicalDeviceFormatProperties(g_drm_vk.pd, fmt.format, &dstp);
	g_drm_vk.vk.vkGetPhysicalDeviceFormatProperties(g_drm_vk.pd, VK_FORMAT_R8G8B8A8_UNORM, &srcp);
	if (!(dstp.optimalTilingFeatures & VK_FORMAT_FEATURE_BLIT_DST_BIT) ||
	    !(srcp.optimalTilingFeatures & VK_FORMAT_FEATURE_BLIT_SRC_BIT)) {
		blog(LOG_WARNING, "drm-output: vk-direct: the GPU cannot blit RGBA8 into swapchain format %d", (int)fmt.format);
		return false;
	}

	uint32_t min_images = caps.minImageCount < 2 ? 2 : caps.minImageCount;
	if (caps.maxImageCount && min_images > caps.maxImageCount)
		min_images = caps.maxImageCount;
	VkSwapchainCreateInfoKHR sci = {.sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
					.surface = g_drm_vk.surface,
					.minImageCount = min_images,
					.imageFormat = fmt.format,
					.imageColorSpace = fmt.colorSpace,
					.imageExtent = {g_drm_vk.mode_w, g_drm_vk.mode_h},
					.imageArrayLayers = 1,
					.imageUsage = VK_IMAGE_USAGE_TRANSFER_DST_BIT,
					.imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
					.preTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
					.compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
					/* FIFO = vblank-locked, never tearing (the only mode the spec guarantees). */
					.presentMode = VK_PRESENT_MODE_FIFO_KHR,
					.clipped = VK_TRUE,
					.oldSwapchain = old_swapchain};
	VkResult r = g_drm_vk.vk.vkCreateSwapchainKHR(g_drm_vk.device, &sci, NULL, &g_drm_vk.swapchain);
	if (r != VK_SUCCESS) {
		g_drm_vk.swapchain = VK_NULL_HANDLE;
		blog(LOG_WARNING, "drm-output: vk-direct: vkCreateSwapchainKHR FAILED VkResult %d", (int)r);
		return false;
	}
	uint32_t n = 0;
	g_drm_vk.vk.vkGetSwapchainImagesKHR(g_drm_vk.device, g_drm_vk.swapchain, &n, NULL);
	if (n == 0 || n > DRM_OUTPUT_VK_MAX_SWAP) {
		blog(LOG_WARNING, "drm-output: vk-direct: unexpected swapchain image count %u", n);
		return false;
	}
	g_drm_vk.vk.vkGetSwapchainImagesKHR(g_drm_vk.device, g_drm_vk.swapchain, &n, g_drm_vk.swap_images);
	g_drm_vk.n_swap = n;

	/* The per-image present semaphores live across rebuilds (a retired swapchain's queued present may
	 * still wait one; re-signalling a semaphore whose earlier wait is already submitted is valid): only
	 * the missing slots are created, all of them go in the teardown. */
	VkSemaphoreCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
	for (uint32_t i = 0; i < n; i++)
		if (!g_drm_vk.sem_done[i] &&
		    g_drm_vk.vk.vkCreateSemaphore(g_drm_vk.device, &smci, NULL, &g_drm_vk.sem_done[i]) != VK_SUCCESS) {
			g_drm_vk.sem_done[i] = VK_NULL_HANDLE;
			return false;
		}
	blog(LOG_INFO, "drm-output: vk-direct swapchain %ux%u FIFO images=%u format=%d", g_drm_vk.mode_w,
	     g_drm_vk.mode_h, n, (int)fmt.format);
	return true;
}

/* The swapchain + the per-vblank sync objects and command buffer (open only). */
static bool drm_output_vk_create_swapchain(void)
{
	if (!drm_output_vk_create_swapchain_only(VK_NULL_HANDLE))
		return false;
	VkSemaphoreCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
	if (g_drm_vk.vk.vkCreateSemaphore(g_drm_vk.device, &smci, NULL, &g_drm_vk.sem_acquire) != VK_SUCCESS)
		return false;
	VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
	if (g_drm_vk.vk.vkCreateFence(g_drm_vk.device, &fci, NULL, &g_drm_vk.fence) != VK_SUCCESS)
		return false;
	VkCommandPoolCreateInfo cpci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
					.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
					.queueFamilyIndex = g_drm_vk.qfi};
	if (g_drm_vk.vk.vkCreateCommandPool(g_drm_vk.device, &cpci, NULL, &g_drm_vk.pool) != VK_SUCCESS)
		return false;
	VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
					    .commandPool = g_drm_vk.pool,
					    .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
					    .commandBufferCount = 1};
	return g_drm_vk.vk.vkAllocateCommandBuffers(g_drm_vk.device, &cbai, &g_drm_vk.cmd) == VK_SUCCESS;
}

/* Drop the current swapchain (the device must be idle -- the teardown). The present semaphores stay
 * (destroy_all frees them). */
static void drm_output_vk_destroy_swapchain(void)
{
	if (g_drm_vk.swapchain)
		g_drm_vk.vk.vkDestroySwapchainKHR(g_drm_vk.device, g_drm_vk.swapchain, NULL);
	g_drm_vk.swapchain = VK_NULL_HANDLE;
	memset(g_drm_vk.swap_images, 0, sizeof(g_drm_vk.swap_images));
	g_drm_vk.n_swap = 0;
}

void drm_output_vk_free_retired(void)
{
	if (g_drm_vk.retired_swapchain)
		g_drm_vk.vk.vkDestroySwapchainKHR(g_drm_vk.device, g_drm_vk.retired_swapchain, NULL);
	if (g_drm_vk.retired_surface)
		g_drm_vk.vk.vkDestroySurfaceKHR(g_drm_vk.instance, g_drm_vk.retired_surface, NULL);
	g_drm_vk.retired_swapchain = VK_NULL_HANDLE;
	g_drm_vk.retired_surface = VK_NULL_HANDLE;
	g_drm_vk.retire_countdown = 0;
}

bool drm_output_vk_rebuild_presentation(bool surface_lost)
{
	const uint32_t w = g_drm_vk.mode_w, h = g_drm_vk.mode_h;
	/* No device wait (it cannot be bounded and halt() joins this thread). The replaced swapchain -- whose
	 * last present may still be queued, waiting a present semaphore -- is RETIRED, not destroyed: it is
	 * handed to the new swapchain as oldSwapchain (same surface) and freed after RETIRE_FRAMES later
	 * signalled fences or at the teardown.
	 * Back-to-back rebuilds (a rebuilt swapchain whose very first acquire is out of date again): the
	 * current presentation never presented, so nothing is queued on it -- destroy it directly and KEEP
	 * the older retiree, whose present may still be queued. Only a presentation that completed a fenced
	 * present since the last rebuild replaces the retiree (>= 1 fence has passed since it was retired). */
	const bool pending_retiree = g_drm_vk.retired_swapchain || g_drm_vk.retired_surface;
	const bool fresh = pending_retiree && !g_drm_vk.presented_since_rebuild;
	VkSwapchainKHR old_for_create = VK_NULL_HANDLE;
	if (fresh) {
		drm_output_vk_destroy_swapchain();
		if (surface_lost && g_drm_vk.surface) {
			if (!g_drm_vk.retired_surface) {
				/* the retiree was retired on THIS surface: the surface must outlive it, retire it too */
				g_drm_vk.retired_surface = g_drm_vk.surface;
			} else {
				/* the retiree lives on an older surface: this one carried only the never-used swapchain */
				g_drm_vk.vk.vkDestroySurfaceKHR(g_drm_vk.instance, g_drm_vk.surface, NULL);
			}
			g_drm_vk.surface = VK_NULL_HANDLE;
		}
	} else {
		drm_output_vk_free_retired();
		g_drm_vk.retired_swapchain = g_drm_vk.swapchain;
		g_drm_vk.swapchain = VK_NULL_HANDLE;
		memset(g_drm_vk.swap_images, 0, sizeof(g_drm_vk.swap_images));
		g_drm_vk.n_swap = 0;
		g_drm_vk.retire_countdown = DRM_OUTPUT_VK_RETIRE_FRAMES;
		if (surface_lost) {
			/* the retired swapchain belongs to the old surface: retire both, free them together */
			g_drm_vk.retired_surface = g_drm_vk.surface;
			g_drm_vk.surface = VK_NULL_HANDLE;
		} else {
			old_for_create = g_drm_vk.retired_swapchain; /* a swapchain is retired into at most one */
		}
	}
	g_drm_vk.presented_since_rebuild = false;
	if (surface_lost) {
		if (!drm_output_vk_create_surface(true))
			return false;
		VkBool32 present = VK_FALSE;
		g_drm_vk.vk.vkGetPhysicalDeviceSurfaceSupportKHR(g_drm_vk.pd, g_drm_vk.qfi, g_drm_vk.surface, &present);
		if (!present) {
			blog(LOG_WARNING, "drm-output: vk-direct: the rebuilt display surface is not presentable from queue family %u",
			     g_drm_vk.qfi);
			return false;
		}
	}
	if (!drm_output_vk_create_swapchain_only(old_for_create)) {
		drm_output_vk_destroy_swapchain();
		return false;
	}
	blog(LOG_INFO, "drm-output: vk-direct presentation rebuilt on '%s' (%ux%u)", g_drm_vk.output_name, w, h);
	return true;
}

/* The shared images: device-local, optimal tiling, exportable as OPAQUE_FD with a DEDICATED allocation
 * (the GL side declares GL_DEDICATED_MEMORY_OBJECT_EXT to match), + an exportable binary semaphore each.
 * RGBA8 UNORM = GL_RGBA8 storage byte-for-byte; the present blit converts to the swapchain's BGRA. */
static bool drm_output_vk_create_shared(void)
{
	VkPhysicalDeviceMemoryProperties mp;
	g_drm_vk.vk.vkGetPhysicalDeviceMemoryProperties(g_drm_vk.pd, &mp);
	for (int i = 0; i < DRM_OUTPUT_VK_SHARED_IMAGES; i++) {
		struct drm_output_vk_shared *s = &g_drm_vk.shared[i];
		VkExternalMemoryImageCreateInfo emi = {.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
						       .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT};
		VkImageCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
					 .pNext = &emi,
					 .imageType = VK_IMAGE_TYPE_2D,
					 .format = VK_FORMAT_R8G8B8A8_UNORM,
					 .extent = {g_drm_vk.mode_w, g_drm_vk.mode_h, 1},
					 .mipLevels = 1,
					 .arrayLayers = 1,
					 .samples = VK_SAMPLE_COUNT_1_BIT,
					 .tiling = VK_IMAGE_TILING_OPTIMAL,
					 .usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
						  VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
					 .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
					 .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED};
		if (g_drm_vk.vk.vkCreateImage(g_drm_vk.device, &ici, NULL, &s->image) != VK_SUCCESS) {
			blog(LOG_WARNING, "drm-output: vk-direct: shared image %d create failed", i);
			return false;
		}
		VkMemoryRequirements req;
		g_drm_vk.vk.vkGetImageMemoryRequirements(g_drm_vk.device, s->image, &req);
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
		if (g_drm_vk.vk.vkAllocateMemory(g_drm_vk.device, &mai, NULL, &s->memory) != VK_SUCCESS ||
		    g_drm_vk.vk.vkBindImageMemory(g_drm_vk.device, s->image, s->memory, 0) != VK_SUCCESS) {
			blog(LOG_WARNING, "drm-output: vk-direct: shared image %d memory allocation failed", i);
			return false;
		}
		s->size = req.size;
		VkExportSemaphoreCreateInfo esi = {.sType = VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO,
						   .handleTypes = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT};
		VkSemaphoreCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO, .pNext = &esi};
		if (g_drm_vk.vk.vkCreateSemaphore(g_drm_vk.device, &smci, NULL, &s->sem) != VK_SUCCESS) {
			blog(LOG_WARNING, "drm-output: vk-direct: shared semaphore %d create failed", i);
			return false;
		}
	}
	return true;
}

bool drm_output_vk_setup(const char *output_name)
{
	g_drm_vk.dpy = XOpenDisplay(NULL);
	if (!g_drm_vk.dpy) {
		blog(LOG_WARNING, "drm-output: vk-direct: cannot open the X display (DISPLAY) -- output dormant");
		return false;
	}
	unsigned long output = 0;
	return drm_output_vk_load() && drm_output_vk_create_instance() &&
	       (output = drm_output_vk_find_output(output_name)) != 0 && drm_output_vk_acquire(output) &&
	       drm_output_vk_create_surface(false) && drm_output_vk_create_device() && drm_output_vk_create_swapchain() &&
	       drm_output_vk_create_shared();
}

/* Free everything open() built (idempotent, safe on partial init). The present thread is halted. */
void drm_output_vk_destroy_all(void)
{
	if (g_drm_vk.device && g_drm_vk.submit_outstanding && g_drm_vk.fence) {
		/* The present loop exited on a fence it never saw signalled: wait BOUNDED (5 x 1 s). A GPU that
		 * never finishes the copy leaks the Vulkan objects instead of hanging the OBS shutdown in an
		 * unbounded vkDeviceWaitIdle. */
		VkResult fr = VK_TIMEOUT;
		for (int i = 0; i < 5 && fr == VK_TIMEOUT; i++)
			fr = g_drm_vk.vk.vkWaitForFences(g_drm_vk.device, 1, &g_drm_vk.fence, VK_TRUE, DRM_OUTPUT_VK_WAIT_NS);
		if (fr == VK_TIMEOUT) { /* a lost device returns at once: destroy it normally below */
			blog(LOG_WARNING,
			     "drm-output: vk-direct: the last copy never completed (VkResult %d) -- leaking the Vulkan objects "
			     "of '%s' instead of hanging the shutdown",
			     (int)fr, g_drm_vk.output_name);
			g_drm_vk.device = VK_NULL_HANDLE;
			g_drm_vk.surface = VK_NULL_HANDLE;
			g_drm_vk.acquired = false;
			g_drm_vk.instance = VK_NULL_HANDLE;
			/* the driver may still use the loader and the X connection: leak them too */
			g_drm_vk.lib = NULL;
			g_drm_vk.dpy = NULL;
		}
		g_drm_vk.submit_outstanding = false;
	}
	if (g_drm_vk.device) {
		g_drm_vk.vk.vkDeviceWaitIdle(g_drm_vk.device);
		for (int i = 0; i < DRM_OUTPUT_VK_SHARED_IMAGES; i++) {
			struct drm_output_vk_shared *s = &g_drm_vk.shared[i];
			if (s->sem)
				g_drm_vk.vk.vkDestroySemaphore(g_drm_vk.device, s->sem, NULL);
			if (s->image)
				g_drm_vk.vk.vkDestroyImage(g_drm_vk.device, s->image, NULL);
			if (s->memory)
				g_drm_vk.vk.vkFreeMemory(g_drm_vk.device, s->memory, NULL);
			memset(s, 0, sizeof(*s));
		}
		if (g_drm_vk.pool)
			g_drm_vk.vk.vkDestroyCommandPool(g_drm_vk.device, g_drm_vk.pool, NULL);
		if (g_drm_vk.fence)
			g_drm_vk.vk.vkDestroyFence(g_drm_vk.device, g_drm_vk.fence, NULL);
		if (g_drm_vk.sem_acquire)
			g_drm_vk.vk.vkDestroySemaphore(g_drm_vk.device, g_drm_vk.sem_acquire, NULL);
		drm_output_vk_destroy_swapchain();
		drm_output_vk_free_retired();
		for (uint32_t i = 0; i < DRM_OUTPUT_VK_MAX_SWAP; i++) {
			if (g_drm_vk.sem_done[i])
				g_drm_vk.vk.vkDestroySemaphore(g_drm_vk.device, g_drm_vk.sem_done[i], NULL);
			g_drm_vk.sem_done[i] = VK_NULL_HANDLE;
		}
		g_drm_vk.vk.vkDestroyDevice(g_drm_vk.device, NULL);
	}
	if (g_drm_vk.surface)
		g_drm_vk.vk.vkDestroySurfaceKHR(g_drm_vk.instance, g_drm_vk.surface, NULL);
	if (g_drm_vk.acquired) {
		VkResult r = g_drm_vk.vk.vkReleaseDisplayEXT(g_drm_vk.pd, g_drm_vk.display);
		blog(LOG_INFO,
		     "drm-output: vk-direct display released (VkResult %d) -- '%s' stays OFF in X, never back on the "
		     "desktop by itself",
		     (int)r, g_drm_vk.output_name);
	}
	if (g_drm_vk.instance)
		g_drm_vk.vk.vkDestroyInstance(g_drm_vk.instance, NULL);
	if (g_drm_vk.dpy)
		XCloseDisplay(g_drm_vk.dpy);
	if (g_drm_vk.lib)
		dlclose(g_drm_vk.lib);

	g_drm_vk.lib = NULL;
	memset(&g_drm_vk.vk, 0, sizeof(g_drm_vk.vk));
	g_drm_vk.dpy = NULL;
	g_drm_vk.instance = VK_NULL_HANDLE;
	g_drm_vk.pd = VK_NULL_HANDLE;
	g_drm_vk.display = VK_NULL_HANDLE;
	g_drm_vk.acquired = false;
	g_drm_vk.surface = VK_NULL_HANDLE;
	g_drm_vk.device = VK_NULL_HANDLE;
	g_drm_vk.queue = VK_NULL_HANDLE;
	g_drm_vk.swapchain = VK_NULL_HANDLE;
	memset(g_drm_vk.swap_images, 0, sizeof(g_drm_vk.swap_images));
	memset(g_drm_vk.sem_done, 0, sizeof(g_drm_vk.sem_done));
	g_drm_vk.n_swap = 0;
	g_drm_vk.sem_acquire = VK_NULL_HANDLE;
	g_drm_vk.fence = VK_NULL_HANDLE;
	g_drm_vk.pool = VK_NULL_HANDLE;
	g_drm_vk.cmd = VK_NULL_HANDLE;
	g_drm_vk.retired_swapchain = VK_NULL_HANDLE; /* leaked with a wedged device, or already freed */
	g_drm_vk.retired_surface = VK_NULL_HANDLE;
	g_drm_vk.retire_countdown = 0;
	g_drm_vk.presented_since_rebuild = false;
	g_drm_vk.front = g_drm_vk.pending = g_drm_vk.ready = -1;
	g_drm_vk.open = false;
}

#endif /* defined(__linux__) */
