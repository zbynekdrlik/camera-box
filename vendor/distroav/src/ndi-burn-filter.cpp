/******************************************************************************
	#111 — DistroAV QR render-time burn filter (Path B).

	An OBS effect filter that burns a per-render QR (carrying this node's frame
	identity + boundary-snapped wall-clock emit time) into the rendered video each
	frame, so a downstream RECORDING captures a frame-exact, per-node timestamp. The
	burned payload is byte-identical to the camera-box probe payload
	(src/probe/payload.rs), so the existing rqrr recorded-file decoder
	(src/probe/recording.rs / #106) reads it UNCHANGED, and #108 (post-event) can
	subtract `node_stamp - cam2_gen_ts` per hop on one shared timebase.

	Architecture (the blueprint's, patterned on ndi-filter.cpp's render→stage path):
	  1. gs_texrender the filter target (the source this filter is attached to).
	  2. gs_stage_texture + gs_stagesurface_map -> a CPU BGRA copy of the rendered frame.
	  3. CPU-draw the QR (burn_qr::render, qrcodegen EC-High, white quiet zone) into the
	     copy at THIS node's BOTTOM CORNER (strih=bottom-left, stream=bottom-right,
	     imag=bottom-center-left, #463) at ~300px (burn_geom::corner_placement) — fully
	     clear of the camera dual-QR (top band) and of every other node's burn, so one
	     stream recording carries every node's readable QR (camera L/R + strih + stream +
	     imag burns), none overlapping (#111 4-corner layout, extended by #463 — replaces
	     the old center-bottom ~700px burn that overlapped both the camera QR and the
	     other node's burn → strih→stream 0 paired frames).
	  4. Re-upload the composited buffer to a dynamic texture and gs_draw_sprite it as the
	     filter's output, so the burn flows downstream into the recording.
	NO libobs core change — this is purely a DistroAV plugin filter.

	Identity / gating (#257 — no env; #463 adds imag):
	  - run_id: reserved per-node constant DERIVED FROM THE HOST ROLE (no OBS_BURN_RUN_ID env):
	    stream box (hostname contains "stream") = 911004, imag box (hostname contains "imag") =
	    911003 (#463 — Topology v2 IMAG box, freed from the deferred cam3 camera-capture role,
	    see `BURN_RUN_ID_IMAG` in camera-box's `src/probe/recording_latency.rs`), any other
	    (default, incl. strih) = 911002. All three sit OUTSIDE cam2's normal run_id range so
	    #108 distinguishes node-stamp from cam2-stamp.
	  - frame_id: this filter's own per-render monotonic counter.
	  - gen_ts_ns: RAW render-instant wall-clock (burn_clock::gen_ts_ns, NOT boundary-
	    snapped) — shares the camera-box painter's RAW basis so cam→strih is bias-free (#108
	    finding #2). The genlock EMIT timecode (ndi-output.cpp) stays snapped; separate path.
	  - Gated by the PARENT source's per-source genlock_burn bool (#257, default OFF, no env),
	    read LIVE each render (obs_source_get_genlock_burn) so toggling needs NO OBS restart.
	    When OFF the filter is a transparent pass-through (renders the target, no burn).

	UNVERIFIED until post-event on-rig deploy: the actual burn into a real recording.
	VERIFIED pre-event (tests/burn_payload_parity.rs): the payload is byte-identical to
	Rust Payload::encode, round-trips through the decoder, and the C++ QR render path
	produces a dual-QR frame rqrr decodes back to the burned payloads.
******************************************************************************/

#include "plugin-main.h"

#include "burn-payload.hpp"
#include "burn-clock.hpp"
#include "burn-qr.hpp"
#include "burn-geom.hpp"
#include "burn-tick-cache.hpp"

#include <graphics/graphics.h>
#include <util/platform.h>

#include <atomic>
#include <cctype>
#include <cstring>
#include <string>

#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#include <unistd.h>
#endif

#define OBS_NDI_BURN_FILTER_ID "distroav_qr_burn_filter"
#define BURN_TEXFORMAT GS_BGRA

// camera-box #111/#257: reserved per-node burn run_ids. strih = 911002, stream = 911004 —
// both far OUTSIDE cam2's normal run_id range so the verdict tells node-stamp from cam2-stamp.
// #257 removed the OBS_BURN_RUN_ID env: the run_id is now derived from THIS box's hostname
// (the fixed per-box/role default), so no env is needed and the verdict 911002/911004 pairing
// keeps working. Mirror of src/probe/recording_latency.rs BURN_RUN_ID_STRIH / _STREAM.
#define BURN_RUN_ID_DEFAULT_STRIH 911002u
#define BURN_RUN_ID_DEFAULT_STREAM 911004u
// camera-box #463: imag-nb's (Topology v2 IMAG box) reserved burn run_id — freed from the
// deferred cam3 camera-capture role (that mechanism is unrelated: `CAMERA_BOX_BURN_RUN_ID` on
// a source camera, not this OBS filter). Mirror of `BURN_RUN_ID_IMAG` in
// src/probe/recording_latency.rs (renamed from `BURN_RUN_ID_CAM3` — see #463 / issue #24).
#define BURN_RUN_ID_DEFAULT_IMAG 911003u

struct burn_filter {
	obs_source_t *context;

	// camera-box #404: OVERLAY compositing — no per-frame full-frame GPU→CPU readback.
	// The base video is rendered to a texrender (GPU) and drawn as-is; only the small
	// (~square_px) QR is CPU-drawn into `work` and uploaded to `qr_texture`, then drawn as
	// a sprite over the corner. This replaces the old gs_stage_texture + gs_stagesurface_map
	// full-frame readback + full-frame re-upload that cost ~14-24 ms/frame and choked the
	// 60fps render (#404) — the QR square is ~0.28*h, so the CPU work + upload is ~500× less
	// pixels than the whole 1920×1080 frame, and the readback is gone entirely.
	gs_texrender_t *texrender;   // base video render target (GPU)
	uint32_t known_width;        // texrender size
	uint32_t known_height;
	gs_texture_t *qr_texture;    // small QR overlay texture (qr_side × qr_side)
	uint32_t known_qr_side;      // qr_texture size

	// CPU working buffer for the small QR square only (qr_side × qr_side × 4).
	uint8_t *work;
	size_t work_size;

	// Identity (resolved once at create from the host role — no env). The ENABLE gate is
	// the parent source's per-source genlock_burn flag, read LIVE each render (#257).
	uint32_t run_id;
	uint32_t qr_px;
	burn_geom::Corner corner; // this node's bottom corner (strih=left, stream=right, imag=BCL)

	// Per-render monotonic frame counter. camera-box #1260: advanced ONCE PER TICK (in
	// burn_draw_qr, gated by tick_cache below), not per draw — see tick_cache.
	uint32_t frame_id;

	// camera-box #1260: within-tick "prepare once, reuse" state. The burn filter's video_render
	// runs once per DRAW of this source (PROGRAM + Studio-Mode preview + every Multiview cell).
	// Doing the full base texrender + QR raster/upload per draw meant strih's 4K MV re-ran all 7
	// cam burns every MV frame, pushing the MV render_ewma over the per-tick budget and collapsing
	// it to 7.5fps (#278/#293). We prep the base texrender + QR + advance frame_id ONCE per tick
	// (the first draw = the PROGRAM, since output_frames() runs before render_displays()); the
	// later within-tick draws reuse f->texrender + f->qr_texture (a cheap sprite blit).
	// burn_filter_videotick clears it each tick; bzalloc zeroes it, so the first render preps.
	struct burn_tick_cache tick_cache;
};

// camera-box #257: resolve an OBS export by name at RUNTIME (same rationale as ndi-source.cpp:
// the Windows DistroAV build fetches stock OBS SDK headers without the genlock symbols, so a
// link-time call cannot build; runtime binding works against any headers AND keeps the plugin
// loadable on a stock OBS — the burn-enable read is just inert there).
static void *burn_resolve_obs_export(const char *name)
{
#ifdef _WIN32
	HMODULE m = GetModuleHandleA("obs.dll");
	if (!m)
		m = GetModuleHandleA("libobs.dll");
	return m ? (void *)GetProcAddress(m, name) : nullptr;
#else
	return dlsym(RTLD_DEFAULT, name);
#endif
}

// camera-box #257: the per-source measurement-burn ENABLE — read LIVE each render from the
// parent NDI source's genlock_burn flag (set over OBS WebSocket SetInputSettings genlock_burn,
// applied by ndi_source_update → obs_source_set_genlock_burn). NO env (OBS_BURN_QR is gone):
// toggling the burn no longer needs an OBS relaunch.
typedef bool (*get_genlock_burn_fn)(const obs_source_t *);
static get_genlock_burn_fn resolve_get_genlock_burn()
{
	static get_genlock_burn_fn fn = nullptr;
	static bool tried = false;
	if (!tried) {
		tried = true;
		fn = (get_genlock_burn_fn)burn_resolve_obs_export("obs_source_get_genlock_burn");
		if (!fn)
			obs_log(LOG_WARNING,
				"[burn] obs_source_get_genlock_burn not exported by this OBS build — "
				"the measurement-burn toggle is inert (stock OBS?)");
	}
	return fn;
}

// camera-box #257: is THIS box the stream node? Derived from the hostname (no env) — the fixed
// per-box/role default. stream box → run_id 911004 / bottom-right; strih (and any other) →
// 911002 / bottom-left. A substring match on "stream" keeps it robust to the exact host name.
static bool burn_host_is_stream()
{
	char name[256] = {0};
#ifdef _WIN32
	DWORD n = (DWORD)sizeof(name);
	if (!GetComputerNameA(name, &n))
		name[0] = '\0';
#else
	if (gethostname(name, sizeof(name) - 1) != 0)
		name[0] = '\0';
#endif
	for (char *p = name; *p; ++p)
		*p = (char)tolower((unsigned char)*p);
	/* #257: the verdict's strih(911002)/stream(911004) pairing rests on this host-role match. If
	 * the box is NEITHER "stream" NOR "strih" (renamed / a new host), we fall back to strih's id —
	 * which would silently mis-pair the verdict. WARN LOUD so a host rename is never a silent
	 * mis-stamp (the resolved id is also logged at filter-create). */
	if (!strstr(name, "stream") && !strstr(name, "strih"))
		obs_log(LOG_WARNING,
			"[burn] host '%s' matches neither 'stream' nor 'strih' — defaulting run_id to strih "
			"(911002). If this box should stamp 911004, rename it to contain 'stream' (#257).",
			name);
	return strstr(name, "stream") != nullptr;
}

// camera-box #463: is THIS box the imag node (Topology v2 IMAG box, imag-nb)? Same
// hostname-substring style as burn_host_is_stream, kept as its OWN predicate (checked FIRST by
// resolve_run_id, below) so an imag host never falls through into burn_host_is_stream's
// stream/strih-only WARN — an "imag" hostname legitimately matches neither of those substrings
// and must NOT be treated as a host-rename anomaly.
static bool burn_host_is_imag()
{
	char name[256] = {0};
#ifdef _WIN32
	DWORD n = (DWORD)sizeof(name);
	if (!GetComputerNameA(name, &n))
		name[0] = '\0';
#else
	if (gethostname(name, sizeof(name) - 1) != 0)
		name[0] = '\0';
#endif
	for (char *p = name; *p; ++p)
		*p = (char)tolower((unsigned char)*p);
	return strstr(name, "imag") != nullptr;
}

// camera-box #257/#463: this box's reserved burn run_id from the host role (no OBS_BURN_RUN_ID
// env). imag is checked FIRST and returns early so burn_host_is_stream() (unchanged) is only
// ever consulted for a non-imag host, exactly its original strih/stream/anomaly-WARN behaviour.
static uint32_t resolve_run_id()
{
	if (burn_host_is_imag())
		return BURN_RUN_ID_DEFAULT_IMAG;
	return burn_host_is_stream() ? BURN_RUN_ID_DEFAULT_STREAM : BURN_RUN_ID_DEFAULT_STRIH;
}

// This node's bottom corner, derived FROM the run_id (stream → bottom-right, strih →
// bottom-left, imag → bottom-center-left, #463) so one stream recording carries every node's QR
// (camera L/R + strih + stream + imag burns) with no overlap (#111 4-corner layout, extended by
// #463). #257 removed the OBS_BURN_CORNER env.
static burn_geom::Corner resolve_corner(uint32_t run_id)
{
	if (run_id == BURN_RUN_ID_DEFAULT_STREAM)
		return burn_geom::Corner::BottomRight;
	if (run_id == BURN_RUN_ID_DEFAULT_IMAG)
		return burn_geom::Corner::BottomCenterLeft;
	return burn_geom::Corner::BottomLeft;
}

// Human-readable tag for `corner`, shared by the create-log and the throttled per-frame
// draw-log (#463 — extended from a strih/stream-only ternary to the three-way case).
static const char *corner_tag(burn_geom::Corner corner)
{
	switch (corner) {
	case burn_geom::Corner::BottomRight:
		return "bottom-right";
	case burn_geom::Corner::BottomCenterLeft:
		return "bottom-center-left";
	default:
		return "bottom-left";
	}
}

static const char *burn_filter_getname(void *)
{
	return "DistroAV QR Burn (latency probe)";
}

static obs_properties_t *burn_filter_getproperties(void *)
{
	return obs_properties_create();
}

static void burn_filter_update(void *, obs_data_t *) {}

static void *burn_filter_create(obs_data_t *, obs_source_t *source)
{
	auto *f = (burn_filter *)bzalloc(sizeof(burn_filter));
	f->context = source;
	f->run_id = resolve_run_id(); // #257: from the host role (no env)
	f->qr_px = 0u;                 // #257: always canvas-relative auto (no OBS_BURN_QR_PX env)
	f->corner = resolve_corner(f->run_id);
	f->frame_id = 0;

	obs_log(LOG_INFO,
		"[burn] filter created: run_id=%u (host role) corner=%s qr_px=auto — burn is gated LIVE "
		"by the parent source's per-source genlock_burn flag (#257, no env, no restart). "
		"run_ids %u/%u/%u strih/stream/imag → bottom-left/bottom-right/bottom-center-left (#463)",
		f->run_id, corner_tag(f->corner), BURN_RUN_ID_DEFAULT_STRIH, BURN_RUN_ID_DEFAULT_STREAM,
		BURN_RUN_ID_DEFAULT_IMAG);
	return f;
}

// Destroy the GPU composite resources. Caller MUST already hold the graphics context
// (e.g. from inside video_render, or wrapped by burn_free_gfx off-thread).
static void burn_destroy_gfx_locked(burn_filter *f)
{
	if (f->texrender) {
		gs_texrender_destroy(f->texrender);
		f->texrender = nullptr;
	}
	if (f->qr_texture) {
		gs_texture_destroy(f->qr_texture);
		f->qr_texture = nullptr;
	}
}

// Off-graphics-thread free (e.g. from destroy): take the graphics context, then destroy.
// gs_enter_context is refcount-recursive, so this is also safe if ever called with the
// context held, but the render path uses burn_destroy_gfx_locked directly (no redundant
// enter) per the libobs idiom.
static void burn_free_gfx(burn_filter *f)
{
	obs_enter_graphics();
	burn_destroy_gfx_locked(f);
	obs_leave_graphics();
}

static void burn_filter_destroy(void *data)
{
	auto *f = (burn_filter *)data;
	burn_free_gfx(f);
	if (f->work)
		bfree(f->work);
	obs_log(LOG_INFO, "[burn] filter destroyed (run_id=%u, last frame_id=%u)", f->run_id,
		f->frame_id);
	bfree(f);
}

// camera-box #404: (Re)allocate the OVERLAY resources — a frame-sized texrender for the
// base video (GPU) and a SMALL qr_side × qr_side texture + CPU buffer for the QR overlay.
// No full-frame stagesurface / readback buffer any more.
//
// Called from video_render with the graphics context already held: destroy + recreate
// directly (no redundant obs_enter_graphics — the libobs idiom, matches ndi-filter.cpp
// which creates GPU resources inside video_render without entering the context).
static bool burn_ensure_resources(burn_filter *f, uint32_t width, uint32_t height, uint32_t qr_side)
{
	if (qr_side < 1)
		qr_side = 1;
	const bool tex_ok = f->known_width == width && f->known_height == height && f->texrender;
	const bool qr_ok = f->known_qr_side == qr_side && f->qr_texture && f->work;
	if (tex_ok && qr_ok)
		return true;

	if (!tex_ok) {
		if (f->texrender) {
			gs_texrender_destroy(f->texrender);
			f->texrender = nullptr;
		}
		f->texrender = gs_texrender_create(BURN_TEXFORMAT, GS_ZS_NONE);
		if (!f->texrender) {
			obs_log(LOG_ERROR, "[burn] failed to allocate texrender for %ux%u", width, height);
			return false;
		}
		f->known_width = width;
		f->known_height = height;
	}

	if (!qr_ok) {
		if (f->qr_texture) {
			gs_texture_destroy(f->qr_texture);
			f->qr_texture = nullptr;
		}
		if (f->work) {
			bfree(f->work);
			f->work = nullptr;
		}
		f->qr_texture =
			gs_texture_create(qr_side, qr_side, BURN_TEXFORMAT, 1, nullptr, GS_DYNAMIC);
		f->work_size = (size_t)qr_side * qr_side * 4;
		f->work = (uint8_t *)bmalloc(f->work_size);
		if (!f->qr_texture || !f->work) {
			obs_log(LOG_ERROR, "[burn] failed to allocate QR overlay for %ux%u", qr_side,
				qr_side);
			return false;
		}
		f->known_qr_side = qr_side;
	}

	obs_log(LOG_DEBUG, "[burn] (re)allocated overlay resources: base %ux%u, QR %ux%u", width,
		height, qr_side, qr_side);
	return true;
}

// camera-box #404: render THIS node's QR into a SMALL tight `side` × `side` BGRA buffer
// (pre-cleared white by the caller), CENTERED. Advances the per-render frame_id, encodes the
// payload, logs throttled. The caller uploads `buf` to `qr_texture` and draws it as a corner
// sprite — so this is the ONLY per-frame CPU pixel work (~side² px), and there is no full-frame
// GPU→CPU readback any more. `pl` is the corner placement (used by the caller for the sprite
// position; here only for the log). QR pixels are byte-identical to the old path (same
// burn_qr::render), so the recorded QR decodes UNCHANGED (burn_payload_parity / fixture tests).
static void burn_draw_qr(burn_filter *f, uint8_t *buf, uint32_t side,
			 const burn_geom::Placement &pl, double fps)
{
	const uint32_t fid = f->frame_id++;
	const int64_t gen_ts_ns = burn_clock::gen_ts_ns(fps);
	const std::string payload = burn_payload::encode(f->run_id, fid, gen_ts_ns);

	// Center the QR in the side×side overlay buffer: the band IS the whole buffer.
	burn_qr::render(buf, side * 4, side, side, payload, 0, side, side / 2, side);

	if ((fid % 300u) == 0u) // throttled: one log line / ~10s @ 30fps
		obs_log(LOG_INFO,
			"[burn] burned QR run_id=%u frame_id=%u gen_ts_ns=%lld corner=%s "
			"band_x=%u band_cy=%u px=%u overlay (%.3f fps)",
			f->run_id, fid, (long long)gen_ts_ns, corner_tag(f->corner), pl.band_x,
			pl.band_cy, pl.square_px, fps);
}

static void burn_filter_videorender(void *data, gs_effect_t *)
{
	auto *f = (burn_filter *)data;
	obs_source_t *target = obs_filter_get_target(f->context);
	obs_source_t *parent = obs_filter_get_parent(f->context);

	if (!target || !parent) {
		obs_source_skip_video_filter(f->context);
		return;
	}

	// camera-box #257: the ENABLE gate is the PARENT source's per-source genlock_burn flag,
	// read LIVE here (toggled over OBS WebSocket SetInputSettings genlock_burn → applied by
	// ndi_source_update → obs_source_set_genlock_burn, NO OBS restart). OFF (default, prod):
	// transparent pass-through, zero overhead beyond a render. No env (OBS_BURN_QR is gone).
	bool burn_on = false;
	if (auto get_burn = resolve_get_genlock_burn())
		burn_on = get_burn(parent);
	if (!burn_on) {
		obs_source_skip_video_filter(f->context);
		return;
	}

	const uint32_t width = obs_source_get_width(f->context);
	const uint32_t height = obs_source_get_height(f->context);
	if (width == 0 || height == 0) {
		obs_source_skip_video_filter(f->context);
		return;
	}

	// #404: overlay placement — the QR square side + its corner position. BOTH the QR px and the
	// edge margin are canvas-relative (#186/#172, #257: f->qr_px is 0 = auto). corner_placement
	// clamps side to fit a tiny frame, so use its returned square_px for the texture + sprite.
	const uint32_t margin = burn_geom::burn_margin_for_canvas(height);
	const uint32_t qr_px = burn_geom::burn_qr_px_for_canvas(f->qr_px, height);
	const burn_geom::Placement pl = burn_geom::corner_placement(width, height, f->corner, qr_px,
								    margin);
	const uint32_t side = pl.square_px < 1u ? 1u : pl.square_px;

	if (!burn_ensure_resources(f, width, height, side)) {
		obs_source_skip_video_filter(f->context);
		return;
	}

	const double fps = []() {
		obs_video_info ovi;
		if (obs_get_video_info(&ovi) && ovi.fps_den > 0)
			return (double)ovi.fps_num / (double)ovi.fps_den;
		return 0.0;
	}();

	// camera-box #1260: decide ONCE per tick whether to do the expensive PREP. The first draw of
	// the tick (always the PROGRAM — output_frames() runs before render_displays()) preps + stamps
	// frame_id; the later within-tick draws (Studio-Mode preview, Multiview cells) REUSE the cached
	// f->texrender + f->qr_texture (the sprite blit in section 3, always run below). This is what
	// takes strih's 4K MV out of the #278/#293 budget collapse (7.5fps) — the 7 MV burns no longer
	// re-render every frame — AND stamps the recorded frame_id once per tick, not per draw.
	const bool prepare = burn_tick_cache_on_render(&f->tick_cache);

	if (prepare) {
		// 1) Render the target into our texrender (GPU — NO CPU readback).
		gs_texrender_reset(f->texrender);
		if (!gs_texrender_begin(f->texrender, width, height)) {
			// #1260: the prep FAILED (transient graphics-reset) — do NOT leave the tick marked
			// prepared, or a later within-tick draw would reuse a stale/empty composite. Re-arm
			// the next draw this tick, then pass the source through for this frame (as before).
			burn_tick_cache_abort_prepare(&f->tick_cache);
			obs_source_skip_video_filter(f->context);
			return;
		}
		struct vec4 clear;
		vec4_zero(&clear);
		gs_clear(GS_CLEAR_COLOR, &clear, 0.0f, 0);
		gs_ortho(0.0f, (float)width, 0.0f, (float)height, -100.0f, 100.0f);
		gs_blend_state_push();
		gs_blend_function(GS_BLEND_ONE, GS_BLEND_ZERO);
		// Degenerate case (filter directly on a source with no chain): mirrors ndi-filter.cpp.
		// The texrender stays cleared/transparent and we still overlay the QR — only a
		// MISCONFIGURATION (the probe scene attaches this filter to a real program source, not
		// a bare source), so this path is cosmetic, not the intended use.
		if (target == parent)
			obs_source_skip_video_filter(f->context);
		else
			obs_source_video_render(target);
		gs_blend_state_pop();
		gs_texrender_end(f->texrender);

		// 2) CPU-draw ONLY the small QR into the pre-cleared-white overlay buffer, upload to the
		//    small texture. This is the ONLY per-frame CPU pixel work — ~side² px, not the whole
		//    1920×1080 frame — and there is NO gs_stagesurface_map readback (#404). Once per tick
		//    now (#1260), so burn_draw_qr advances frame_id once per tick, not per draw.
		std::memset(f->work, 0xFF, f->work_size); // white backing fills the overlay square
		burn_draw_qr(f, f->work, side, pl, fps);
		gs_texture_set_image(f->qr_texture, f->work, side * 4, false);
	}

	// 3) Draw the base (texrender) full-frame, then overlay the QR sprite at the corner. ALWAYS
	//    run (both the prep draw and the reuse draws) — #1260: a reuse draw blits the cached
	//    f->texrender + f->qr_texture from this tick's prep (a cheap sprite blit).
	gs_texture_t *base = gs_texrender_get_texture(f->texrender);
	gs_effect_t *def = obs_get_base_effect(OBS_EFFECT_DEFAULT);
	gs_eparam_t *image = def ? gs_effect_get_param_by_name(def, "image") : nullptr;
	if (!def || !image || !base) {
		// Default effect / base unavailable (graphics-reset window): pass the source through
		// rather than crash the graphics thread (the burn is dropped for this frame).
		obs_log(LOG_WARNING, "[burn] default effect/base unavailable; passing frame through");
		obs_source_skip_video_filter(f->context);
		return;
	}

	// Draw sRGB-correctly, exactly as libobs render_filter_tex (obs-source.c) does: under OBS's
	// default linear-sRGB pipeline the texture MUST be bound via the _srgb setter and the
	// framebuffer-sRGB state enabled, or the video is drawn with the wrong gamma downstream.
	// Opaque blend (GS_BLEND_ONE/ZERO) for BOTH draws: the base fills the frame and the QR
	// square (white backing + black modules, A=255) REPLACES the corner — identical output to
	// the old white-backing-into-a-readback composite, so the recorded QR decodes unchanged.
	const bool linear_srgb = gs_get_linear_srgb();
	const bool prev_srgb = gs_framebuffer_srgb_enabled();
	gs_enable_framebuffer_srgb(linear_srgb);
	gs_blend_state_push();
	gs_blend_function(GS_BLEND_ONE, GS_BLEND_ZERO);

	// base video
	if (linear_srgb)
		gs_effect_set_texture_srgb(image, base);
	else
		gs_effect_set_texture(image, base);
	while (gs_effect_loop(def, "Draw"))
		gs_draw_sprite(base, 0, width, height);

	// QR overlay at the corner: top-left = (band_x, band_cy - side/2).
	const uint32_t ox = pl.band_x;
	const uint32_t oy = (pl.band_cy > side / 2u) ? (pl.band_cy - side / 2u) : 0u;
	gs_matrix_push();
	gs_matrix_translate3f((float)ox, (float)oy, 0.0f);
	if (linear_srgb)
		gs_effect_set_texture_srgb(image, f->qr_texture);
	else
		gs_effect_set_texture(image, f->qr_texture);
	while (gs_effect_loop(def, "Draw"))
		gs_draw_sprite(f->qr_texture, 0, side, side);
	gs_matrix_pop();

	gs_blend_state_pop();
	gs_enable_framebuffer_srgb(prev_srgb);
}

// camera-box #1260: clear the within-tick prepare flag once per video tick, so the next render
// re-preps + re-stamps the burn for the new frame. video_tick fires once per tick per source on
// the graphics thread (tick_sources, obs-video.c), BEFORE output_frames()/render_displays() — the
// same per-frame cadence obs-filters' crop/scale/scroll/gpu-delay filters rely on.
static void burn_filter_videotick(void *data, float)
{
	auto *f = (burn_filter *)data;
	burn_tick_cache_on_tick(&f->tick_cache);
}

struct obs_source_info create_ndi_burn_filter_info()
{
	struct obs_source_info info = {};
	info.id = OBS_NDI_BURN_FILTER_ID;
	info.type = OBS_SOURCE_TYPE_FILTER;
	info.output_flags = OBS_SOURCE_VIDEO;
	info.get_name = burn_filter_getname;
	info.get_properties = burn_filter_getproperties;
	info.create = burn_filter_create;
	info.destroy = burn_filter_destroy;
	info.update = burn_filter_update;
	info.video_render = burn_filter_videorender;
	info.video_tick = burn_filter_videotick; // #1260: once-per-tick cache invalidation
	return info;
}
