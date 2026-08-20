/*
 * camera-box #1152 M1 — in-OBS vendored DRM-lease output: lease acquire + solid-color flip.
 *
 * See obs-drm-output.h for the design. Owner KOREKCIA (2026-08-20): the imag HDMI Program output
 * must leave the Xorg desktop. Our forked OBS acquires DRM master of the HDMI connector through an
 * X RandR output LEASE (xcb_randr_create_lease) and page-flips onto it DIRECTLY (drmModePageFlip),
 * render->scanout — no NDI hop, no external presenter. This M1 file proves the mechanism with a
 * solid-color double buffer; it is NOT yet bound to the Program render texture (M2 — see M2 HOOK).
 *
 * This whole translation unit is Linux-only and is built only via libobs/cmake/os-linux.cmake.
 * The lift-compilable pure helper `drm_output_pick_free_crtc` is guarded by
 * tests/drm_output_lease_1152.rs (source anchors + a cc-compiled truth table); the xcb/drm glue is
 * compiled first by linux-genlock.yml.
 */

#if defined(__linux__)

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <xcb/randr.h>
#include <xcb/xcb.h>

#include <drm_mode.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#include "obs.h"
#include "obs-drm-output.h"
#include "util/threading.h"

/* -------------------------------------------------------------------------------------------------
 * Pure decision helper (Tier-0, lift-compiled + truth-table-tested by tests/drm_output_lease_1152.rs).
 *
 * Given `busy_mask` — bit i set = the i-th CANDIDATE CRTC of the target output is currently in use
 * (driving some output) — and `n` candidate CRTCs, return the index of the FIRST free candidate, or
 * -1 if none is free. This chooses which CRTC XID to include in the lease request so we never lease
 * a CRTC X is actively displaying. Kept pure (no HW, no globals) so it is unit-testable off-rig.
 * ------------------------------------------------------------------------------------------------- */
static int drm_output_pick_free_crtc(uint32_t busy_mask, int n)
{
	for (int i = 0; i < n && i < 32; i++) {
		if (((busy_mask >> i) & 1u) == 0u)
			return i;
	}
	return -1;
}

/* -------------------------------------------------------------------------------------------------
 * Module state (single instance; M1 drives one HDMI connector).
 * ------------------------------------------------------------------------------------------------- */
#define DRM_OUTPUT_BUFFERS 2

struct drm_output_buffer {
	uint32_t handle; /* GEM handle of the dumb BO */
	uint32_t fb_id;  /* drmModeAddFB framebuffer id */
	uint32_t pitch;  /* bytes per scanline */
	uint64_t size;   /* mmap size */
	void *map;       /* mmap'd pixels */
};

static struct {
	pthread_mutex_t lock;
	bool active;   /* lease held + flip thread running (guarded by lock) */
	bool stopping; /* a stop is mid-flight (join in progress) — guarded by lock */
	volatile bool running; /* flip thread run flag (os_atomic_{set,load}_bool) */
	pthread_t thread;

	xcb_connection_t *conn;
	xcb_randr_lease_t lease;
	bool have_lease;

	int drm_fd; /* DRM master fd of the leased objects (from the lease reply) */
	uint32_t crtc_id;
	uint32_t connector_id;
	drmModeModeInfo mode;

	struct drm_output_buffer buffers[DRM_OUTPUT_BUFFERS];
	unsigned long long flips;
} g_drm = {
	.lock = PTHREAD_MUTEX_INITIALIZER,
	.drm_fd = -1,
	.lease = 0,
};

/* Page-flip completion: the handler clears the pending flag the flip loop waits on. */
static void drm_output_page_flip_handler(int fd, unsigned int seq, unsigned int tv_sec,
					 unsigned int tv_usec, void *user)
{
	(void)fd;
	(void)seq;
	(void)tv_sec;
	(void)tv_usec;
	if (user)
		*(volatile int *)user = 0;
}

/* Fill a mapped XRGB8888 buffer with a solid 0x00RRGGBB colour, respecting pitch. */
static void drm_output_fill_solid(struct drm_output_buffer *buf, uint32_t argb, uint32_t height)
{
	for (uint32_t y = 0; y < height; y++) {
		uint32_t *row = (uint32_t *)((uint8_t *)buf->map + (size_t)y * (size_t)buf->pitch);
		uint32_t px = buf->pitch / 4u;
		for (uint32_t x = 0; x < px; x++)
			row[x] = argb;
	}
}

/* Acquire the RandR output lease for `connector_name`. On success fills g_drm.conn/lease/drm_fd
 * and returns true; on failure cleans up whatever it opened and returns false. */
static bool drm_output_acquire_lease(const char *connector_name)
{
	int screen_num = 0;
	xcb_connection_t *conn = xcb_connect(NULL, &screen_num);
	if (!conn || xcb_connection_has_error(conn)) {
		blog(LOG_WARNING, "drm-output: cannot connect to X (DISPLAY) — lease not attempted");
		if (conn)
			xcb_disconnect(conn);
		return false;
	}

	const xcb_setup_t *setup = xcb_get_setup(conn);
	xcb_screen_iterator_t it = xcb_setup_roots_iterator(setup);
	for (int i = 0; i < screen_num && it.rem; i++)
		xcb_screen_next(&it);
	if (!it.data) {
		blog(LOG_WARNING, "drm-output: no X screen found — lease not attempted");
		xcb_disconnect(conn);
		return false;
	}
	xcb_window_t root = it.data->root;

	xcb_randr_get_screen_resources_current_reply_t *res =
		xcb_randr_get_screen_resources_current_reply(
			conn, xcb_randr_get_screen_resources_current(conn, root), NULL);
	if (!res) {
		blog(LOG_WARNING, "drm-output: RandR get_screen_resources_current failed");
		xcb_disconnect(conn);
		return false;
	}
	xcb_timestamp_t cfg_ts = res->config_timestamp;
	xcb_randr_output_t *outputs = xcb_randr_get_screen_resources_current_outputs(res);
	int n_out = xcb_randr_get_screen_resources_current_outputs_length(res);

	xcb_randr_output_t target_output = 0;
	xcb_randr_crtc_t crtc_xid = 0;
	size_t name_len = strlen(connector_name);

	for (int i = 0; i < n_out && target_output == 0; i++) {
		xcb_randr_get_output_info_reply_t *oi = xcb_randr_get_output_info_reply(
			conn, xcb_randr_get_output_info(conn, outputs[i], cfg_ts), NULL);
		if (!oi)
			continue;

		uint8_t *nm = xcb_randr_get_output_info_name(oi);
		int nlen = xcb_randr_get_output_info_name_length(oi);
		bool match = (nlen >= 0 && (size_t)nlen == name_len &&
			      memcmp(nm, connector_name, name_len) == 0);
		if (match) {
			xcb_randr_crtc_t *cands = xcb_randr_get_output_info_crtcs(oi);
			int n_cand = xcb_randr_get_output_info_crtcs_length(oi);
			if (n_cand > 32)
				n_cand = 32;

			/* Build the busy mask over the output's candidate CRTCs. */
			uint32_t busy = 0;
			for (int c = 0; c < n_cand; c++) {
				xcb_randr_get_crtc_info_reply_t *ci =
					xcb_randr_get_crtc_info_reply(
						conn,
						xcb_randr_get_crtc_info(conn, cands[c], cfg_ts),
						NULL);
				bool in_use = false;
				if (ci) {
					int no = xcb_randr_get_crtc_info_outputs_length(ci);
					in_use = (ci->mode != 0) || (no > 0);
					free(ci);
				} else {
					in_use = true; /* unknown => treat as busy, fail safe */
				}
				if (in_use)
					busy |= (1u << (unsigned)c);
			}

			int pick = drm_output_pick_free_crtc(busy, n_cand);
			if (pick >= 0) {
				target_output = outputs[i];
				crtc_xid = cands[pick];
			} else {
				blog(LOG_WARNING,
				     "drm-output: output '%s' has no free CRTC to lease "
				     "(busy_mask=0x%x, candidates=%d) — is it still in the X layout?",
				     connector_name, busy, n_cand);
			}
		}
		free(oi);
	}
	free(res);

	if (target_output == 0 || crtc_xid == 0) {
		blog(LOG_WARNING, "drm-output: RandR output '%s' not found / not leasable",
		     connector_name);
		xcb_disconnect(conn);
		return false;
	}

	xcb_randr_lease_t lease = (xcb_randr_lease_t)xcb_generate_id(conn);
	xcb_generic_error_t *err = NULL;
	xcb_randr_create_lease_reply_t *lr = xcb_randr_create_lease_reply(
		conn, xcb_randr_create_lease(conn, root, lease, 1, 1, &crtc_xid, &target_output),
		&err);
	if (!lr || err || lr->nfd < 1) {
		blog(LOG_WARNING, "drm-output: xcb_randr_create_lease failed for '%s'",
		     connector_name);
		free(err);
		free(lr);
		xcb_disconnect(conn);
		return false;
	}
	int *fds = xcb_randr_create_lease_reply_fds(conn, lr);
	int drm_fd = fds[0];
	free(lr);

	g_drm.conn = conn;
	g_drm.lease = lease;
	g_drm.have_lease = true;
	g_drm.drm_fd = drm_fd;
	blog(LOG_INFO, "drm-output: lease acquired output='%s' crtc_xid=%u drm_fd=%d", connector_name,
	     (unsigned)crtc_xid, drm_fd);
	return true;
}

/* Discover the leased CRTC + connector + mode on the leased fd and set the initial scanout. */
static bool drm_output_setup_scanout(uint32_t argb)
{
	drmModeRes *res = drmModeGetResources(g_drm.drm_fd);
	if (!res || res->count_crtcs < 1 || res->count_connectors < 1) {
		blog(LOG_WARNING, "drm-output: leased fd exposes no CRTC/connector");
		if (res)
			drmModeFreeResources(res);
		return false;
	}
	g_drm.crtc_id = res->crtcs[0];
	g_drm.connector_id = res->connectors[0];

	drmModeConnector *conn = drmModeGetConnector(g_drm.drm_fd, g_drm.connector_id);
	if (!conn || conn->count_modes < 1) {
		blog(LOG_WARNING, "drm-output: leased connector has no modes");
		if (conn)
			drmModeFreeConnector(conn);
		drmModeFreeResources(res);
		return false;
	}
	g_drm.mode = conn->modes[0];
	uint32_t w = g_drm.mode.hdisplay;
	uint32_t h = g_drm.mode.vdisplay;
	drmModeFreeConnector(conn);
	drmModeFreeResources(res);

	for (int i = 0; i < DRM_OUTPUT_BUFFERS; i++) {
		struct drm_output_buffer *b = &g_drm.buffers[i];
		if (drmModeCreateDumbBuffer(g_drm.drm_fd, w, h, 32, 0, &b->handle, &b->pitch,
					    &b->size) != 0) {
			/* out-params are undefined on failure — clear the handle so teardown does
			 * not try to destroy a garbage GEM handle. */
			b->handle = 0;
			b->fb_id = 0;
			b->map = NULL;
			blog(LOG_WARNING, "drm-output: create dumb buffer %d failed (%s)", i,
			     strerror(errno));
			return false;
		}
		if (drmModeAddFB(g_drm.drm_fd, w, h, 24, 32, b->pitch, b->handle, &b->fb_id) != 0) {
			blog(LOG_WARNING, "drm-output: addFB %d failed (%s)", i, strerror(errno));
			return false;
		}
		uint64_t offset = 0;
		if (drmModeMapDumbBuffer(g_drm.drm_fd, b->handle, &offset) != 0) {
			blog(LOG_WARNING, "drm-output: map dumb buffer %d failed (%s)", i,
			     strerror(errno));
			return false;
		}
		b->map = mmap(NULL, (size_t)b->size, PROT_READ | PROT_WRITE, MAP_SHARED,
			      g_drm.drm_fd, (off_t)offset);
		if (b->map == MAP_FAILED) {
			b->map = NULL;
			blog(LOG_WARNING, "drm-output: mmap dumb buffer %d failed (%s)", i,
			     strerror(errno));
			return false;
		}
		/* M2 HOOK: for M1 both buffers carry the SAME solid colour so a page-flip proves the
		 * vblank-locked scanout path. In M2 this fill is replaced by importing the OBS Program
		 * GL texture as a dma-buf (EGL export -> drmModeAddFB2WithModifiers) into `back`, so the
		 * live Program renders straight to this scanout with zero copy. */
		drm_output_fill_solid(b, argb, h);
	}

	uint32_t conn_id = g_drm.connector_id;
	if (drmModeSetCrtc(g_drm.drm_fd, g_drm.crtc_id, g_drm.buffers[0].fb_id, 0, 0, &conn_id, 1,
			   &g_drm.mode) != 0) {
		blog(LOG_WARNING, "drm-output: setCrtc failed (%s)", strerror(errno));
		return false;
	}
	blog(LOG_INFO, "drm-output: mode set %ux%u@%uHz crtc=%u connector=%u", w, h,
	     (unsigned)g_drm.mode.vrefresh, (unsigned)g_drm.crtc_id, (unsigned)g_drm.connector_id);
	return true;
}

/* The vblank-locked page-flip loop. For M1 it alternates the two solid buffers, waiting on each
 * flip-complete event — proving OBS holds the leased CRTC and page-flips at the display rate. */
static void *drm_output_flip_thread(void *arg)
{
	(void)arg;
	drmEventContext evctx;
	memset(&evctx, 0, sizeof(evctx));
	evctx.version = 2;
	evctx.page_flip_handler = drm_output_page_flip_handler;

	int front = 0;
	bool fatal = false;
	while (os_atomic_load_bool(&g_drm.running) && !fatal) {
		int back = front ^ 1;
		volatile int pending = 1;
		if (drmModePageFlip(g_drm.drm_fd, g_drm.crtc_id, g_drm.buffers[back].fb_id,
				    DRM_MODE_PAGE_FLIP_EVENT, (void *)&pending) != 0) {
			blog(LOG_WARNING, "drm-output: drmModePageFlip failed (%s) — stopping flip loop",
			     strerror(errno));
			break;
		}
		/* Wait for the flip-complete vblank event; poll so stop() can break within 1s. Any fd
		 * error (an X-server restart / external lease revoke on the imag rig) or a page-flip that
		 * never completes must NOT become a 100%-CPU spin on the 25W-clamped box — break out or
		 * surface a wedge warning (the wedge-watchdog-pattern class). */
		unsigned overdue = 0;
		while (os_atomic_load_bool(&g_drm.running) && pending) {
			struct pollfd pfd;
			pfd.fd = g_drm.drm_fd;
			pfd.events = POLLIN;
			pfd.revents = 0;
			int pr = poll(&pfd, 1, 1000);
			if (pr < 0) {
				if (errno == EINTR)
					continue;
				blog(LOG_WARNING, "drm-output: poll on DRM fd failed (%s) — stopping",
				     strerror(errno));
				fatal = true;
				break;
			}
			if (pr == 0) {
				/* No flip-complete within 1s. Keep waiting (stop can still break us),
				 * but surface a wedge signal after ~5s of silence for the dev1 watchdog. */
				if (++overdue % 5u == 0u)
					blog(LOG_WARNING,
					     "drm-output: page-flip completion overdue (%us) on crtc=%u — "
					     "possible display wedge",
					     overdue, (unsigned)g_drm.crtc_id);
				continue;
			}
			if (pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) {
				blog(LOG_WARNING,
				     "drm-output: DRM fd error (revents=0x%x) — lease revoked? stopping",
				     (unsigned)pfd.revents);
				fatal = true;
				break;
			}
			if ((pfd.revents & POLLIN) && drmHandleEvent(g_drm.drm_fd, &evctx) != 0) {
				blog(LOG_WARNING, "drm-output: drmHandleEvent failed — stopping");
				fatal = true;
				break;
			}
		}
		front = back;
		g_drm.flips++;
		/* Log the first flip (immediate mechanism proof), then ~once/minute at 60Hz so this
		 * never floods the OBS log the jitter_audit-family parsers grep once M2 runs steady. */
		if (g_drm.flips == 1ULL || g_drm.flips % 3600ULL == 0ULL)
			blog(LOG_INFO, "drm-output: page-flip #%llu (vblank-locked, solid M1 pattern)",
			     g_drm.flips);
	}
	blog(LOG_INFO, "drm-output: flip loop exited after %llu flips", g_drm.flips);
	return NULL;
}

/* Tear down everything acquired by start (idempotent — safe on partial init). MUST run under lock. */
static void drm_output_teardown_locked(void)
{
	if (g_drm.drm_fd >= 0) {
		for (int i = 0; i < DRM_OUTPUT_BUFFERS; i++) {
			struct drm_output_buffer *b = &g_drm.buffers[i];
			if (b->map) {
				munmap(b->map, (size_t)b->size);
				b->map = NULL;
			}
			if (b->fb_id) {
				drmModeRmFB(g_drm.drm_fd, b->fb_id);
				b->fb_id = 0;
			}
			if (b->handle) {
				drmModeDestroyDumbBuffer(g_drm.drm_fd, b->handle);
				b->handle = 0;
			}
		}
		close(g_drm.drm_fd);
		g_drm.drm_fd = -1;
	}
	if (g_drm.have_lease && g_drm.conn) {
		/* terminate=1: revoke the lease so the connector returns to Xorg immediately. */
		xcb_randr_free_lease(g_drm.conn, g_drm.lease, 1);
		xcb_flush(g_drm.conn);
		g_drm.have_lease = false;
		g_drm.lease = 0;
		blog(LOG_INFO, "drm-output: lease released — connector returned to Xorg");
	}
	if (g_drm.conn) {
		xcb_disconnect(g_drm.conn);
		g_drm.conn = NULL;
	}
	g_drm.crtc_id = 0;
	g_drm.connector_id = 0;
}

bool obs_drm_output_start(const struct obs_drm_output_config *cfg)
{
	if (!cfg || !cfg->connector_name) {
		blog(LOG_WARNING, "drm-output: start called with no connector — ignored");
		return false;
	}

	/* NOTE: the whole start sequence holds g_drm.lock across X round-trips + a full modeset
	 * (seconds if X is slow). obs_drm_output_active() blocks for that window — fine for M1's
	 * single-threaded activation; M2 must not call active() from a render-adjacent thread until
	 * this lock scope is narrowed. */
	pthread_mutex_lock(&g_drm.lock);
	if (g_drm.stopping) {
		pthread_mutex_unlock(&g_drm.lock);
		blog(LOG_WARNING, "drm-output: start rejected — a stop is in progress");
		return false;
	}
	if (g_drm.active) {
		pthread_mutex_unlock(&g_drm.lock);
		blog(LOG_INFO, "drm-output: already active — start ignored");
		return true;
	}
	g_drm.flips = 0;

	bool ok = drm_output_acquire_lease(cfg->connector_name) &&
		  drm_output_setup_scanout(cfg->solid_argb);
	if (!ok) {
		drm_output_teardown_locked();
		pthread_mutex_unlock(&g_drm.lock);
		blog(LOG_WARNING, "drm-output: start FAILED for '%s'", cfg->connector_name);
		return false;
	}

	os_atomic_set_bool(&g_drm.running, true);
	if (pthread_create(&g_drm.thread, NULL, drm_output_flip_thread, NULL) != 0) {
		os_atomic_set_bool(&g_drm.running, false);
		drm_output_teardown_locked();
		pthread_mutex_unlock(&g_drm.lock);
		blog(LOG_WARNING, "drm-output: could not create flip thread");
		return false;
	}
	g_drm.active = true;
	pthread_mutex_unlock(&g_drm.lock);
	blog(LOG_INFO, "drm-output: ACTIVE — page-flipping solid 0x%06x on '%s'",
	     cfg->solid_argb & 0xFFFFFFu, cfg->connector_name);
	return true;
}

void obs_drm_output_stop(void)
{
	pthread_mutex_lock(&g_drm.lock);
	if (!g_drm.active || g_drm.stopping) {
		/* Not running, or another stop already claimed the teardown — never join twice
		 * (joining an already-joined pthread_t is undefined behaviour). */
		pthread_mutex_unlock(&g_drm.lock);
		return;
	}
	g_drm.stopping = true; /* claim the transition: a racing stop returns above, a racing start rejects */
	os_atomic_set_bool(&g_drm.running, false);
	pthread_t th = g_drm.thread;
	pthread_mutex_unlock(&g_drm.lock);

	/* Join outside the lock: the flip loop reads g_drm fields but never takes g_drm.lock. */
	pthread_join(th, NULL);

	pthread_mutex_lock(&g_drm.lock);
	drm_output_teardown_locked();
	g_drm.active = false;
	g_drm.stopping = false;
	pthread_mutex_unlock(&g_drm.lock);
	blog(LOG_INFO, "drm-output: stopped");
}

bool obs_drm_output_active(void)
{
	pthread_mutex_lock(&g_drm.lock);
	bool a = g_drm.active;
	pthread_mutex_unlock(&g_drm.lock);
	return a;
}

void obs_drm_output_maybe_autostart(void)
{
	const char *home = getenv("HOME");
	char path[1024];
	if (!home || home[0] == '\0') {
		blog(LOG_INFO, "drm-output: autostart disabled (no HOME) — module present, dormant");
		return;
	}
	int n = snprintf(path, sizeof(path), "%s/.camera-box/drm-output.json", home);
	if (n < 0 || (size_t)n >= sizeof(path)) {
		blog(LOG_INFO, "drm-output: autostart disabled (config path too long) — dormant");
		return;
	}

	if (access(path, R_OK) != 0) {
		blog(LOG_INFO, "drm-output: autostart disabled (no config at %s) — module present, dormant",
		     path);
		return;
	}

	obs_data_t *data = obs_data_create_from_json_file(path);
	if (!data) {
		blog(LOG_WARNING, "drm-output: autostart config %s present but unparseable — dormant",
		     path);
		return;
	}
	bool enabled = obs_data_get_bool(data, "enabled");
	const char *connector = obs_data_get_string(data, "connector");
	bool has_argb = obs_data_has_user_value(data, "argb");
	long long argb_ll = obs_data_get_int(data, "argb");

	if (!enabled) {
		blog(LOG_INFO, "drm-output: autostart disabled ({\"enabled\":false} in %s) — dormant",
		     path);
		obs_data_release(data);
		return;
	}
	if (!connector || connector[0] == '\0') {
		blog(LOG_WARNING, "drm-output: autostart config %s has no \"connector\" — dormant", path);
		obs_data_release(data);
		return;
	}

	struct obs_drm_output_config cfg;
	/* Copy the connector name onto the stack; obs_drm_output_start() consumes it synchronously
	 * (strlen/memcmp/log) and never retains the pointer, so it need not outlive this call. */
	char connector_buf[128];
	snprintf(connector_buf, sizeof(connector_buf), "%s", connector);
	cfg.connector_name = connector_buf;
	/* Honour an explicit "argb": 0 (solid black); only fall back to dark grey when absent. */
	cfg.solid_argb = has_argb ? (uint32_t)argb_ll : 0x00202020u;

	blog(LOG_INFO, "drm-output: autostart ENABLED from %s — connector='%s'", path,
	     cfg.connector_name);
	obs_data_release(data);
	(void)obs_drm_output_start(&cfg);
}

#endif /* defined(__linux__) */
