#pragma once

/*
 * camera-box issue 1424 (design 6386467303 layer 3) -- the DRM output module's ONE log entry.
 *
 * Every TU of the module includes this header through obs-drm-output-internal.h or
 * obs-drm-output-vk-internal.h, and each of those two headers turns `blog` into drm_output_blog
 * (`#define blog drm_output_blog` right after including this one). On any thread it is plain
 * blog (blogva). On the retry supervisor's thread, while it attempts a dormant output's start, the
 * lines are CAPTURED instead (obs-drm-output-retry.c): a failed attempt every 2-5 s must not write its
 * dozen open-path lines each time, so only a change of the failure reason is logged, and a successful
 * attempt flushes its captured lines (acquired, mode, swapchain, ACTIVE ...) in order.
 *
 * The rig harness (tests/c/drm_output_vk_rig_harness.c) includes the vk internal header, so its own
 * `blog` definition becomes the drm_output_blog definition: it links without the retry TU.
 */

#include "util/base.h"

#ifdef __cplusplus
extern "C" {
#endif

#if defined(__GNUC__)
__attribute__((__format__(__printf__, 2, 3)))
#endif
void drm_output_blog(int log_level, const char *format, ...);

#ifdef __cplusplus
}
#endif

