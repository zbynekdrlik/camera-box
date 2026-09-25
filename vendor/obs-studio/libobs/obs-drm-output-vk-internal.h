#pragma once

/*
 * camera-box issue 1346 — the vk-direct backend's INTERNAL seam between its two TUs:
 *   obs-drm-output-vk-setup.c  load Vulkan, find + acquire the X output, build the surface, device,
 *                              swapchain and the shared images; tear all of it down again
 *   obs-drm-output-vk.c        the present thread, the lifecycle API (obs-drm-output-vk.h) and the
 *                              GL side of the GL<->Vulkan interop
 * Not exported; Linux-only, like both TUs.
 */

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>

#include <X11/Xlib.h>

#define VK_NO_PROTOTYPES 1
#include <vulkan/vulkan.h>

#include <GL/gl.h>
#include <GL/glext.h>

#include "obs-drm-output-vk.h"

/* -------------------------------------------------------------------------------------------------
 * Vulkan entry points (the loader is dlopen'd; VK_NO_PROTOTYPES).
 * ------------------------------------------------------------------------------------------------- */

/* VK_EXT_acquire_xlib_display's two entry points, declared here with RROutput spelled as its XID type
 * (unsigned long) so neither TU needs <X11/extensions/Xrandr.h> (libxrandr-dev is not a build dep). */
#define DRM_OUTPUT_VK_EXT_ACQUIRE_XLIB "VK_EXT_acquire_xlib_display"
typedef VkResult(VKAPI_PTR *drm_output_vk_acquire_xlib_fn)(VkPhysicalDevice pd, Display *dpy, VkDisplayKHR display);
typedef VkResult(VKAPI_PTR *drm_output_vk_randr_display_fn)(VkPhysicalDevice pd, Display *dpy, unsigned long output,
							    VkDisplayKHR *display);

#define DRM_OUTPUT_VK_INSTANCE_FNS(X)                    \
	X(vkDestroyInstance)                             \
	X(vkEnumeratePhysicalDevices)                    \
	X(vkGetPhysicalDeviceProperties)                 \
	X(vkGetPhysicalDeviceQueueFamilyProperties)      \
	X(vkGetPhysicalDeviceMemoryProperties)           \
	X(vkEnumerateDeviceExtensionProperties)          \
	X(vkCreateDevice)                                \
	X(vkGetDeviceProcAddr)                           \
	X(vkGetPhysicalDeviceDisplayPropertiesKHR)       \
	X(vkGetDisplayModePropertiesKHR)                 \
	X(vkGetPhysicalDeviceDisplayPlanePropertiesKHR)  \
	X(vkGetDisplayPlaneSupportedDisplaysKHR)         \
	X(vkCreateDisplayPlaneSurfaceKHR)                \
	X(vkDestroySurfaceKHR)                           \
	X(vkGetPhysicalDeviceSurfaceSupportKHR)          \
	X(vkGetPhysicalDeviceSurfaceCapabilitiesKHR)     \
	X(vkGetPhysicalDeviceSurfaceFormatsKHR)          \
	X(vkReleaseDisplayEXT)

#define DRM_OUTPUT_VK_DEVICE_FNS(X)       \
	X(vkDestroyDevice)                \
	X(vkGetDeviceQueue)               \
	X(vkDeviceWaitIdle)               \
	X(vkCreateSwapchainKHR)           \
	X(vkDestroySwapchainKHR)          \
	X(vkGetSwapchainImagesKHR)        \
	X(vkAcquireNextImageKHR)          \
	X(vkQueuePresentKHR)              \
	X(vkQueueSubmit)                  \
	X(vkCreateCommandPool)            \
	X(vkDestroyCommandPool)           \
	X(vkAllocateCommandBuffers)       \
	X(vkBeginCommandBuffer)           \
	X(vkEndCommandBuffer)             \
	X(vkResetCommandBuffer)           \
	X(vkCmdPipelineBarrier)           \
	X(vkCmdBlitImage)                 \
	X(vkCmdClearColorImage)           \
	X(vkCreateSemaphore)              \
	X(vkDestroySemaphore)             \
	X(vkCreateFence)                  \
	X(vkDestroyFence)                 \
	X(vkWaitForFences)                \
	X(vkResetFences)                  \
	X(vkCreateImage)                  \
	X(vkDestroyImage)                 \
	X(vkGetImageMemoryRequirements)   \
	X(vkAllocateMemory)               \
	X(vkFreeMemory)                   \
	X(vkBindImageMemory)              \
	X(vkGetMemoryFdKHR)               \
	X(vkGetSemaphoreFdKHR)

#define DRM_OUTPUT_VK_DECLARE(name) PFN_##name name;

struct drm_output_vk_fns {
	PFN_vkGetInstanceProcAddr vkGetInstanceProcAddr;
	PFN_vkCreateInstance vkCreateInstance;
	PFN_vkEnumerateInstanceExtensionProperties vkEnumerateInstanceExtensionProperties;
	DRM_OUTPUT_VK_INSTANCE_FNS(DRM_OUTPUT_VK_DECLARE)
	DRM_OUTPUT_VK_DEVICE_FNS(DRM_OUTPUT_VK_DECLARE)
	drm_output_vk_randr_display_fn vkGetRandROutputDisplayEXT;
	drm_output_vk_acquire_xlib_fn vkAcquireXlibDisplayEXT;
};

/* GL 1.x entry points have no PFN typedef in <GL/glext.h>. */
typedef void(APIENTRYP drm_output_vk_gl_delete_textures_fn)(GLsizei n, const GLuint *textures);
typedef void(APIENTRYP drm_output_vk_gl_flush_fn)(void);
typedef GLenum(APIENTRYP drm_output_vk_gl_get_error_fn)(void);

/* The raw GL entry points of the interop (resolved in the CURRENT context's library). */
struct drm_output_vk_gl {
	PFNGLCREATEMEMORYOBJECTSEXTPROC CreateMemoryObjectsEXT;
	PFNGLDELETEMEMORYOBJECTSEXTPROC DeleteMemoryObjectsEXT;
	PFNGLMEMORYOBJECTPARAMETERIVEXTPROC MemoryObjectParameterivEXT;
	PFNGLIMPORTMEMORYFDEXTPROC ImportMemoryFdEXT;
	PFNGLCREATETEXTURESPROC CreateTextures;
	drm_output_vk_gl_delete_textures_fn DeleteTextures;
	PFNGLTEXTUREPARAMETERIPROC TextureParameteri;
	PFNGLTEXTURESTORAGEMEM2DEXTPROC TextureStorageMem2DEXT;
	PFNGLGENSEMAPHORESEXTPROC GenSemaphoresEXT;
	PFNGLDELETESEMAPHORESEXTPROC DeleteSemaphoresEXT;
	PFNGLIMPORTSEMAPHOREFDEXTPROC ImportSemaphoreFdEXT;
	PFNGLSIGNALSEMAPHOREEXTPROC SignalSemaphoreEXT;
	PFNGLWAITSEMAPHOREEXTPROC WaitSemaphoreEXT;
	PFNGLCOPYIMAGESUBDATAPROC CopyImageSubData;
	drm_output_vk_gl_flush_fn Flush;
	drm_output_vk_gl_get_error_fn GetError;
};

/* -------------------------------------------------------------------------------------------------
 * Module state (single instance).
 * ------------------------------------------------------------------------------------------------- */
#define DRM_OUTPUT_VK_MAX_SWAP 8
#define DRM_OUTPUT_VK_WAIT_NS 1000000000ull /* 1 s bound on every blocking Vulkan wait */

struct drm_output_vk_shared {
	VkImage image;
	VkDeviceMemory memory;
	VkDeviceSize size;
	VkSemaphore sem; /* GL signals (publish), the present thread waits (first copy of a publish) */
	GLuint gl_mem;
	GLuint gl_tex;
	GLuint gl_sem;
	bool armed; /* GL signalled `sem` and nobody consumed it yet (g_drm_vk.lock) */
};

struct drm_output_vk_state {
	pthread_mutex_t lock; /* the mailbox roles + armed flags */
	bool open;
	volatile bool running;     /* present-thread run flag (os_atomic) */
	volatile bool want_frames; /* the frame hook's fast gate (os_atomic) */
	bool thread_started;
	pthread_t thread;

	void *lib;
	struct drm_output_vk_fns vk;
	Display *dpy;
	char output_name[64];
	uint32_t solid_argb;

	VkInstance instance;
	VkPhysicalDevice pd;
	VkDisplayKHR display;
	bool acquired;
	VkSurfaceKHR surface;
	VkDevice device;
	VkQueue queue;
	uint32_t qfi;
	VkSwapchainKHR swapchain;
	VkImage swap_images[DRM_OUTPUT_VK_MAX_SWAP];
	uint32_t n_swap;
	VkSemaphore sem_done[DRM_OUTPUT_VK_MAX_SWAP];
	VkSemaphore sem_acquire;
	VkFence fence;
	VkCommandPool pool;
	VkCommandBuffer cmd;
	uint32_t mode_w;
	uint32_t mode_h;
	uint32_t refresh_mhz;

	struct drm_output_vk_shared shared[DRM_OUTPUT_VK_SHARED_IMAGES];
	int front;   /* image last presented (re-copied when nothing new), -1 before the first */
	int pending; /* image taken by the present thread, copy in flight */
	int ready;   /* newest published image not yet taken */

	struct drm_output_vk_gl gl;
	bool gl_bound; /* graphics thread only */

	unsigned long long presents;
	unsigned long long program_presents;
	bool scanout_live_logged;
};

/* The single instance (defined in obs-drm-output-vk.c). */
extern struct drm_output_vk_state g_drm_vk;

/* obs-drm-output-vk-setup.c: open the X display, load Vulkan, acquire output `output_name` and build
 * the surface, device, swapchain and shared images into g_drm_vk. false = nothing usable (the caller
 * runs drm_output_vk_destroy_all). */
bool drm_output_vk_setup(const char *output_name);

/* obs-drm-output-vk-setup.c: free everything setup built (idempotent, safe on partial init) and release
 * the display. The present thread must be halted. */
void drm_output_vk_destroy_all(void);
