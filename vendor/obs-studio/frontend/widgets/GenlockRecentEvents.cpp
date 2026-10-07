// GenlockRecentEvents.cpp — camera-box issue 1302: the per-input recent-event tick of the in-OBS
// genlock LOCK indicator (see GenlockRecentEvents.hpp). Plain std C++ + the parity-gated C rules of
// GenlockLockState.hpp; no OBS/Qt, so tests/genlock_phase_baseline_1302.rs compiles THIS file with
// g++ and replays the widget's ticks on it.
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
