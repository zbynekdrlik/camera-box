/******************************************************************************
	#111 — QR render-time burn clock (gen_ts_ns timebase).

	The burned `gen_ts_ns` MUST share cam2's timebase so #108's per-hop subtraction
	(`node_stamp − cam2_gen_ts`) is valid: epoch NANOSECONDS on the wall clock
	(DanteSync-disciplined on strih/stream), snapped to the strictly-next 1/fps frame
	boundary. This is the SAME basis + boundary math as the camera-box painter's
	`gen_ts_ns` (src/probe/painter.rs `wall_now_ns` + `next_wall_boundary_ns`) and the
	DistroAV output emit-timecode (ndi-output.cpp), just expressed in ns (the painter's
	unit) rather than 100ns (the NDI timecode unit).

	Pure pacing math is split out (no <chrono>) so it can be unit-tested for parity with
	the Rust `next_wall_boundary_ns`. The wall-clock read is a thin <chrono> wrapper.
******************************************************************************/

#pragma once

#include <cstdint>

namespace burn_clock {

// Strictly-next wall-clock frame boundary at or after `now_ns`, for a frame period of
// `period_ns`. EXACT port of Rust `next_wall_boundary_ns` (src/probe/painter.rs):
//   (now_ns / period_ns + 1) * period_ns
// "Strictly next" (an exact boundary advances one full period) keeps two equal-rate
// cadences from emitting two ids at the same instant. `period_ns == 0` (fps 0) is
// guarded — returns `now_ns` (zero alignment) rather than dividing by zero.
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

// Boundary-snapped emit timestamp (epoch ns) for a frame at `fps` — the value burned
// into the QR as gen_ts_ns. Mirrors painter.rs: snap wall_now_ns() to the next 1/fps
// boundary.
inline int64_t gen_ts_ns(double fps)
{
	return next_wall_boundary_ns(wall_now_ns(), period_ns_for_fps(fps));
}

} // namespace burn_clock
#endif // BURN_CLOCK_NO_WALL
