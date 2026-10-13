#pragma once

/*
 * camera-box issue 1346 — the vk-direct backend's INTERNAL seam between its two TUs:
 *   obs-drm-output-vk-setup.c  load Vulkan, find + acquire the X output, build the surface, device,
 *                              swapchain and the shared images; tear all of it down again
 *   obs-drm-output-vk.c        the present thread, the lifecycle API (obs-drm-output-vk.h) and the
 *                              GL side of the GL<->Vulkan interop
 * issue 1424 part 2: the state is split into ONE shared device (struct drm_output_vk_device) and one
 * struct drm_output_vk_disp per display, so two connectors scan out at once on one VkDevice.
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
	X(vkGetPhysicalDeviceFormatProperties)           \
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
	PFNGLCOPYIMAGESUBDATAPROC CopyImageSubData;
	drm_output_vk_gl_flush_fn Flush;
	drm_output_vk_gl_flush_fn Finish; /* same signature as glFlush */
	drm_output_vk_gl_get_error_fn GetError;
};

/* -------------------------------------------------------------------------------------------------
 * The core's state (issue 1424 part 2): ONE shared device + one display struct per output.
 * ------------------------------------------------------------------------------------------------- */
#define DRM_OUTPUT_VK_MAX_SWAP 8
#define DRM_OUTPUT_VK_WAIT_NS 1000000000ull /* 1 s bound on every blocking Vulkan wait */
/* A swapchain-only rebuild's old swapchain is freed only after this many later signalled copy fences --
 * its queued present may still wait on a semaphore -- or at the teardown's idle wait. A rebuild with a
 * NEW surface never retires: it drains and destroys the old presentation first (issue 1424, live
 * strih-pp: a retired display-plane surface destroyed after its replacement presents ends the new
 * presentation with VkResult -13). */
#define DRM_OUTPUT_VK_RETIRE_FRAMES 4u
/* issue 1424 part 2 (live strih-pp): NVIDIA refuses vkAcquireXlibDisplayEXT (-13) while ANOTHER display
 * of the process brings a new presentation up (its first presents = the mode set on its head). A
 * presentation is settled after this many presents on its current surface: on a 2-image FIFO swapchain
 * the third acquire returns only after the first image left the screen. Measured: an acquire 50 ms after
 * the other display's open succeeds, 0 us after it fails. */
#define DRM_OUTPUT_VK_SETTLE_PRESENTS 3ULL
/* The acquire waits at most this long for the other displays to settle, then tries anyway (named). */
#define DRM_OUTPUT_VK_SETTLE_WAIT_MS 2000

struct drm_output_vk_shared {
	VkImage image;
	VkDeviceMemory memory;
	VkDeviceSize size;
	VkSemaphore sem; /* GL signals (publish), the present thread waits (first copy of a publish) */
	GLuint gl_mem;
	GLuint gl_tex;
	GLuint gl_sem;
};

/* The shared part: the loader, the X connection, the VkInstance + VkPhysicalDevice + VkDevice every
 * display uses, one queue per display slot (the family's queueCount permitting; a display whose slot has
 * no queue of its own shares queue 0, serialised by queue_lock), and the GL interop entry points of the
 * OBS context. Opened with the first display, freed with the last (`refs`), all under `lock`. */
struct drm_output_vk_device {
	pthread_mutex_t lock;
	int refs;     /* open displays */
	bool wedged;  /* a display's teardown leaked its objects on a wedged GPU: never destroy the device */
	void *lib;
	struct drm_output_vk_fns vk;
	Display *dpy;
	VkInstance instance;
	VkPhysicalDevice pd; /* the GPU of the first display; a display on another GPU is refused */
	VkDevice device;
	uint32_t qfi;
	uint32_t n_queues;
	VkQueue queues[DRM_OUTPUT_VK_MAX_DISPLAYS];
	pthread_mutex_t queue_lock[DRM_OUTPUT_VK_MAX_DISPLAYS]; /* host access to queues[i] (submit, present, wait) */
	struct drm_output_vk_gl gl;
	bool gl_resolved; /* graphics thread only */
	/* issue 1424: a display is being acquired (os_atomic, set under `lock`); a re-mode on another display
	 * waits for the next vblank instead of starting a new mode set inside the acquire. */
	volatile bool acquiring;
};

/* One display: its X output, VkDisplayKHR, surface, swapchain, shared images, mailbox roles and present
 * thread. The slot array is static; a slot is taken at open and given back at close. */
struct drm_output_vk_disp {
	struct drm_output_vk_device *dev;
	int slot;
	bool in_use; /* the slot is taken (g_drm_vk_dev.lock) */

	pthread_mutex_t lock; /* the mailbox roles */
	bool open;
	volatile bool running;     /* present-thread run flag (os_atomic) */
	volatile bool want_frames; /* the frame hook's fast gate (os_atomic) */
	bool thread_started;
	pthread_t thread;

	char output_name[64];
	uint32_t solid_argb;
	drm_output_vk_shown_fn shown; /* issue 1424: the present-delay note of the owning output */
	void *shown_param;

	VkDisplayKHR display;
	bool acquired;
	VkSurfaceKHR surface;
	int plane;                  /* the display plane it presents on (-1 none; one plane per display) */
	VkQueue queue;              /* the device queue this display submits + presents on */
	pthread_mutex_t *queue_lock; /* that queue's host-access lock (in the device) */
	VkSwapchainKHR swapchain;
	VkSwapchainKHR retired_swapchain; /* replaced by a rebuild, freed later (see RETIRE_FRAMES) */
	unsigned retire_countdown;        /* signalled fences left before the retired objects go */
	bool present_queued_since_rebuild; /* vkQueuePresentKHR was called on the current swapchain */
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
	/* camera-box issue 1424: the mode the graphics thread wants (the Program canvas; 0 = today's rule)
	 * and the flag the present thread re-modes on (os_atomic, graphics -> present thread). */
	volatile long want_w;
	volatile long want_h;
	volatile long want_mhz;
	volatile bool remode;

	struct drm_output_vk_shared shared[DRM_OUTPUT_VK_SHARED_IMAGES];
	int front;   /* image last presented (re-copied when nothing new), -1 before the first */
	int pending; /* image taken by the present thread, copy in flight */
	int ready;   /* newest published image not yet taken */
	/* A submit whose fence was never seen signalled (present thread; read by the teardown quiesce). */
	bool submit_outstanding;
	unsigned long long gl_skips; /* GL claims refused while a READY image was waiting (d->lock) */

	bool gl_bound; /* graphics thread only */

	unsigned long long presents;
	unsigned long long program_presents;
	bool scanout_live_logged;
	/* issue 1424: successful presents on the CURRENT surface (os_atomic: the present thread counts, a new
	 * surface restarts it at 0, another display's open reads it -- see DRM_OUTPUT_VK_SETTLE_PRESENTS), and
	 * the current swapchain's image count the settle threshold follows. */
	volatile long presents_since_surface;
	volatile long settle_images;
	/* issue 1424 review: a rebuild's bounded drain timed out (present thread + teardown): the empty
	 * submit's fence may still be pending, so no rebuild retries (it would reset/re-submit that fence). */
	bool queue_wedged;
};

/* The shared device and the display slots (defined in obs-drm-output-vk.c). */
extern struct drm_output_vk_device g_drm_vk_dev;
extern struct drm_output_vk_disp g_drm_vk_disp[DRM_OUTPUT_VK_MAX_DISPLAYS];

/* obs-drm-output-vk-setup.c, caller holds g_drm_vk_dev.lock: open the X display, load Vulkan (the first
 * display only), acquire output `d->output_name` and build its surface (and the device with the first
 * display), its swapchain and its shared images. false = nothing usable (the caller runs
 * drm_output_vk_destroy_display). */
bool drm_output_vk_setup(struct drm_output_vk_disp *d);

/* obs-drm-output-vk-setup.c, caller holds g_drm_vk_dev.lock: free everything setup built for display `d`
 * (idempotent, safe on partial init) and release it; the shared device goes when no display holds it
 * (dev->refs == 0). The present thread must be halted. An outstanding submit is waited BOUNDED first; a
 * GPU that never finishes it leaks the display's Vulkan objects (loudly) and marks the device wedged
 * instead of hanging the OBS shutdown. */
void drm_output_vk_destroy_display(struct drm_output_vk_disp *d);
/* Drain the display's queue BOUNDED (5 x 1 s) before its teardown; false = it never drained (leak). */
bool drm_output_vk_drain_queue_bounded(struct drm_output_vk_disp *d);

/* issue 1424 (obs-drm-output-vk-setup.c, shared with the remode TU): the display's modes as the picker
 * reads them, and the pick for the wanted mode the graphics thread handed over (0/0/0 = the issue-1346
 * rule). One enumeration for the surface setup and the re-mode. */
struct drm_output_vk_mode_list {
	VkExtent2D native;
	uint32_t n;
	VkDisplayModePropertiesKHR *modes;
	uint32_t *w, *h, *r;
	uint32_t want_w, want_h, want_mhz;
	int best;
};
bool drm_output_vk_mode_list_read(struct drm_output_vk_disp *d, struct drm_output_vk_mode_list *ml);
void drm_output_vk_mode_list_free(struct drm_output_vk_mode_list *ml);
bool drm_output_vk_create_surface(struct drm_output_vk_disp *d, bool rebuild);
bool drm_output_vk_create_swapchain_only(struct drm_output_vk_disp *d, VkSwapchainKHR old_swapchain);
void drm_output_vk_destroy_swapchain(struct drm_output_vk_disp *d);
/* obs-drm-output-vk-remode.c: the open's bounded settle wait before its acquire (issue 1424 part 2). */
void drm_output_vk_wait_others_settled(struct drm_output_vk_disp *d);

/* obs-drm-output-vk-setup.c, present thread only: rebuild the swapchain (and the display-plane surface
 * too when `surface_lost`) after VK_ERROR_OUT_OF_DATE_KHR / VK_ERROR_SURFACE_LOST_KHR, e.g. an HDMI
 * replug. false = the display now offers a different mode size (the shared images no longer fit) or a
 * Vulkan call failed; the caller retries a bounded number of times, then gives up. */
bool drm_output_vk_rebuild_presentation(struct drm_output_vk_disp *d, bool surface_lost);

/* camera-box issue 1424, obs-drm-output-vk-setup.c, present thread: re-mode onto the wanted mode when
 * flagged and the pick changed (same size only). false = the surface rebuild failed. */
bool drm_output_vk_remode_if_needed(struct drm_output_vk_disp *d);

/* obs-drm-output-vk-setup.c, present thread (after a signalled fence) or teardown: free a retired
 * swapchain + surface (safe when none). */
void drm_output_vk_free_retired(struct drm_output_vk_disp *d);
