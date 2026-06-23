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
	     copy at THIS node's BOTTOM CORNER (strih=bottom-left, stream=bottom-right) at
	     ~300px (burn_geom::corner_placement) — fully clear of the camera dual-QR (top
	     band) and of the other node's burn, so one stream recording carries all four
	     readable QRs (camera L/R + strih burn + stream burn), none overlapping (#111
	     4-corner layout — replaces the old center-bottom ~700px burn that overlapped both
	     the camera QR and the other node's burn → strih→stream 0 paired frames).
	  4. Re-upload the composited buffer to a dynamic texture and gs_draw_sprite it as the
	     filter's output, so the burn flows downstream into the recording.
	NO libobs core change — this is purely a DistroAV plugin filter.

	Identity / gating:
	  - run_id: reserved per-node constant, env-overridable via OBS_BURN_RUN_ID. Defaults
	    to 911002 (strih); stream sets OBS_BURN_RUN_ID=911004. Both sit OUTSIDE cam2's
	    normal run_id range so #108 distinguishes node-stamp from cam2-stamp by run_id.
	  - frame_id: this filter's own per-render monotonic counter.
	  - gen_ts_ns: RAW render-instant wall-clock (burn_clock::gen_ts_ns, NOT boundary-
	    snapped) — shares the camera-box painter's RAW basis so cam→strih is bias-free (#108
	    finding #2). The genlock EMIT timecode (ndi-output.cpp) stays snapped; separate path.
	  - Gated behind OBS_BURN_QR (mirrors OBS_GENLOCK_WALL_CLOCK): default OFF, so the
	    production install is UNAFFECTED until #108 enables it on the dedicated PROBE scene.
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

#include <graphics/graphics.h>
#include <util/platform.h>

#include <atomic>
#include <cstdlib>
#include <cstring>
#include <string>

#define OBS_NDI_BURN_FILTER_ID "distroav_qr_burn_filter"
#define BURN_TEXFORMAT GS_BGRA

// Default reserved node run_ids (env-overridable). strih = 911002, stream = 911004 —
// both far OUTSIDE cam2's normal run_id range so #108 can tell node-stamp from cam2-stamp.
#define BURN_RUN_ID_DEFAULT_STRIH 911002u
#define BURN_RUN_ID_DEFAULT_STREAM 911004u

struct burn_filter {
	obs_source_t *context;

	// Composite resources (rebuilt on size change).
	gs_texrender_t *texrender;
	gs_stagesurf_t *stagesurface;
	gs_texture_t *out_texture;
	uint32_t known_width;
	uint32_t known_height;

	// CPU working buffer for the staged frame + QR composite.
	uint8_t *work;
	size_t work_size;

	// Identity / gating (resolved once at create).
	uint32_t run_id;
	bool enabled; // OBS_BURN_QR present -> burn; else transparent pass-through
	uint32_t qr_px;

	// Per-render monotonic frame counter.
	uint32_t frame_id;
};

// Read OBS_BURN_RUN_ID; fall back to the strih default if unset/invalid. The value is a
// reserved per-node constant — each box sets the env to its own reserved id.
static uint32_t resolve_run_id()
{
	const char *env = getenv("OBS_BURN_RUN_ID");
	if (env && *env) {
		char *end = nullptr;
		unsigned long v = strtoul(env, &end, 10);
		if (end && *end == '\0' && v <= 0xFFFFFFFFul)
			return (uint32_t)v;
	}
	return BURN_RUN_ID_DEFAULT_STRIH;
}

// OBS_BURN_QR present (any non-empty value) -> the burn is enabled. Default OFF so the
// production install is unaffected until #108 enables it on the PROBE scene.
static bool resolve_enabled()
{
	const char *env = getenv("OBS_BURN_QR");
	return env && *env;
}

// Desired burn QR pixel size; OBS_BURN_QR_PX overrides (default 300 — SMALLER than the
// camera dual-QR's ~700px so the two bottom-corner burns sit fully clear of the camera QRs
// (top band) and of each other, the #111 4-corner no-overlap layout). Clamped to a sane
// range so a bad env can't blow up the buffer math.
static uint32_t resolve_qr_px()
{
	const char *env = getenv("OBS_BURN_QR_PX");
	if (env && *env) {
		char *end = nullptr;
		unsigned long v = strtoul(env, &end, 10);
		if (end && *end == '\0' && v >= 64 && v <= 4096)
			return (uint32_t)v;
	}
	return 700u;
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
	f->run_id = resolve_run_id();
	f->enabled = resolve_enabled();
	f->qr_px = resolve_qr_px();
	f->frame_id = 0;

	obs_log(LOG_INFO,
		"[burn] filter created: enabled=%s run_id=%u qr_px=%u (env OBS_BURN_QR=%s, "
		"OBS_BURN_RUN_ID default %u/%u strih/stream)",
		f->enabled ? "yes" : "no(pass-through)", f->run_id, f->qr_px,
		f->enabled ? "set" : "unset", BURN_RUN_ID_DEFAULT_STRIH, BURN_RUN_ID_DEFAULT_STREAM);
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
	if (f->stagesurface) {
		gs_stagesurface_destroy(f->stagesurface);
		f->stagesurface = nullptr;
	}
	if (f->out_texture) {
		gs_texture_destroy(f->out_texture);
		f->out_texture = nullptr;
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

// (Re)allocate the composite resources for a new frame size.
static bool burn_ensure_size(burn_filter *f, uint32_t width, uint32_t height)
{
	if (f->known_width == width && f->known_height == height && f->texrender && f->stagesurface &&
	    f->out_texture && f->work)
		return true;

	// Called from video_render with the graphics context already held: destroy + recreate
	// directly (no redundant obs_enter_graphics — the libobs idiom, matches ndi-filter.cpp
	// which calls gs_stagesurface_create inside video_render without entering the context).
	burn_destroy_gfx_locked(f);
	if (f->work) {
		bfree(f->work);
		f->work = nullptr;
	}

	f->texrender = gs_texrender_create(BURN_TEXFORMAT, GS_ZS_NONE);
	f->stagesurface = gs_stagesurface_create(width, height, BURN_TEXFORMAT);
	f->out_texture = gs_texture_create(width, height, BURN_TEXFORMAT, 1, nullptr, GS_DYNAMIC);
	f->work_size = (size_t)width * height * 4;
	f->work = (uint8_t *)bmalloc(f->work_size);

	if (!f->texrender || !f->stagesurface || !f->out_texture || !f->work) {
		obs_log(LOG_ERROR, "[burn] failed to allocate composite resources for %ux%u", width,
			height);
		return false;
	}
	f->known_width = width;
	f->known_height = height;
	obs_log(LOG_DEBUG, "[burn] (re)allocated composite resources for %ux%u", width, height);
	return true;
}

// Burn THIS node's QR into the CPU buffer `buf` (BGRA, w x h, stride w*4) at THIS node's
// bottom corner (strih=bottom-left, stream=bottom-right) at ~300px — fully clear of the
// camera dual-QR (top band) and of the other node's burn (the other bottom corner), the
// #111 4-corner no-overlap layout, so one stream recording carries all four readable QRs.
static void burn_draw_qr(burn_filter *f, uint8_t *buf, uint32_t w, uint32_t h)
{
	const double fps = []() {
		obs_video_info ovi;
		if (obs_get_video_info(&ovi) && ovi.fps_den > 0)
			return (double)ovi.fps_num / (double)ovi.fps_den;
		return 0.0;
	}();

	const uint32_t fid = f->frame_id++;
	const int64_t gen_ts_ns = burn_clock::gen_ts_ns(fps);
	const std::string payload = burn_payload::encode(f->run_id, fid, gen_ts_ns);

	// Bottom strip: vertically centered at h - qr_px/2 - margin, horizontally centered.
	const uint32_t margin = 40;
	uint32_t band_cy = (h > f->qr_px / 2 + margin) ? (h - f->qr_px / 2 - margin) : (h / 2);
	burn_qr::render(buf, w * 4, w, h, payload, 0, w, band_cy, f->qr_px);

	if ((fid % 300u) == 0u) // throttled: one log line / ~10s @ 30fps
		obs_log(LOG_INFO, "[burn] burned QR run_id=%u frame_id=%u gen_ts_ns=%lld (%.3f fps)",
			f->run_id, fid, (long long)gen_ts_ns, fps);
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

	// Disabled (default on prod): transparent pass-through, zero overhead beyond a render.
	if (!f->enabled) {
		obs_source_skip_video_filter(f->context);
		return;
	}

	const uint32_t width = obs_source_get_width(f->context);
	const uint32_t height = obs_source_get_height(f->context);
	if (width == 0 || height == 0) {
		obs_source_skip_video_filter(f->context);
		return;
	}
	if (!burn_ensure_size(f, width, height)) {
		obs_source_skip_video_filter(f->context);
		return;
	}

	// 1) Render the target into our texrender.
	gs_texrender_reset(f->texrender);
	if (!gs_texrender_begin(f->texrender, width, height)) {
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
	// The texrender stays cleared/transparent and we still burn the QR onto it — only a
	// MISCONFIGURATION (the probe scene attaches this filter to a real program source, not
	// a bare source), so this path is cosmetic, not the intended use.
	if (target == parent)
		obs_source_skip_video_filter(f->context);
	else
		obs_source_video_render(target);
	gs_blend_state_pop();
	gs_texrender_end(f->texrender);

	// 2) Stage the rendered texture to a CPU BGRA buffer.
	gs_stage_texture(f->stagesurface, gs_texrender_get_texture(f->texrender));
	uint8_t *mapped = nullptr;
	uint32_t map_linesize = 0;
	if (!gs_stagesurface_map(f->stagesurface, &mapped, &map_linesize)) {
		obs_source_skip_video_filter(f->context);
		return;
	}

	// Copy row-by-row into a tightly-packed (stride = w*4) work buffer (the QR renderer
	// assumes a tight stride; the staged surface may be padded).
	const uint32_t tight = width * 4;
	for (uint32_t y = 0; y < height; ++y)
		memcpy(f->work + (size_t)y * tight, mapped + (size_t)y * map_linesize, tight);
	gs_stagesurface_unmap(f->stagesurface);

	// 3) Burn the QR into the CPU copy.
	burn_draw_qr(f, f->work, width, height);

	// 4) Re-upload and draw the composited frame as the filter's output.
	gs_texture_set_image(f->out_texture, f->work, tight, false);

	gs_effect_t *def = obs_get_base_effect(OBS_EFFECT_DEFAULT);
	gs_eparam_t *image = def ? gs_effect_get_param_by_name(def, "image") : nullptr;
	if (!def || !image) {
		// Default effect unavailable (graphics-reset window): pass the source through
		// rather than crash the graphics thread (the burn is dropped for this frame).
		obs_log(LOG_WARNING, "[burn] default effect/param unavailable; passing frame through");
		obs_source_skip_video_filter(f->context);
		return;
	}

	// Draw sRGB-correctly, exactly as libobs render_filter_tex (obs-source.c) does: under
	// OBS's default linear-sRGB pipeline the texture MUST be bound via the _srgb setter and
	// the framebuffer-sRGB state enabled, or the composited program video is drawn with the
	// wrong gamma (washed/shifted colors) downstream into the recording.
	const bool linear_srgb = gs_get_linear_srgb();
	const bool prev_srgb = gs_framebuffer_srgb_enabled();
	gs_enable_framebuffer_srgb(linear_srgb);
	if (linear_srgb)
		gs_effect_set_texture_srgb(image, f->out_texture);
	else
		gs_effect_set_texture(image, f->out_texture);
	while (gs_effect_loop(def, "Draw"))
		gs_draw_sprite(f->out_texture, 0, width, height);
	gs_enable_framebuffer_srgb(prev_srgb);
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
	return info;
}
