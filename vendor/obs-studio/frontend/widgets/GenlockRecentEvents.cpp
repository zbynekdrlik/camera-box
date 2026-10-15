// GenlockRecentEvents.cpp — camera-box issue 1302: the per-input recent-event tick and the idle
// classification ring of the in-OBS genlock LOCK indicator (see GenlockRecentEvents.hpp). Plain std
// C++ + the parity-gated C rules of GenlockLockState.hpp; no OBS/Qt, so
// tests/genlock_phase_baseline_1302.rs and tests/genlock_idle_class_1302.rs compile THIS file with
// g++ and replay the widget's ticks on it.
#include "GenlockRecentEvents.hpp"
#include "GenlockLockState.hpp"

#include <cstdint>
#include <set>
#include <string>
#include <vector>

static uint64_t genlock_phase_sat_add(uint64_t a, uint64_t b)
{
	return a > UINT64_MAX - b ? UINT64_MAX : a + b;
}

GenlockRecentEventTick genlock_recent_events_tick(GenlockRecentEvents &st, int64_t now_ms, int64_t window_ms,
						   const std::vector<GenlockPhaseInput> &inputs)
{
	GenlockRecentEventTick tick;
	std::set<std::string> present;
	for (const GenlockPhaseInput &in : inputs) {
		const int contributing = (in.connected && !in.idle) ? 1 : 0;
		const uint64_t total = genlock_input_phase_events(in.connected ? 1 : 0, in.idle ? 1 : 0, in.relocks,
								  in.late_holds, in.backward_steps);
		const auto found = st.inputs.find(in.name);
		const int has_prev = found != st.inputs.end() ? 1 : 0;
		GenlockPhaseBaseline &b = has_prev ? found->second : st.inputs[in.name];
		const uint64_t fresh =
			genlock_input_new_phase_events(has_prev, b.contributing ? 1 : 0, b.total, contributing, total);
		b.total = total;
		b.contributing = contributing != 0;
		if (fresh > 0) {
			b.recent.emplace_back(now_ms, fresh);
			tick.new_events = genlock_phase_sat_add(tick.new_events, fresh);
		}
		while (!b.recent.empty() && now_ms - b.recent.front().first >= window_ms)
			b.recent.pop_front();
		uint64_t windowed = 0;
		for (const auto &e : b.recent)
			windowed = genlock_phase_sat_add(windowed, e.second);
		/* strictly more: a tie keeps the first input in scan order */
		if (windowed > tick.top_events) {
			tick.top_events = windowed;
			tick.top_name = in.name;
		}
		present.insert(in.name);
	}
	/* bound the remembered state: an input that left the scan is forgotten, so its return is a first
	 * sight (re-baselined), never a burst of the events it collected while away. */
	for (auto it = st.inputs.begin(); it != st.inputs.end();) {
		if (present.count(it->first) == 0)
			it = st.inputs.erase(it);
		else
			++it;
	}
	if (tick.new_events > 0)
		st.last_event_ms = now_ms;
	tick.recent_event = st.last_event_ms >= 0 && now_ms - st.last_event_ms < window_ms;
	return tick;
}

std::vector<int> genlock_idle_classify_tick(GenlockIdleClassifier &st, int64_t now_ms,
					    const std::vector<GenlockRxInput> &inputs)
{
	std::vector<int> classes;
	classes.reserve(inputs.size());
	std::set<std::string> present;
	for (const GenlockRxInput &in : inputs) {
		if (!in.connected) {
			/* an absent input is n_absent, never classified; its ring is forgotten below */
			classes.push_back(GENLOCK_INPUT_UNCLASSIFIED);
			continue;
		}
		present.insert(in.name);
		GenlockRxRing &ring = st.inputs[in.name];
		if (!ring.samples.empty() && in.frames_received < ring.samples.back().second) {
			/* the received counter went backward: a reconnect -- re-baseline and re-classify */
			ring.samples.clear();
			ring.idle_class = GENLOCK_INPUT_UNCLASSIFIED;
		}
		ring.samples.emplace_back(now_ms, in.frames_received);
		while (ring.samples.size() > 1 && now_ms - ring.samples.front().first > GENLOCK_IDLE_WINDOW_MS)
			ring.samples.pop_front();
		const int64_t span_ms = ring.samples.back().first - ring.samples.front().first;
		const uint64_t delta_frames = ring.samples.back().second - ring.samples.front().second;
		ring.idle_class = (int)genlock_input_idle_class(span_ms, delta_frames,
								(genlock_input_idle_class_t)ring.idle_class);
		classes.push_back(ring.idle_class);
	}
	/* bound the remembered state: a ring whose input is not connected this tick is forgotten, so its
	 * return is a first sight (UNCLASSIFIED). */
	for (auto it = st.inputs.begin(); it != st.inputs.end();) {
		if (present.count(it->first) == 0)
			it = st.inputs.erase(it);
		else
			++it;
	}
	return classes;
}
