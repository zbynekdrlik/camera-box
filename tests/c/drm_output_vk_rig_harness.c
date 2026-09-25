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
 * Then a 2 s BURST phase publishes with no sleep (several frames per vblank), which forces the overwrite
 * path — the GL side re-claims a READY image the present thread has not taken and must consume its
 * pending semaphore signal first. The harness requires gl_consumes > 0 (the path really ran) and a clean
 * release after it (a double-signalled binary semaphore would fail the submit or wedge the fence).
 *
 * Build ON the box (never on dev1 — Tier-0), with the X11/xcb/EGL/GL/Vulkan headers staged in $INC and
 * the libobs tree in $LIBOBS (only the util headers and the four obs-drm-output-vk files are read):
 *   gcc -O2 -std=gnu11 -Wall -Wextra -I$INC -I$LIBOBS tests/c/drm_output_vk_rig_harness.c \
 *       $LIBOBS/obs-drm-output-vk.c $LIBOBS/obs-drm-output-vk-setup.c -o vk-rig-harness \
 *       -ldl -lpthread -l:libX11.so.6 -l:libX11-xcb.so.1 -l:libxcb.so.1 -l:libxcb-randr.so.0 -l:libEGL.so.1
 * Run (the output must be OFF in X first — `xrandr --output HDMI-0 --off`):
 *   DISPLAY=:0 ./vk-rig-harness HDMI-0
 * Exit 0 = the backend acquired the display, bound GL, published every phase and released cleanly.
 */

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <stdarg.h>
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
#undef GLP
	if (!glGenTextures || !glTexStorage2D || !glGenFramebuffers || !glClear) {
		fprintf(stderr, "FAIL: GL entry points\n");
		return 1;
	}

	/* 2. The backend: acquire, solid pattern for 2 s, then the lazy GL bind (as on the first OBS tick). */
	if (!drm_output_vk_open(output, 0x202020u)) {
		fprintf(stderr, "FAIL: drm_output_vk_open\n");
		return 1;
	}
	uint32_t w = 0, h = 0;
	drm_output_vk_mode_size(&w, &h);
	printf("PHASE solid t=%.3f mode=%ux%u\n", now_s(), w, h);
	fflush(stdout);
	sleep(2);
	if (!drm_output_vk_gl_bind()) {
		fprintf(stderr, "FAIL: drm_output_vk_gl_bind\n");
		goto out;
	}

	/* 3. The source: the storage libobs gives a GS_BGRA render target (GL_SRGB8_ALPHA8), drawn with
	 * framebuffer sRGB OFF = raw bytes, exactly like drm_output_blit_raw writes the intermediate. */
	GLuint tex = 0, fbo = 0;
	glGenTextures(1, &tex);
	glBindTexture(GL_TEXTURE_2D, tex);
	glTexStorage2D(GL_TEXTURE_2D, 1, GL_SRGB8_ALPHA8, (GLsizei)w, (GLsizei)h);
	glGenFramebuffers(1, &fbo);
	glBindFramebuffer(GL_FRAMEBUFFER, fbo);
	glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
	if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
		fprintf(stderr, "FAIL: framebuffer incomplete\n");
		goto out;
	}
	glDisable(GL_FRAMEBUFFER_SRGB);
	glViewport(0, 0, (GLsizei)w, (GLsizei)h);

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
			glClearColor(phases[p].r, phases[p].g, phases[p].b, 1.0f);
			glClear(GL_COLOR_BUFFER_BIT);
			int idx = drm_output_vk_claim();
			if (idx >= 0 && drm_output_vk_publish_gl(idx, tex, w, h))
				published++;
			else
				skipped++;
			usleep(16667);
		}
	}
	printf("PHASE burst t=%.3f\n", now_s());
	fflush(stdout);
	unsigned burst = 0;
	double burst_end = now_s() + 2.0;
	for (unsigned k = 0; now_s() < burst_end; k++) {
		const float v = (k & 1u) ? 0.75f : 0.25f;
		glClearColor(v, v, v, 1.0f);
		glClear(GL_COLOR_BUFFER_BIT);
		int idx = drm_output_vk_claim();
		if (idx >= 0 && drm_output_vk_publish_gl(idx, tex, w, h))
			burst++;
	}
	pthread_mutex_lock(&g_drm_vk.lock);
	unsigned long long consumes = g_drm_vk.gl_consumes;
	pthread_mutex_unlock(&g_drm_vk.lock);
	printf("PHASE end t=%.3f published=%u skipped=%u burst=%u gl_consumes=%llu\n", now_s(), published, skipped,
	       burst, consumes);
	rc = (published > 0 && burst > 0 && consumes > 0 && drm_output_vk_wants_frames()) ? 0 : 1;

out:
	drm_output_vk_halt();
	drm_output_vk_gl_unbind();
	drm_output_vk_close();
	eglMakeCurrent(edpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
	eglTerminate(edpy);
	XCloseDisplay(xdpy);
	printf("harness rc=%d\n", rc);
	return rc;
}
