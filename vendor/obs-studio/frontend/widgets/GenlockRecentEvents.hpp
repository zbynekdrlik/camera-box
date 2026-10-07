// GenlockRecentEvents.hpp — camera-box issue 1302
//
// The state the in-OBS genlock LOCK indicator (OBSBasicStatusBar) keeps between its 1 Hz ticks to
// decide `recent_event`: a per-input event BASELINE. Each input remembers its phase total
// (relocks + late_holds + backward_steps) and whether it contributed (connected and not idle) at the
// last tick, so a reconnect, a wake from idle or a first sight re-baselines instead of counting the
// input's whole lifetime total as new events (the #1299 aggregate compare did, and held the box
// DEGRADED recent_event for 60 s after every reattach).
//
// Plain std C++ (no OBS/Qt) so tests/genlock_phase_baseline_1302.rs compiles it with the widget's
// tick function and replays the widget's ticks on the real bytes. The tick itself
// (genlock_recent_events_tick) lives in OBSBasicStatusBar.cpp next to the scan, because it calls
// the C decision helpers of GenlockLockState.hpp, which this header must not pull into every TU
// that includes the status bar.
#pragma once

#include <cstdint>
#include <deque>
#include <map>
#include <string>
#include <utility>

/* what the widget remembers about ONE genlock input between its ticks */
struct GenlockPhaseBaseline {
	uint64_t total = 0;        /* the input's phase total at the last tick (genlock_input_phase_events) */
	bool contributing = false; /* it was connected and not idle at the last tick */
	/* (monotonic ms, NEW phase events) of the input's ticks that saw new events, within the window --
	 * the top recent-event offender is the input with the most new events here */
	std::deque<std::pair<int64_t, uint64_t>> recent;
};

/* the widget's whole recent_event state */
struct GenlockRecentEvents {
	/* input name -> its baseline; an input that leaves the scan is forgotten, so its return is a
	 * first sight (re-baselined, never a burst of old events) */
	std::map<std::string, GenlockPhaseBaseline> inputs;
	int64_t last_event_ms = -1; /* monotonic ms of the last tick that saw a new event; -1 = never */
};
