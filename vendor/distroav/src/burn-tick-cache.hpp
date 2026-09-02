/******************************************************************************
	camera-box #1260 — pure, dependency-free within-tick "prepare once, reuse"
	decision for the DistroAV QR burn filter (ndi-burn-filter.cpp).

	The burn filter's video_render runs once per DRAW of its parent source — the
	PROGRAM mix + (Studio-Mode) preview + every Multiview cell. Doing the full base
	texrender + QR raster/upload per draw meant strih's 4K Multiview re-ran the burn
	for all 7 cam sources every MV frame, pushing the MV render_ewma over the per-tick
	budget (obs-display-budget.h, #278/#293) and collapsing the MV to 30/(K+1)=7.5 fps
	while the program render stayed healthy (issue 1260).

	This is an IDIOMATIC per-tick render cache, not a novel one (review 🔵-6): stock
	OBS filters that go through obs_source_process_filter_begin already cache their
	target render per tick — filter_texrender is gs_texrender_reset ONCE per tick in
	obs_source_video_tick (obs-source.c), and gs_texrender_begin early-returns on an
	already-rendered texrender (texture-render.c). The #404 overlay burn filter drives
	its OWN texrender and called gs_texrender_reset every DRAW, opting out of that
	per-tick cache; this restores the per-tick semantics for it. (DistroAV's aux
	ndi_filter already uses the same shape via its own `f->rendered` flag.)

	This state machine does the expensive prep + advances the burn frame_id EXACTLY
	ONCE per video tick (the first draw — NORMALLY the program, since output_frames()
	runs before render_displays(); the DistroAV preview NDI output is an earlier
	main-render callback so a program+preview cam can prep in the preview draw, a
	bounded low-ms gen_ts bias only — see the filter's tick_cache field 🟡-1 note),
	and lets the later within-tick draws REUSE the cached base texrender + QR texture
	(a cheap sprite blit). burn_filter_videotick() clears prepared_this_tick once per
	tick (a benign plain-bool race, 🔵-2 in the filter); the first render sets it.

	Tier-0 authority: src/burn_tick_cache.rs::BurnTickCache (byte-identical results,
	proven by the C-parity harness in tests/burn_tick_cache_parity.rs). That Rust
	module has no PRODUCTION consumer (review 🔵-3) — it exists solely as the parity
	authority + the local RED->GREEN Tier-0 seam the supervisor asked for; the parity
	test compiles THESE shipped bytes, so the mirror can never silently drift. Kept in
	its own header (no libobs deps — only <stdbool.h>) so the exact decision the
	shipped filter uses is compiled + unit-checked from a standalone harness, not
	buried in video_render where CI is its only compile.
******************************************************************************/

#pragma once

#include <stdbool.h>

// Per-filter-instance within-tick prepare/reuse state. bzalloc'd with the filter, so all-zero
// (prepared_this_tick == false) is the correct fresh state: the first render of the tick prepares.
// (review 🔵-3: no separate _init helper — bzalloc IS the constructor; the Rust authority's new()
// is the mirror of that all-zero state.)
struct burn_tick_cache {
	bool prepared_this_tick;
};

// Called once per video tick (the filter's video_tick): invalidate the cached composite so the
// next render re-preps + re-stamps the burn for the new frame.
static inline void burn_tick_cache_on_tick(struct burn_tick_cache *c)
{
	c->prepared_this_tick = false;
}

// Called at the start of each render's draw (after the burn-enabled + resources gates). Returns
// true iff this render must do the EXPENSIVE prep (base texrender + QR raster + upload + advance
// frame_id); false iff it may REUSE the cached composite. Exactly ONE render per tick returns true.
static inline bool burn_tick_cache_on_render(struct burn_tick_cache *c)
{
	if (c->prepared_this_tick)
		return false;
	c->prepared_this_tick = true;
	return true;
}

// A prep that FAILED (a transient graphics-reset window) must not leave the tick marked prepared —
// else a later within-tick draw would reuse an unprepared/stale composite. Re-arm the next draw.
static inline void burn_tick_cache_abort_prepare(struct burn_tick_cache *c)
{
	c->prepared_this_tick = false;
}
