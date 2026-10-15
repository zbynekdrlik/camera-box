// GenlockRecentEvents.hpp — camera-box issue 1302
//
// The recent_event driver of the in-OBS genlock LOCK indicator (OBSBasicStatusBar): a per-input
// event BASELINE. Each input remembers its phase total (relocks + late_holds + backward_steps) and
// whether it contributed (connected and not idle) at the last 1 Hz tick, so a reconnect, a wake from
// idle or a first sight re-baselines instead of counting the input's whole lifetime total as new
// events (the #1299 aggregate compare did, and held the box DEGRADED recent_event for 60 s after
// every reattach). The same unit keeps the #1341 idle classification ring, which decides which
// inputs contribute at all: an input is UNCLASSIFIED after a (re)connect or first sight, LIVE once
// it delivers a live rate (genlock_idle_classify_tick).
//
// Plain std C++ (no OBS/Qt). The tick lives in its own translation unit, GenlockRecentEvents.cpp,
// which calls the parity-gated C rules of GenlockLockState.hpp; this header does not include that
// one, so every TU that includes the status bar sees only these std structs.
// tests/genlock_phase_baseline_1302.rs compiles GenlockRecentEvents.cpp with g++ and replays the
// widget's ticks on the shipped bytes.
#pragma once

#include <cstdint>
#include <deque>
#include <map>
#include <string>
#include <utility>
#include <vector>

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

/* one genlock input as the tick reads it (the widget copies it out of its scan) */
struct GenlockPhaseInput {
	std::string name;
	bool connected = true; /* the DistroAV receiver has a live NDI connection */
	bool idle = false;     /* #1341: connected but keep-alive-only */
	uint64_t relocks = 0;
	uint64_t late_holds = 0;
	uint64_t backward_steps = 0;
};

/* what one tick produced */
struct GenlockRecentEventTick {
	uint64_t new_events = 0;   /* the new phase events of every input this tick */
	bool recent_event = false; /* a tick within window_ms saw a new event */
	std::string top_name;      /* the input with the most new events in the window ("" = none) */
	uint64_t top_events = 0;   /* its new events in the window */
};

/* One 1 Hz tick: each input's NEW phase events from the parity-gated genlock_input_new_phase_events
 * against what `st` remembered at the last tick; recent_event = a new event within window_ms; the top
 * offender is the input with the most new events in that window (a tie keeps the first in scan
 * order). Inputs no longer in `inputs` are forgotten. */
GenlockRecentEventTick genlock_recent_events_tick(GenlockRecentEvents &st, int64_t now_ms, int64_t window_ms,
						   const std::vector<GenlockPhaseInput> &inputs);

/* issue 1302 + #1341: what the widget remembers about ONE connected genlock input's received frames */
struct GenlockRxRing {
	/* (monotonic ms, cumulative frames_received) within the idle window, oldest first */
	std::deque<std::pair<int64_t, uint64_t>> samples;
	/* the input's class at the last tick: genlock_input_idle_class_t of GenlockLockState.hpp
	 * (0 UNCLASSIFIED, 1 LIVE, 2 IDLE) */
	int idle_class = 0;
};

/* the widget's whole idle-classification state: input name -> its ring. An input that is not connected
 * or leaves the scan is forgotten, so its return is a first sight (UNCLASSIFIED). */
struct GenlockIdleClassifier {
	std::map<std::string, GenlockRxRing> inputs;
};

/* one genlock input as the idle classification reads it */
struct GenlockRxInput {
	std::string name;
	bool connected = true;        /* the DistroAV receiver has a live NDI connection */
	uint64_t frames_received = 0; /* cumulative frames queued onto the FIFO (obs_genlock_stats) */
};

/* One 1 Hz tick of the idle classification: each connected input's ring takes this tick's sample (a
 * received counter that went backward clears the ring and the class: a reconnect), is pruned to the
 * idle window, and the parity-gated genlock_input_idle_class decides from its span, its frame delta and
 * the previous class. Returns each input's class in scan order (a disconnected input reads UNCLASSIFIED
 * and is forgotten). Only a LIVE input is graded and feeds recent_event; the widget gives UNCLASSIFIED
 * and IDLE inputs the idle path. */
std::vector<int> genlock_idle_classify_tick(GenlockIdleClassifier &st, int64_t now_ms,
					    const std::vector<GenlockRxInput> &inputs);
