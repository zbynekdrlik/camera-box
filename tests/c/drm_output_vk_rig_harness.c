/*
 * camera-box issue 1346 — rig harness for the vk-direct HDMI backend: runs the REAL vendored core
 * (vendor/obs-studio/libobs/obs-drm-output-vk.c + obs-drm-output-vk-setup.c) OUTSIDE OBS, with its own
 * EGL/GL 3.3 core context standing in for the OBS graphics context, on the box whose HDMI it drives.
 *
 * Why it exists: the libobs build is compiled first by CI and cannot be run anywhere but a live OBS,
 * and the whole GPU path — VK_EXT_acquire_xlib_display, the display-plane FIFO swapchain, the
 * GL_EXT_memory_object_fd import, the GL_EXT_semaphore_fd sync, the RGBA8 -> BGRA8 present blit — only
 * exists on the NVIDIA box. This harness proves that path end to end on the rig, byte-exact, before a
 * bundle is deployed (and again on every new NVIDIA strih box, e.g. strih PP).
 *
 * It publishes solid phases the way the OBS glue does (claim -> copy from a GL_SRGB8_ALPHA8 texture,
 * the GS_BGRA storage libobs uses, with framebuffer sRGB OFF -> publish), 60 times a second:
 *   2 s solid pattern (before any publish) -> 3 s RED -> 3 s GREEN -> 3 s BLUE -> 3 s GREY 50 %
 * and prints the wall-clock start of each phase, so a capture of the HDMI (cam2 on the SNV rig) can be
 * matched phase by phase: the R/G/B order proves the channel mapping, the grey proves no gamma step.
 * Then a 2 s BURST phase tries to publish with no sleep (many claims per vblank): the GL side must SKIP
 * every claim while a READY image waits for the present thread (never overwrite it -- every GL signal is
 * waited by Vulkan exactly once). The harness requires gl_skips > 0 (the path really ran), a present loop
 * still alive after the burst, and a clean release (a double-signalled or never-signalled binary
 * semaphore would wedge the copy fence and force the teardown's leak path).
 *
 * Build ON the box (never on dev1 — Tier-0), with the X11/xcb/EGL/GL/Vulkan headers staged in $INC and
 * the libobs tree in $LIBOBS (only the util headers and the four obs-drm-output-vk files are read):
 *   gcc -O2 -std=gnu11 -Wall -Wextra -I$INC -I$LIBOBS tests/c/drm_output_vk_rig_harness.c \
 *       $LIBOBS/obs-drm-output-vk.c $LIBOBS/obs-drm-output-vk-setup.c -o vk-rig-harness \
 *       -ldl -lpthread -l:libX11.so.6 -l:libX11-xcb.so.1 -l:libxcb.so.1 -l:libxcb-randr.so.0 -l:libEGL.so.1
 * Run (each output must be OFF in X first — `xrandr --output HDMI-0 --off`):
 *   DISPLAY=:0 ./vk-rig-harness HDMI-0
 * issue 1424 part 2: name two outputs to prove both displays on ONE shared VkDevice at once (each its
 * own swapchain + present thread; strih-pp: the Multiview monitor + the vestibule HDMI-to-SDI):
 *   DISPLAY=:0 ./vk-rig-harness HDMI-0 HDMI-1
 * With two outputs it then closes the first while the second keeps presenting (the second's present
 * count must advance, its loop stay alive) and re-opens the first in the same slot beside it: the two
 * outputs never depend on each other.
 * Exit 0 = the backend acquired the display, bound GL, published every phase and released cleanly.
 */

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include <X11/Xlib.h>

#include "obs-drm-output-vk-internal.h"

/* libobs's logger, for the standalone build. */
void blog(int log_level, const char *format, ...)
{
	va_list ap;
	va_start(ap, format);
	fprintf(stderr, "[%d] ", log_level);
	vfprintf(stderr, format, ap);
	fputc('\n', stderr);
	va_end(ap);
}

/* libobs's monotonic clock, for the standalone build (the core stamps each present for its shown
 * callback, issue 1424). */
uint64_t os_gettime_ns(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return (uint64_t)t.tv_sec * 1000000000ULL + (uint64_t)t.tv_nsec;
}

static double now_s(void)
{
	struct timespec t;
	clock_gettime(CLOCK_REALTIME, &t);
	return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

typedef void(APIENTRYP gen_textures_fn)(GLsizei, GLuint *);
typedef void(APIENTRYP bind_texture_fn)(GLenum, GLuint);
typedef void(APIENTRYP tex_storage_2d_fn)(GLenum, GLsizei, GLenum, GLsizei, GLsizei);
typedef void(APIENTRYP gen_framebuffers_fn)(GLsizei, GLuint *);
typedef void(APIENTRYP bind_framebuffer_fn)(GLenum, GLuint);
typedef void(APIENTRYP framebuffer_texture_2d_fn)(GLenum, GLenum, GLenum, GLuint, GLint);
typedef GLenum(APIENTRYP check_framebuffer_fn)(GLenum);
typedef void(APIENTRYP clear_color_fn)(GLfloat, GLfloat, GLfloat, GLfloat);
typedef void(APIENTRYP clear_fn)(GLbitfield);
typedef void(APIENTRYP disable_fn)(GLenum);
typedef void(APIENTRYP viewport_fn)(GLint, GLint, GLsizei, GLsizei);
typedef void(APIENTRYP finish_fn)(void);

int main(int argc, char **argv)
{
	const char *output = argc > 1 ? argv[1] : "HDMI-0";
	int rc = 1;

	/* 1. An EGL/X11 GL 3.3 core context, like OBS's ("Using EGL/X11", GL 3.3.0 NVIDIA). */
	Display *xdpy = XOpenDisplay(NULL);
	if (!xdpy) {
		fprintf(stderr, "FAIL: no X display\n");
		return 1;
	}
	EGLDisplay edpy = eglGetDisplay((EGLNativeDisplayType)xdpy);
	if (edpy == EGL_NO_DISPLAY || !eglInitialize(edpy, NULL, NULL) || !eglBindAPI(EGL_OPENGL_API)) {
		fprintf(stderr, "FAIL: EGL init\n");
		return 1;
	}
	const EGLint cfg_attr[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
				   EGL_RED_SIZE,     8,               EGL_GREEN_SIZE,      8,
				   EGL_BLUE_SIZE,    8,               EGL_NONE};
	EGLConfig cfg;
	EGLint ncfg = 0;
	if (!eglChooseConfig(edpy, cfg_attr, &cfg, 1, &ncfg) || ncfg < 1) {
		fprintf(stderr, "FAIL: no EGL config\n");
		return 1;
	}
	const EGLint ctx_attr[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 3,
				   EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
	EGLContext ctx = eglCreateContext(edpy, cfg, EGL_NO_CONTEXT, ctx_attr);
	const EGLint pb_attr[] = {EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE};
	EGLSurface pb = eglCreatePbufferSurface(edpy, cfg, pb_attr);
	if (ctx == EGL_NO_CONTEXT || pb == EGL_NO_SURFACE || !eglMakeCurrent(edpy, pb, pb, ctx)) {
		fprintf(stderr, "FAIL: EGL context\n");
		return 1;
	}

#define GLP(type, name) type name = (type)eglGetProcAddress(#name)
	GLP(gen_textures_fn, glGenTextures);
	GLP(bind_texture_fn, glBindTexture);
	GLP(tex_storage_2d_fn, glTexStorage2D);
	GLP(gen_framebuffers_fn, glGenFramebuffers);
	GLP(bind_framebuffer_fn, glBindFramebuffer);
	GLP(framebuffer_texture_2d_fn, glFramebufferTexture2D);
	GLP(check_framebuffer_fn, glCheckFramebufferStatus);
	GLP(clear_color_fn, glClearColor);
	GLP(clear_fn, glClear);
	GLP(disable_fn, glDisable);
	GLP(viewport_fn, glViewport);
	GLP(finish_fn, glFinish);
#undef GLP
	if (!glGenTextures || !glTexStorage2D || !glGenFramebuffers || !glClear) {
		fprintf(stderr, "FAIL: GL entry points\n");
		return 1;
	}

	/* 2. The backend: acquire every named output (issue 1424 part 2: up to DRM_OUTPUT_VK_MAX_DISPLAYS at
	 * once on ONE shared VkDevice), solid pattern for 2 s, then the lazy GL bind (as on the first OBS
	 * tick). */
	struct drm_output_vk_disp *disp[DRM_OUTPUT_VK_MAX_DISPLAYS] = {NULL};
	uint32_t w[DRM_OUTPUT_VK_MAX_DISPLAYS] = {0}, h[DRM_OUTPUT_VK_MAX_DISPLAYS] = {0};
	GLuint tex[DRM_OUTPUT_VK_MAX_DISPLAYS] = {0}, fbo[DRM_OUTPUT_VK_MAX_DISPLAYS] = {0};
	/* issue 1424: an argument may carry the mode the OBS frame hook hands over for the Program view,
	 * `HDMI-1=1920x1080@30000` (mHz): the output opens with the 1346 rule, then re-modes like in OBS. */
	char names[DRM_OUTPUT_VK_MAX_DISPLAYS][64] = {{0}};
	uint32_t want_w[DRM_OUTPUT_VK_MAX_DISPLAYS] = {0}, want_h[DRM_OUTPUT_VK_MAX_DISPLAYS] = {0},
		 want_mhz[DRM_OUTPUT_VK_MAX_DISPLAYS] = {0};
	int n_disp = 0;
	for (int a = 1; a < argc && n_disp < DRM_OUTPUT_VK_MAX_DISPLAYS; a++) {
		snprintf(names[n_disp], sizeof(names[n_disp]), "%s", argv[a]);
		char *eq = strchr(names[n_disp], '=');
		if (eq) {
			*eq = '\0';
			if (sscanf(eq + 1, "%ux%u@%u", &want_w[n_disp], &want_h[n_disp], &want_mhz[n_disp]) != 3) {
				fprintf(stderr, "FAIL: bad wanted mode '%s' (WxH@mHz)\n", eq + 1);
				goto out;
			}
		}
		disp[n_disp] = drm_output_vk_open(n_disp, names[n_disp], 0x202020u, NULL, NULL);
		if (!disp[n_disp]) {
			fprintf(stderr, "FAIL: drm_output_vk_open(%s)\n", names[n_disp]);
			goto out;
		}
		drm_output_vk_mode_size(disp[n_disp], &w[n_disp], &h[n_disp]);
		printf("PHASE solid t=%.3f output=%s mode=%ux%u\n", now_s(), names[n_disp], w[n_disp], h[n_disp]);
		n_disp++;
	}
	if (n_disp == 0) {
		disp[0] = drm_output_vk_open(0, output, 0x202020u, NULL, NULL);
		if (!disp[0]) {
			fprintf(stderr, "FAIL: drm_output_vk_open(%s)\n", output);
			goto out;
		}
		drm_output_vk_mode_size(disp[0], &w[0], &h[0]);
		printf("PHASE solid t=%.3f output=%s mode=%ux%u\n", now_s(), output, w[0], h[0]);
		n_disp = 1;
	}
	fflush(stdout);
	sleep(2);
	for (int i = 0; i < n_disp; i++) {
		if (want_mhz[i] == 0)
			continue;
		drm_output_vk_want_mode(disp[i], want_w[i], want_h[i], want_mhz[i]);
		printf("PHASE want-mode t=%.3f output=%s want=%ux%u@%u\n", now_s(), names[i], want_w[i], want_h[i],
		       want_mhz[i]);
	}
	fflush(stdout);
	for (int i = 0; i < n_disp; i++) {
		if (!drm_output_vk_gl_bind(disp[i])) {
			fprintf(stderr, "FAIL: drm_output_vk_gl_bind (display %d)\n", i);
			goto out;
		}
	}

	/* 3. The source per display: the storage libobs gives a GS_BGRA render target (GL_SRGB8_ALPHA8),
	 * drawn with framebuffer sRGB OFF = raw bytes, exactly like drm_output_blit_raw writes the
	 * intermediate. */
	for (int i = 0; i < n_disp; i++) {
		glGenTextures(1, &tex[i]);
		glBindTexture(GL_TEXTURE_2D, tex[i]);
		glTexStorage2D(GL_TEXTURE_2D, 1, GL_SRGB8_ALPHA8, (GLsizei)w[i], (GLsizei)h[i]);
		glGenFramebuffers(1, &fbo[i]);
		glBindFramebuffer(GL_FRAMEBUFFER, fbo[i]);
		glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex[i], 0);
		if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
			fprintf(stderr, "FAIL: framebuffer incomplete (display %d)\n", i);
			goto out;
		}
	}
	glDisable(GL_FRAMEBUFFER_SRGB);

	static const struct {
		const char *name;
		float r, g, b;
	} phases[] = {{"red", 1, 0, 0}, {"green", 0, 1, 0}, {"blue", 0, 0, 1}, {"grey50", 0.5f, 0.5f, 0.5f}};
	unsigned published = 0, skipped = 0;
	for (size_t p = 0; p < sizeof(phases) / sizeof(phases[0]); p++) {
		printf("PHASE %s t=%.3f\n", phases[p].name, now_s());
		fflush(stdout);
		double end = now_s() + 3.0;
		while (now_s() < end) {
			for (int i = 0; i < n_disp; i++) {
				glBindFramebuffer(GL_FRAMEBUFFER, fbo[i]);
				glViewport(0, 0, (GLsizei)w[i], (GLsizei)h[i]);
				glClearColor(phases[p].r, phases[p].g, phases[p].b, 1.0f);
				glClear(GL_COLOR_BUFFER_BIT);
				int idx = drm_output_vk_claim(disp[i]);
				if (idx >= 0 && drm_output_vk_publish_gl(disp[i], idx, tex[i], w[i], h[i]))
					published++;
				else
					skipped++;
			}
			usleep(16667);
		}
	}
	printf("PHASE burst t=%.3f\n", now_s());
	fflush(stdout);
	unsigned burst = 0;
	double burst_end = now_s() + 2.0;
	for (unsigned k = 0; now_s() < burst_end; k++) {
		/* claim FIRST, like the OBS frame hook: a refused claim renders nothing (no GL backlog) */
		bool any = false;
		for (int i = 0; i < n_disp; i++) {
			int idx = drm_output_vk_claim(disp[i]);
			if (idx < 0)
				continue;
			any = true;
			const float v = (k & 1u) ? 0.75f : 0.25f;
			glBindFramebuffer(GL_FRAMEBUFFER, fbo[i]);
			glViewport(0, 0, (GLsizei)w[i], (GLsizei)h[i]);
			glClearColor(v, v, v, 1.0f);
			glClear(GL_COLOR_BUFFER_BIT);
			if (drm_output_vk_publish_gl(disp[i], idx, tex[i], w[i], h[i]))
				burst++;
		}
		if (!any)
			usleep(200);
	}
	/* Drain the GL work of the burst, then let the present loops run on for 2 s -- they must still be
	 * alive (no fence wedge, no semaphore misuse) and the release below must not need the leak path. */
	glFinish();
	sleep(2);
	unsigned long long skips = 0;
	bool alive = true;
	for (int i = 0; i < n_disp; i++) {
		pthread_mutex_lock(&disp[i]->lock);
		skips += disp[i]->gl_skips;
		pthread_mutex_unlock(&disp[i]->lock);
		alive = alive && drm_output_vk_wants_frames(disp[i]);
	}
	printf("PHASE end t=%.3f displays=%d published=%u skipped=%u burst=%u gl_skips=%llu\n", now_s(), n_disp,
	       published, skipped, burst, skips);
	/* issue 1424: a wanted mode must be the one on scanout now, within the picker's 0.2 % (29.970 for a
	 * 30.000 canvas), with the loop still presenting on it */
	bool modes_ok = true;
	for (int i = 0; i < n_disp; i++) {
		const uint32_t hz = disp[i]->refresh_mhz;
		const bool on_want = want_mhz[i] == 0 ||
				     (disp[i]->mode_w == want_w[i] && disp[i]->mode_h == want_h[i] &&
				      (hz > want_mhz[i] ? hz - want_mhz[i] : want_mhz[i] - hz) <= want_mhz[i] / 500u);
		modes_ok = modes_ok && on_want;
		printf("MODE output=%s scanout=%ux%u@%u.%03uHz presents=%llu wanted=%s\n", names[i], disp[i]->mode_w,
		       disp[i]->mode_h, hz / 1000u, hz % 1000u, disp[i]->presents,
		       want_mhz[i] == 0 ? "none" : (on_want ? "yes" : "NO"));
	}
	rc = (published > 0 && burst > 0 && skips > 0 && alive && modes_ok) ? 0 : 1;
	printf("present loop(s) alive after the drain: %s\n", alive ? "yes" : "NO");

	/* 4. issue 1424 review: the two outputs are independent -- close display 0 while display 1 keeps
	 * presenting, then re-open display 0 in its slot beside the live display 1. */
	if (rc == 0 && n_disp == 2) {
		drm_output_vk_halt(disp[0]);
		drm_output_vk_gl_unbind(disp[0]);
		drm_output_vk_close(disp[0]);
		disp[0] = NULL;
		pthread_mutex_lock(&disp[1]->lock);
		const unsigned long long before = disp[1]->presents;
		pthread_mutex_unlock(&disp[1]->lock);
		double one_end = now_s() + 2.0;
		while (now_s() < one_end) {
			int idx = drm_output_vk_claim(disp[1]);
			if (idx >= 0)
				drm_output_vk_publish_gl(disp[1], idx, tex[1], w[1], h[1]);
			usleep(16667);
		}
		pthread_mutex_lock(&disp[1]->lock);
		const unsigned long long after = disp[1]->presents;
		pthread_mutex_unlock(&disp[1]->lock);
		const bool one_alive = drm_output_vk_wants_frames(disp[1]) && after > before + 60u;
		disp[0] = drm_output_vk_open(0, names[0], 0x202020u, NULL, NULL);
		const bool reopened = disp[0] && drm_output_vk_gl_bind(disp[0]) && drm_output_vk_wants_frames(disp[0]) &&
				      drm_output_vk_wants_frames(disp[1]);
		printf("PHASE independent t=%.3f display1_presents=%llu->%llu alive=%s reopened=%s\n", now_s(), before,
		       after, one_alive ? "yes" : "NO", reopened ? "yes" : "NO");
		rc = (one_alive && reopened) ? 0 : 1;
	}

out:
	for (int i = 0; i < DRM_OUTPUT_VK_MAX_DISPLAYS; i++) {
		if (!disp[i])
			continue;
		drm_output_vk_halt(disp[i]);
		drm_output_vk_gl_unbind(disp[i]);
		drm_output_vk_close(disp[i]);
	}
	eglMakeCurrent(edpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
	eglTerminate(edpy);
	XCloseDisplay(xdpy);
	printf("harness rc=%d\n", rc);
	return rc;
}
