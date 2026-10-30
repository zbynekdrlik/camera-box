/* camera-box issue 1387 -- the JPEG XS indicator's report: the rows DistroAV publishes (one per JPEG
 * XS source and running program output) in, the label, its colour, the tooltip and the jxs-state:
 * line out. Plain std C++ (no Qt, no OBS) so tests/genlock_jxs_indicator_1387.rs compiles and runs
 * the SHIPPED file; the widget (GenlockJxsWidget.cpp) only reads the table and shows this. The C
 * decision it grades with (GenlockJxsState.hpp) is included by the .cpp only. */
#pragma once

#include <cstdint>
#include <string>
#include <vector>

struct GenlockJxsRow {
	int kind = 0; /* 0 input, 1 output (genlock_jxs_kind_t) */
	std::string name;
	std::string detail;
	int64_t age_ms = -1; /* since the row's last frame, -1 = none yet */
	bool window = false;
	uint32_t fps_x100 = 0;
	uint32_t bpp_x100 = 0;
	uint64_t lost = 0;
	uint64_t undecodable = 0;
	uint64_t dropped = 0;
	uint64_t errors = 0;
	int64_t report_age_ms = -1;
};

struct GenlockJxsReport {
	int state = 0;  /* genlock_jxs_state_t */
	int reason = 0; /* genlock_jxs_reason_t */
	int n_inputs = 0;
	int n_inputs_down = 0;
	int n_inputs_degraded = 0;
	int n_outputs = 0;
	int n_outputs_down = 0;
	int n_outputs_degraded = 0;
	std::string offender; /* the first row behind the reason, "" for OFF / OK */
	std::string label;
	std::string colour;
	std::string tooltip;
	std::string log_line; /* the jxs-state: line (the widget logs it on a change) */
};

/* `unreadable`: the proc handler answered but its table did not parse -- DEGRADED
 * (table_unreadable), never the grey OFF a box without JPEG XS shows. */
GenlockJxsReport genlock_jxs_report(const std::vector<GenlockJxsRow> &rows, bool unreadable = false);

/* The jxs-state: line's change key: the widget logs a report only when its state, reason or the
 * row behind it differs from the last one logged. */
struct GenlockJxsLogKey {
	int state = -1;
	int reason = -1;
	std::string offender;
};

/* True (and the key moves to this report) when the report must be logged. */
bool genlock_jxs_log_due(GenlockJxsLogKey &key, const GenlockJxsReport &rep);
