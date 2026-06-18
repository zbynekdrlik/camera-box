/******************************************************************************
	#111 — QR render-time burn clock (gen_ts_ns timebase).

	The burned `gen_ts_ns` MUST share cam2's timebase so #108's per-hop subtraction
	(`node_stamp − cam2_gen_ts`) is valid: epoch NANOSECONDS on the wall clock
	(DanteSync-disciplined on strih/stream), read RAW at render time.

	BASIS PARITY WITH THE CAMERA-BOX PAINTER (finding #2 — bias-free).
	The cam2 painter stamps the RAW wall clock at paint time: src/probe/painter.rs
	stamps `clock_ns()` (== `SystemTime::now()` epoch ns) into the QR `gen_ts_ns`. Its
	`next_wall_boundary_ns` is used ONLY to pick the next *sleep* target (pacing); it is
	NOT applied to the stamp the QR carries. So the burn's `gen_ts_ns` is likewise the
	RAW `wall_now_ns()` — NOT boundary-snapped. Snapping the burn against a raw cam2
	stamp would inject a systematic ~½-frame offset (~16.7 ms @ 30 fps) plus up to a
	full-frame of quantization jitter into cam→strih. Sharing the RAW basis on both
	sides is what makes the per-hop number genuinely bias-free.

	NOT to be confused with the genlock EMIT timecode. The outgoing NDI frame's
	timecode (ndi-output.cpp `genlock_emit_timecode_100ns`) IS boundary-snapped — it
	must fall on the genlock grid the cameras emit on. That is a SEPARATE value on a
	SEPARATE path; this QR burn stamp does not touch it and it is unchanged by this fix.

	The pure boundary math below stays as a documented, unit-testable port of the Rust
	`next_wall_boundary_ns` (it describes the painter's pacing grid), but `gen_ts_ns`
	deliberately does NOT call it. The wall-clock read is a thin <chrono> wrapper.
******************************************************************************/

#pragma once

#include <cstdint>

namespace burn_clock {

// Strictly-next wall-clock frame boundary at or after `now_ns`, for a frame period of
// `period_ns`. EXACT port of Rust `next_wall_boundary_ns` (src/probe/painter.rs):
//   (now_ns / period_ns + 1) * period_ns
// Retained as a documented description of the painter's PACING grid (the cadence the
// painter sleeps to). The QR `gen_ts_ns` stamp does NOT apply it — see the file header
// (finding #2): the painter stamps the RAW wall clock, so the burn must too.
//
// The Rust mirror computes in u64 (wraps in release, never UB). Real epoch-ns values
// (~1.7e18) are far below i64::MAX so the multiply never overflows in practice — but to
// keep the function well-defined (no signed-overflow UB) for ANY input AND bit-identical
// to Rust's u64 path, the divide/add/multiply are done in uint64_t (well-defined modular
// arithmetic), then cast back. now_ns/period_ns are non-negative here (period_ns>0 by the
// guard; now_ns is a wall-clock epoch ns, always >= 0 in operation).
inline int64_t next_wall_boundary_ns(int64_t now_ns, int64_t period_ns)
{
	if (period_ns <= 0)
		return now_ns;
	const uint64_t now = static_cast<uint64_t>(now_ns);
	const uint64_t period = static_cast<uint64_t>(period_ns);
	return static_cast<int64_t>((now / period + 1) * period);
}

// Frame period in ns for `fps` (rounded). fps <= 0 -> 0 (no alignment).
inline int64_t period_ns_for_fps(double fps)
{
	if (fps <= 0.0)
		return 0;
	// 1e9 ns / fps, rounded to the nearest ns.
	return static_cast<int64_t>(1000000000.0 / fps + 0.5);
}

} // namespace burn_clock

// The live wall-clock read pulls in <chrono>; gate it so the pure pacing math above
// can be compiled freestanding by the parity test harness without linking chrono.
#ifndef BURN_CLOCK_NO_WALL
#include <chrono>

namespace burn_clock {

// Current wall clock in ns since the Unix epoch. std::chrono::system_clock is the OS
// wall clock (DanteSync owns it on strih/stream); its epoch is the Unix epoch on all
// supported platforms — the SAME epoch as Rust's SystemTime::now()/UNIX_EPOCH.
inline int64_t wall_now_ns()
{
	using namespace std::chrono;
	return duration_cast<nanoseconds>(system_clock::now().time_since_epoch()).count();
}

// RAW render-instant emit timestamp (epoch ns) for a frame — the value burned into the
// QR as gen_ts_ns. Shares the camera-box painter's RAW basis (painter.rs stamps
// `clock_ns()`, NOT the pacing boundary), so cam→strih = burn_gen_ts − cam2_gen_ts is
// bias-free (finding #2). `fps` is accepted for call-site symmetry but is deliberately
// unused: the stamp is NOT boundary-snapped (snapping would re-introduce the ~½-frame
// bias this fix removes). The genlock EMIT timecode that DOES snap lives separately in
// ndi-output.cpp and is unchanged.
inline int64_t gen_ts_ns(double /*fps*/)
{
	return wall_now_ns();
}

} // namespace burn_clock
#endif // BURN_CLOCK_NO_WALL
