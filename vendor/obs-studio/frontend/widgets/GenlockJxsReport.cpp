/* camera-box issue 1387 -- the JPEG XS indicator's report (see GenlockJxsReport.hpp). Plain std C++:
 * no Qt, no OBS, so the gate compiles and runs this file as shipped. */
#include "GenlockJxsReport.hpp"
#include "GenlockJxsState.hpp"

#include <algorithm>
#include <cinttypes>
#include <cstdio>

namespace {

genlock_jxs_entry_t genlock_jxs_entry_of(const GenlockJxsRow &r)
{
	genlock_jxs_entry_t e;
	e.kind = r.kind;
	e.age_ms = r.age_ms;
	e.window = r.window ? 1 : 0;
	e.fps_x100 = r.fps_x100;
	e.lost = r.lost;
	e.undecodable = r.undecodable;
	e.dropped = r.dropped;
	e.errors = r.errors;
	e.report_age_ms = r.report_age_ms;
	return e;
}

const char *genlock_jxs_health_name(genlock_jxs_health_t h)
{
	switch (h) {
	case GENLOCK_JXS_HEALTH_OK:
		return "OK";
	case GENLOCK_JXS_HEALTH_DEGRADED:
		return "DEGRADED";
	case GENLOCK_JXS_HEALTH_DOWN:
		return "DOWN";
	}
	return "?";
}

/* The operator's words for a non-OK reason (the label). */
const char *genlock_jxs_reason_text(genlock_jxs_reason_t r)
{
	switch (r) {
	case GENLOCK_JXS_REASON_OUTPUT_DOWN:
		return "program output sends nothing";
	case GENLOCK_JXS_REASON_ALL_INPUTS_DOWN:
		return "no input receives";
	case GENLOCK_JXS_REASON_INPUT_DOWN:
		return "input receives nothing";
	case GENLOCK_JXS_REASON_OUTPUT_DEGRADED:
		return "program output losing";
	case GENLOCK_JXS_REASON_INPUT_DEGRADED:
		return "input losing";
	case GENLOCK_JXS_REASON_NONE:
	case GENLOCK_JXS_REASON_NO_JXS:
		break;
	}
	return "";
}

std::string genlock_jxs_fixed(uint64_t x100)
{
	char buf[32];
	snprintf(buf, sizeof buf, "%" PRIu64 ".%02" PRIu64, x100 / 100u, x100 % 100u);
	return buf;
}

std::string genlock_jxs_seconds(int64_t ms)
{
	char buf[32];
	snprintf(buf, sizeof buf, "%.1f s", (double)ms / 1000.0);
	return buf;
}

/* "8 bpp" or "6-8 bpp" over the rows with a window (whole bpp, rounded); "" with none. */
std::string genlock_jxs_bpp_text(const std::vector<GenlockJxsRow> &rows)
{
	bool any = false;
	uint32_t lo = 0, hi = 0;
	for (const GenlockJxsRow &r : rows) {
		if (!r.window || r.age_ms < 0)
			continue;
		const uint32_t b = (r.bpp_x100 + 50u) / 100u;
		lo = any ? std::min(lo, b) : b;
		hi = any ? std::max(hi, b) : b;
		any = true;
	}
	if (!any)
		return "";
	return lo == hi ? std::to_string(lo) + " bpp" : std::to_string(lo) + "-" + std::to_string(hi) + " bpp";
}

std::string genlock_jxs_tooltip_row(const GenlockJxsRow &r, genlock_jxs_health_t h)
{
	std::string t = r.kind == GENLOCK_JXS_KIND_OUTPUT ? "out '" : "in '";
	t += r.name + "'";
	if (!r.detail.empty())
		t += " (" + r.detail + ")";
	t += ": ";
	t += r.age_ms < 0 ? "no frame yet" : "last frame " + genlock_jxs_seconds(r.age_ms) + " ago";
	if (r.window) {
		t += ", " + genlock_jxs_fixed(r.fps_x100) + " fps, " + genlock_jxs_fixed(r.bpp_x100) + " bpp";
		if (r.kind == GENLOCK_JXS_KIND_INPUT)
			t += ", lost slices " + std::to_string(r.lost) + ", undecodable " + std::to_string(r.undecodable);
		t += ", dropped " + std::to_string(r.dropped) + ", errors " + std::to_string(r.errors);
		if (r.kind == GENLOCK_JXS_KIND_OUTPUT)
			t += r.report_age_ms < 0 ? ", no receiver report"
						 : ", receiver report " + genlock_jxs_seconds(r.report_age_ms) + " ago";
	} else {
		t += ", no 5 s window yet";
	}
	t += " -- ";
	t += genlock_jxs_health_name(h);
	return t;
}

} // namespace

GenlockJxsReport genlock_jxs_report(const std::vector<GenlockJxsRow> &rows)
{
	GenlockJxsReport rep;
	std::vector<genlock_jxs_health_t> health;
	health.reserve(rows.size());
	genlock_jxs_counts_t c = {0, 0, 0, 0, 0, 0};
	for (const GenlockJxsRow &r : rows) {
		const genlock_jxs_entry_t e = genlock_jxs_entry_of(r);
		const genlock_jxs_health_t h = genlock_jxs_entry_health(&e);
		health.push_back(h);
		if (r.kind == GENLOCK_JXS_KIND_OUTPUT) {
			c.n_outputs++;
			c.n_outputs_down += h == GENLOCK_JXS_HEALTH_DOWN ? 1 : 0;
			c.n_outputs_degraded += h == GENLOCK_JXS_HEALTH_DEGRADED ? 1 : 0;
		} else {
			c.n_inputs++;
			c.n_inputs_down += h == GENLOCK_JXS_HEALTH_DOWN ? 1 : 0;
			c.n_inputs_degraded += h == GENLOCK_JXS_HEALTH_DEGRADED ? 1 : 0;
		}
	}
	genlock_jxs_reason_t reason = GENLOCK_JXS_REASON_NONE;
	const genlock_jxs_state_t state = genlock_jxs_decide(&c, &reason);
	rep.state = (int)state;
	rep.reason = (int)reason;
	rep.n_inputs = c.n_inputs;
	rep.n_inputs_down = c.n_inputs_down;
	rep.n_inputs_degraded = c.n_inputs_degraded;
	rep.n_outputs = c.n_outputs;
	rep.n_outputs_down = c.n_outputs_down;
	rep.n_outputs_degraded = c.n_outputs_degraded;

	/* the first row behind the reason, in the table's order (kind, then name) */
	int want_kind = -1;
	genlock_jxs_health_t want_health = GENLOCK_JXS_HEALTH_OK;
	switch (reason) {
	case GENLOCK_JXS_REASON_OUTPUT_DOWN:
		want_kind = GENLOCK_JXS_KIND_OUTPUT;
		want_health = GENLOCK_JXS_HEALTH_DOWN;
		break;
	case GENLOCK_JXS_REASON_ALL_INPUTS_DOWN:
	case GENLOCK_JXS_REASON_INPUT_DOWN:
		want_kind = GENLOCK_JXS_KIND_INPUT;
		want_health = GENLOCK_JXS_HEALTH_DOWN;
		break;
	case GENLOCK_JXS_REASON_OUTPUT_DEGRADED:
		want_kind = GENLOCK_JXS_KIND_OUTPUT;
		want_health = GENLOCK_JXS_HEALTH_DEGRADED;
		break;
	case GENLOCK_JXS_REASON_INPUT_DEGRADED:
		want_kind = GENLOCK_JXS_KIND_INPUT;
		want_health = GENLOCK_JXS_HEALTH_DEGRADED;
		break;
	case GENLOCK_JXS_REASON_NONE:
	case GENLOCK_JXS_REASON_NO_JXS:
		break;
	}
	for (size_t i = 0; want_kind >= 0 && i < rows.size(); i++) {
		if (rows[i].kind == want_kind && health[i] == want_health) {
			rep.offender = rows[i].name;
			break;
		}
	}

	const std::string counts = std::to_string(c.n_inputs) + " in / " + std::to_string(c.n_outputs) + " out";
	if (state == GENLOCK_JXS_OFF) {
		rep.label = "JPEG XS: off";
		rep.colour = "#95a5a6"; /* grey */
	} else if (state == GENLOCK_JXS_OK) {
		const std::string bpp = genlock_jxs_bpp_text(rows);
		rep.label = "JPEG XS: " + counts + (bpp.empty() ? "" : ", " + bpp);
		rep.colour = "#2ecc71"; /* green, the LOCK widget's */
	} else {
		rep.label = std::string("JPEG XS: ") + genlock_jxs_state_name(state) + " " +
			    genlock_jxs_reason_text(reason) + (rep.offender.empty() ? "" : ": " + rep.offender) +
			    " (" + counts + ")";
		rep.colour = state == GENLOCK_JXS_DOWN ? "#e74c3c" /* red */ : "#f1c40f" /* amber */;
	}

	rep.tooltip = std::string("JPEG XS: ") + genlock_jxs_state_name(state);
	if (rows.empty())
		rep.tooltip += "\nno JPEG XS source and no running JPEG XS output";
	for (size_t i = 0; i < rows.size(); i++)
		rep.tooltip += "\n  " + genlock_jxs_tooltip_row(rows[i], health[i]);

	char line[256];
	snprintf(line, sizeof line,
		 "jxs-state: state=%s reason=%s inputs=%d down=%d degraded=%d outputs=%d down=%d degraded=%d entry=",
		 genlock_jxs_state_name(state), genlock_jxs_reason_key(reason), c.n_inputs, c.n_inputs_down,
		 c.n_inputs_degraded, c.n_outputs, c.n_outputs_down, c.n_outputs_degraded);
	rep.log_line = line;
	rep.log_line += rep.offender.empty() ? "-" : "'" + rep.offender + "'";
	rep.log_line += " (issue 1387)";
	return rep;
}
