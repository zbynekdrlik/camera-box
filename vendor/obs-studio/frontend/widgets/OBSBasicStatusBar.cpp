#include "OBSBasicStatusBar.hpp"
#include "ui_StatusBarWidget.h"
#include "GenlockLockState.hpp"

#include <widgets/OBSBasic.hpp>

#include <util/platform.h>

#include <QLabel>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QTimer>
#include <QUrl>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <map>
#include <set>
#include <string>
#include <vector>

#include "moc_OBSBasicStatusBar.cpp"

static constexpr int bitrateUpdateSeconds = 2;
static constexpr int congestionUpdateSeconds = 4;
static constexpr float excellentThreshold = 0.0f;
static constexpr float goodThreshold = 0.3333f;
static constexpr float mediocreThreshold = 0.6667f;
static constexpr float badThreshold = 1.0f;

/* camera-box #1298: the in-OBS GENLOCK lock indicator. The DECISION itself lives in the pure,
 * Tier-0-tested + C-vs-Rust-parity-gated GenlockLockState.hpp; everything here is the OBS glue
 * that scalarises the libobs genlock stats + the dantesync clock facet into genlock_lock_facets_t
 * and renders the verdict.
 *
 * camera-box #1299 Part 4 + #1357 scope C: the qpc_drift term is the wall STEP only, NOT the
 * cumulative wall-vs-QPC offset (it grew ~50 ms/h on a dantesync-disciplined Windows box before issue 1372 and
 * false-paged the whole fleet overnight 15./16.9.) and NOT a rate: on Linux CLOCK_MONOTONIC is
 * kernel-disciplined, so the measured rate is 0 by construction, while on Windows it was the free QPC
 * crystal (since issue 1372 the Windows os_gettime_ns runs at the disciplined rate too) — the removed rate-vs-instantaneous-slew check meant a different thing per box and
 * false-DEGRADED both (28 samples on strih-lx, 4 on stream, 24.9.2026, none a step). We DEGRADE only on
 * a single-sample STEP > one 30 fps frame (GENLOCK_QPC_STEP_BOUND_MS); the pure decision is
 * genlock_qpc_drift_beyond_bound in GenlockLockState.hpp. The windowed rate (GENLOCK_QPC_WINDOW_S is
 * long enough that the integer-ms cumulative drift resolves it) and the dantesync slew stay
 * report-only JSON telemetry. */
static constexpr int64_t GENLOCK_QPC_STEP_BOUND_MS = 33;
static constexpr int GENLOCK_QPC_WINDOW_S = 300;
/* camera-box issue 1372: a single-sample wall step up to this size is a coordinated dantesync fleet
 * DATE step (dantesync 1.9.0 bounds the date error at 50 ms, so a step is <= ~50 ms, about every
 * 1.8 h) and is BOOKED (genlock_qpc_wall_step_rebase_ms): the qpc history is re-baselined by it and
 * the widget stays LOCKED, because the media clock deliberately never follows a step and the render
 * tick re-grids onto the stepped wall in one tick. A bigger jump (a clock set) or a second step inside
 * GENLOCK_QPC_WINDOW_S (a step storm) still DEGRADES. */
static constexpr int64_t GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS = 200;
static constexpr int64_t GENLOCK_QPC_WALL_STEPS_PER_WINDOW = 1;

/* camera-box issue 1372 part D: the MEDIA-clock (audio clock) term. os_gettime_ns() paces the audio
 * mixer and every output; since part A it runs at the dantesync-disciplined rate on Windows too, so the
 * wall-vs-media offset must stay flat on every box apart from wall steps. The widget samples that offset
 * itself in us each tick (libobs' wall_qpc_drift_ms is integer ms truncated toward zero, which cannot
 * tell a dantesync phase step from a rate). Over GENLOCK_MEDIA_CLOCK_WINDOW_S the median per-pair rate is
 * the centre; a pair whose change is off the centre's prediction by more than GENLOCK_MEDIA_CLOCK_BAND_US
 * is a wall STEP (its deviation IS the step; dantesync requests steps of >= 200/500 us, a Windows step
 * can land up to a timer tick short, an unbiased <= 100 us remnant) and is left out, and the rate is the
 * time-weighted rate of the kept pairs -- a drift in only part of the seconds (up to ~95 ppm on ~1 s
 * pairs) counts at its true share. Scaled to the window it DEGRADES beyond
 * GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US (3.3 ppm), and so does a Windows fallback to raw QPC while
 * dantesync answers. A pair more than GENLOCK_MEDIA_CLOCK_MAX_GAP_MS apart (a stalled UI) is not a
 * sample; the window is ready once the counted pairs cover >= 90 % of it. Calibration: the
 * undisciplined stream mixer drifted ~8 ms per 10 min (67 ms / 83 min), a disciplined box stays at 0.
 * The pure decision is in GenlockLockState.hpp (parity-gated). Never UNLOCKED. */
static constexpr int GENLOCK_MEDIA_CLOCK_WINDOW_S = 600;
static constexpr int64_t GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US = 2000;
static constexpr int64_t GENLOCK_MEDIA_CLOCK_MAX_GAP_MS = 5000;
static constexpr int64_t GENLOCK_MEDIA_CLOCK_BAND_US = 100;

/* camera-box #1303: the A/V pairing-offset bound that trips the audio DEGRADE term. An
 * audio-ENABLED genlock source whose |audio_pairing_offset_ms| (from obs_genlock_stats v2)
 * exceeds this — i.e. its audio is not held to match its video FIFO latency, which also catches a
 * deep-latency source whose hold never fired (offset = -latency_ms) — degrades the indicator. One
 * 30 fps frame (33 ms) is the coarsest frame interval on the fleet, so a sub-frame mispairing (the
 * inaudible 3 ms-floor case) never false-degrades. Audio disabled/absent never trips it (guarded by
 * audio_enabled). This is the widget-side reduction feeding GenlockFacets.audio_unpaired, mirroring
 * the pairing-offset branch of camera_box::genlock_audio_pairing::decide_audio_health. */
static constexpr int64_t GENLOCK_AUDIO_PAIRING_BOUND_MS = 33;

/* camera-box #1299: the machine-readable genlock-lock-json: line is emitted on every state/reason
 * change AND at least this often (the 1 Hz timer -> ~30 s), so bundle-state's #1222 bounded TAIL
 * always holds a fresh one even on a box whose state has not changed since startup. */
static constexpr int GENLOCK_JSON_HEARTBEAT_TICKS = 30;

/* camera-box #1341: a CONNECTED genlock input whose received-frame DELTA over the idle window is
 * below this floor is IDLE (keep-alive-only) — excluded from n_locked/n_connected and contributing 0
 * phase events, so an idle SongPlayer playlist input (~1 frame / 11 s -> ~5 frames / 60 s) never
 * flaps the box DEGRADED/recent_event, while a live source (>= 23.98 fps -> >= 1400 frames / 60 s)
 * clears it by a wide margin. */
static constexpr uint64_t GENLOCK_IDLE_INPUT_MIN_FRAMES = 60;
/* The window (ms) the received-frame delta is measured over — the SAME 60 s window recent_event
 * uses; an input is classified idle only once its per-input sample ring spans >= 90 % of it (the
 * qpc rate_ready precedent), so a live source is never mislabelled idle during the first ~54 s. */
static constexpr qint64 GENLOCK_IDLE_WINDOW_MS = 60000;

namespace {
/* camera-box #1299: the structured per-input record the genlock-lock-json: line carries (the
 * tooltip `rows` above are pre-formatted human strings; this is the machine-readable sibling). */
struct GenlockInputRow {
	std::string name;
	bool locked = false;
	bool connected = true; /* #1299: DistroAV receiver has a live NDI connection (sender running) */
	bool idle = false;     /* #1341: connected but keep-alive-only (received-frame rate below the idle floor over the window) */
	uint32_t latency_ms = 0;
	uint64_t frames_received = 0; /* #1341: cumulative video frames queued onto the FIFO (obs_genlock_stats.frames_received) — the idle received-rate signal */
	uint64_t underruns = 0;
	uint64_t relocks = 0;
	uint64_t late_holds = 0;
	uint64_t backward_steps = 0; /* #1299 Part 3: phase-event class (with relocks+late_holds), NOT underruns */
	uint32_t depth = 0;
};
struct GenlockScan {
	int n_inputs = 0;
	int n_locked = 0;
	int n_absent = 0; /* #1299: of n_inputs, how many have NO live NDI receiver connection */
	int n_idle = 0;   /* #1341: of n_inputs, how many are CONNECTED but IDLE (keep-alive-only) — computed post-scan from the received-frame delta */
	quint64 event_sum = 0;
	int64_t max_abs_qpc_drift_ms = 0;
	int64_t qpc_signed_ms = 0; /* #1299 Part 4: the SIGNED cumulative wall-vs-QPC drift (process-global, so every input reports the same value; last wins) — feeds the windowed-rate ring */
	uint32_t min_latency_ms = 0;
	uint32_t max_latency_ms = 0;
	bool any_input = false;
	bool audio_unpaired = false;                    /* #1303: an audio-enabled genlock source is unpaired with its video FIFO hold */
	bool audio_unexpected = false;                  /* #1303: a source is audible when the certified per-box table expects it silent */
	std::vector<std::string> unlocked_names;
	std::vector<std::string> audio_unpaired_names;  /* #1303: which sources tripped the audio pairing bound */
	std::vector<std::string> audio_unexpected_names;/* #1303: silent-by-contract sources that are audible (double-audio hazard) */
	std::vector<std::string> rows; /* per-input tooltip rows */
	std::vector<GenlockInputRow> inputs; /* #1299 per-input machine-readable records */
};
struct GenlockOutScan {
	bool present = false;
	bool stamping = false;
};

bool genlock_scan_source(void *param, obs_source_t *source)
{
	auto *scan = static_cast<GenlockScan *>(param);
	struct obs_genlock_stats st;
	if (!obs_source_get_genlock_stats(source, &st) || !st.genlock_fifo)
		return true;
	scan->n_inputs++;
	if (st.locked)
		scan->n_locked++;
	/* #1299 Part 3: the recent-event driver (scan->event_sum) is NO LONGER summed here — an
	 * incremental sum over EVERY input + EVERY event class (incl. underruns + absent-sender rebind
	 * churn) latched the 60 s window forever. It is recomputed AFTER the scan as the CONNECTED-only,
	 * PHASE-only aggregate via genlock_input_phase_events (see UpdateGenlockLabel). */
	const int64_t d = st.wall_qpc_drift_ms < 0 ? -st.wall_qpc_drift_ms : st.wall_qpc_drift_ms;
	if (d > scan->max_abs_qpc_drift_ms)
		scan->max_abs_qpc_drift_ms = d;
	/* #1299 Part 4: keep the SIGNED cumulative drift for the windowed rate (all inputs report the same
	 * process-global value, so a plain assignment — last input wins — is correct). */
	scan->qpc_signed_ms = st.wall_qpc_drift_ms;
	if (!scan->any_input || st.latency_ms < scan->min_latency_ms)
		scan->min_latency_ms = st.latency_ms;
	if (!scan->any_input || st.latency_ms > scan->max_latency_ms)
		scan->max_latency_ms = st.latency_ms;
	scan->any_input = true;
	const char *name = obs_source_get_name(source);
	const std::string nm = name ? name : "?";
	/* #1299: an input whose NDI sender is not running is ABSENT (idle), not unlocked. Exclude it
	 * from the DEGRADED gate (counted in n_absent -> n_connected) and from unlocked_names so the
	 * reason text never blames a senderless input. connected defaults true for a pre-v3 stats snapshot
	 * (an old libobs), so this can only ADD suppression, never mask a real degrade. */
	const bool connected = (st.version >= 3) ? st.connected : true;
	if (!connected)
		scan->n_absent++;
	if (!st.locked && connected)
		scan->unlocked_names.push_back(nm);
	/* #1303: the audio DEGRADE term — an audio-ENABLED source whose A/V pairing offset breaches the
	 * bound (v2 stats only; audio disabled/absent never trips it). Mirrors the pairing-offset branch
	 * of genlock_audio_pairing::decide_audio_health, reduced to one aggregate facet exactly as the
	 * qpc-drift bound above reduces to qpc_drift_beyond_bound. */
	if (st.version >= 2 && st.audio_enabled) {
		const int64_t aoff = st.audio_pairing_offset_ms < 0 ? -st.audio_pairing_offset_ms
								    : st.audio_pairing_offset_ms;
		if (aoff > GENLOCK_AUDIO_PAIRING_BOUND_MS) {
			scan->audio_unpaired = true;
			scan->audio_unpaired_names.push_back(nm);
		}
	}
	/* #1303: the audio-UNEXPECTED term (box-class-agnostic subset) — a source that is AUDIBLE
	 * (ndi_audio on) when it is silent-by-contract per the certified per-box audio table. A CAMERA
	 * input is silent-by-contract on EVERY box (genlock_forced_table_audit::is_camera_input), so this
	 * needs no box identity and can never false-DEGRADE a correctly-configured box (cameras are
	 * forced ndi_audio=false); an audible camera is the double-audio hazard. The box-class-DEPENDENT
	 * cases (a non-camera audible on a Dante-fed box; a program source silent on the cg box) need a
	 * box-role marker and stay owned by the deploy-time #1303 part-4 preflight (a followup). */
	if (st.version >= 2 && st.audio_enabled && genlock_name_is_camera(nm.c_str())) {
		scan->audio_unexpected = true;
		scan->audio_unexpected_names.push_back(nm);
	}
	char row[320];
	snprintf(row, sizeof(row),
		 "%s: %s  latency=%u ms  depth=%zu  underruns=%llu relocks=%llu late=%llu", nm.c_str(),
		 st.locked ? "LOCKED" : "UNLOCKED", st.latency_ms, st.depth, (unsigned long long)st.underruns,
		 (unsigned long long)st.relocks, (unsigned long long)st.late_holds);
	scan->rows.emplace_back(row);
	/* #1299: the structured sibling of the tooltip row above (same snapshot). */
	GenlockInputRow rec;
	rec.name = nm;
	rec.locked = st.locked;
	rec.connected = connected;
	rec.frames_received = st.frames_received; /* #1341: the idle received-rate signal */
	rec.latency_ms = st.latency_ms;
	rec.underruns = st.underruns;
	rec.relocks = st.relocks;
	rec.late_holds = st.late_holds;
	rec.backward_steps = st.backward_steps; /* #1299 Part 3: feeds genlock_input_phase_events */
	rec.depth = (uint32_t)st.depth;
	scan->inputs.push_back(std::move(rec));
	return true;
}

bool genlock_scan_output(void *param, obs_output_t *output)
{
	auto *scan = static_cast<GenlockOutScan *>(param);
	struct obs_genlock_output_stats st;
	if (!obs_output_get_genlock_stats(output, &st))
		return true;
	if (!st.is_genlock_output || !obs_output_active(output))
		return true; /* a stopped / non-genlock output: facet ABSENT, never forces UNLOCKED */
	scan->present = true;
	/* Optimistic OR across active genlock outputs: ANY one stamping marks the box as stamping.
	 * Correct for the single-2ME-PGM rig topology (one genlock NDI sender). If a SECOND genlock
	 * output is ever added, a non-stamping one would be hidden behind a stamping sibling here —
	 * revisit to an ALL-must-stamp rule at that point. */
	if (st.wall_timecode_stamping)
		scan->stamping = true;
	return true;
}

const char *genlock_state_name(genlock_lock_state_t s)
{
	switch (s) {
	case GENLOCK_LOCK_LOCKED:
		return "LOCKED";
	case GENLOCK_LOCK_DEGRADED:
		return "DEGRADED";
	default:
		return "UNLOCKED";
	}
}

/* A short, stable reason TOKEN for the genlock-lock: log line (greppable, de-dup key). */
const char *genlock_reason_key(genlock_lock_reason_t r)
{
	switch (r) {
	case GENLOCK_LOCK_REASON_NONE:
		return "none";
	case GENLOCK_LOCK_REASON_NO_GENLOCK:
		return "no_genlock";
	case GENLOCK_LOCK_REASON_CLOCK:
		return "clock";
	case GENLOCK_LOCK_REASON_OUTPUT:
		return "output";
	case GENLOCK_LOCK_REASON_NO_INPUT_LOCKED:
		return "no_input_locked";
	case GENLOCK_LOCK_REASON_INPUT_UNLOCKED:
		return "input_unlocked";
	case GENLOCK_LOCK_REASON_RECENT_EVENT:
		return "recent_event";
	case GENLOCK_LOCK_REASON_NTP_FAILED:
		return "ntp_failed";
	case GENLOCK_LOCK_REASON_AUDIO_PAIRING:
		return "audio_pairing";
	case GENLOCK_LOCK_REASON_AUDIO_UNEXPECTED:
		return "audio_unexpected";
	case GENLOCK_LOCK_REASON_MEDIA_CLOCK:
		return "media_clock";
	default:
		return "qpc_drift";
	}
}

/* camera-box issue 1372 part D: stable tokens for the media-clock verdict + the Windows discipline
 * outcome (the genlock-lock: line, the JSON facet and the tooltip). */
const char *genlock_media_clock_name(genlock_media_clock_t m)
{
	switch (m) {
	case GENLOCK_MEDIA_CLOCK_DRIFT:
		return "drift";
	case GENLOCK_MEDIA_CLOCK_UNDISCIPLINED:
		return "undisciplined";
	default:
		return "ok";
	}
}

const char *genlock_media_discipline_name(int d)
{
	switch (d) {
	case GENLOCK_MEDIA_DISCIPLINE_ACTIVE:
		return "active";
	case GENLOCK_MEDIA_DISCIPLINE_DISABLED:
		return "disabled";
	case GENLOCK_MEDIA_DISCIPLINE_READ_FAILED:
		return "read_failed";
	case GENLOCK_MEDIA_DISCIPLINE_API_MISSING:
		return "api_missing";
	case GENLOCK_MEDIA_DISCIPLINE_NOT_APPLICABLE:
		return "n/a";
	default:
		return "unknown";
	}
}

/* camera-box #1298: the human reason text the label appends (names the offending input). Moved out of
 * UpdateGenlockLabel unchanged when issue 1372 part D added the media-clock case, to keep that function
 * readable. */
QString genlock_reason_text(genlock_lock_reason_t reason, const GenlockScan &scan, bool clock_present,
			    long long qpc_max_step_ms, const std::string &recent_event_input_name,
			    genlock_media_clock_t media_clock, int64_t media_drift_us, int media_discipline)
{
	QString reasonText;
	switch (reason) {
	case GENLOCK_LOCK_REASON_NONE:
		break;
	case GENLOCK_LOCK_REASON_NO_GENLOCK:
		reasonText = "no genlock inputs";
		break;
	case GENLOCK_LOCK_REASON_CLOCK:
		reasonText = clock_present ? "clock not locked" : "no clock discipline";
		break;
	case GENLOCK_LOCK_REASON_OUTPUT:
		reasonText = "output not stamping";
		break;
	case GENLOCK_LOCK_REASON_NO_INPUT_LOCKED:
		reasonText = "no input locked";
		break;
	case GENLOCK_LOCK_REASON_INPUT_UNLOCKED:
		if (!scan.unlocked_names.empty()) {
			reasonText = QString::fromStdString(scan.unlocked_names.front());
			if (scan.unlocked_names.size() > 1)
				reasonText += QString(" +%1 more").arg(scan.unlocked_names.size() - 1);
		} else {
			reasonText = "input unlocked";
		}
		break;
	case GENLOCK_LOCK_REASON_RECENT_EVENT:
		/* #1299 Part 3: name the offending input (the connected input with the most phase events);
		 * "underrun" is no longer a recent-event class, so the text no longer says it. */
		if (!recent_event_input_name.empty())
			reasonText = QString("recent event: %1")
					     .arg(QString::fromStdString(recent_event_input_name));
		else
			reasonText = "recent event";
		break;
	case GENLOCK_LOCK_REASON_NTP_FAILED:
		reasonText = "clock NTP failed";
		break;
	case GENLOCK_LOCK_REASON_QPC_DRIFT:
		/* #1357: the verdict is the wall STEP only, so the label names the step size — never the
		 * unbounded cumulative offset or a (report-only) rate. */
		reasonText = QString("clock step %1 ms").arg(qpc_max_step_ms);
		break;
	case GENLOCK_LOCK_REASON_AUDIO_PAIRING:
		if (!scan.audio_unpaired_names.empty()) {
			reasonText = QString("audio unpaired: %1")
					     .arg(QString::fromStdString(scan.audio_unpaired_names.front()));
			if (scan.audio_unpaired_names.size() > 1)
				reasonText += QString(" +%1 more").arg(scan.audio_unpaired_names.size() - 1);
		} else {
			reasonText = "audio unpaired";
		}
		break;
	case GENLOCK_LOCK_REASON_MEDIA_CLOCK:
		/* issue 1372 part D: the audio (media) clock does not follow the disciplined wall clock. */
		if (media_clock == GENLOCK_MEDIA_CLOCK_UNDISCIPLINED)
			reasonText = QString("audio clock not disciplined (%1)")
					     .arg(genlock_media_discipline_name(media_discipline));
		else
			reasonText = QString("audio clock drift %1 ms/%2 min")
					     .arg(QString::number((double)media_drift_us / 1000.0, 'f', 1))
					     .arg(GENLOCK_MEDIA_CLOCK_WINDOW_S / 60);
		break;
	case GENLOCK_LOCK_REASON_AUDIO_UNEXPECTED:
		if (!scan.audio_unexpected_names.empty()) {
			reasonText = QString("audio unexpected: %1")
					     .arg(QString::fromStdString(scan.audio_unexpected_names.front()));
			if (scan.audio_unexpected_names.size() > 1)
				reasonText += QString(" +%1 more").arg(scan.audio_unexpected_names.size() - 1);
		} else {
			reasonText = "audio unexpected";
		}
		break;
	}
	return reasonText;
}

/* camera-box #1299: append *s* as a JSON string (quotes + minimal escaping) to *out*. Pure
 * std::string — NO obs_data dependency — so it lift-compiles under g++ and is Tier-0 testable
 * (tests/genlock_lock_json_guards.rs Facet B). Rig source names are ASCII ("NDI cam1", "mbc"),
 * but escape defensively so a stray quote/backslash/control char can never corrupt the line. */
void genlock_json_append_escaped(std::string &out, const char *s)
{
	out += '"';
	for (const char *p = s ? s : ""; *p; ++p) {
		const unsigned char c = (unsigned char)*p;
		switch (c) {
		case '"':
			out += "\\\"";
			break;
		case '\\':
			out += "\\\\";
			break;
		case '\n':
			out += "\\n";
			break;
		case '\r':
			out += "\\r";
			break;
		case '\t':
			out += "\\t";
			break;
		default:
			if (c < 0x20) {
				char u[8];
				snprintf(u, sizeof(u), "\\u%04x", c);
				out += u;
			} else {
				out += (char)c;
			}
		}
	}
	out += '"';
}

/* camera-box #1299: build the versioned genlock-lock-json: payload from the widget's ALREADY
 * decided verdict + the scalar facets it read this tick (so the fleet facet can never disagree
 * with the statusbar). clock_str/output_str are the SAME tokens the #1298 key=value line uses
 * (absent|locked|unlocked / absent|stamping|not-stamping). Pure — no Qt, no obs_data. */
std::string genlock_build_lock_json(const char *state_name, const char *reason_key, int n_inputs,
				    int n_locked, int n_absent, int n_idle, uint32_t latency_ms,
				    const char *clock_str,
				    const char *output_str, bool recent_event,
				    const char *recent_event_input_name, uint64_t recent_event_input_events,
				    int64_t qpc_drift_ms, double qpc_drift_ppm, double qpc_expected_ppm,
				    int qpc_step, const char *audio_unexpected_input_name,
				    const char *media_clock_state, int64_t media_clock_drift_us,
				    int media_clock_window_s, int media_clock_ready,
				    const char *media_clock_discipline,
				    const std::vector<GenlockInputRow> &inputs)
{
	/* #1299: schema v2 added top-level n_absent + per-input connected; Part 3 (v3) adds
	 * recent_event_inputs (the top recent-event offender name+count). #1303 (v4) adds
	 * audio_unexpected_inputs (a silent-by-contract source found audible). Part 4 (v5) adds the
	 * report-only windowed-drift telemetry qpc_drift_ppm / qpc_expected_ppm / qpc_step at the END.
	 * #1341 (v6) adds top-level n_idle + per-input idle (a connected-but-keep-alive-only input,
	 * excluded from the DEGRADED gate). Issue 1372 part D (v7) adds the media_clock object at the END
	 * ({state, drift_us, window_s, ready, discipline}). All additive: the bundle-state parser defaults
	 * n_absent->None, n_idle->None, connected->true, idle->false, the qpc_*_ppm trio->None, and OMITS
	 * recent_event_inputs / audio_unexpected_inputs / media_clock when absent/empty, so a v1..v6 line
	 * from an older build reads cleanly. */
	std::string j = "{\"v\":7,\"state\":";
	genlock_json_append_escaped(j, state_name);
	j += ",\"reason\":";
	genlock_json_append_escaped(j, reason_key);
	char num[128];
	snprintf(num, sizeof(num),
		 ",\"n_inputs\":%d,\"n_locked\":%d,\"n_absent\":%d,\"n_idle\":%d,\"latency_ms\":%u,", n_inputs,
		 n_locked, n_absent, n_idle, latency_ms);
	j += num;
	j += "\"clock\":";
	genlock_json_append_escaped(j, clock_str);
	j += ",\"output\":";
	genlock_json_append_escaped(j, output_str);
	j += ",\"recent_event\":";
	j += recent_event ? "true" : "false";
	/* #1299 Part 3: the top recent-event offender (a one-element list; empty when none), so a
	 * DEGRADED/recent_event page can name the input. The bundle-state parser omits an empty list. */
	j += ",\"recent_event_inputs\":[";
	if (recent_event_input_name && recent_event_input_name[0]) {
		j += "{\"name\":";
		genlock_json_append_escaped(j, recent_event_input_name);
		snprintf(num, sizeof(num), ",\"events\":%llu}", (unsigned long long)recent_event_input_events);
		j += num;
	}
	j += "]";
	/* #1303 (v4): the audio-unexpected offender (a one-element list; empty when none), so a
	 * DEGRADED/audio_unexpected page can name the audible silent-by-contract source. The parser omits
	 * an empty list, so a line without a real offender never fabricates an attribution. */
	j += ",\"audio_unexpected_inputs\":[";
	if (audio_unexpected_input_name && audio_unexpected_input_name[0]) {
		j += "{\"name\":";
		genlock_json_append_escaped(j, audio_unexpected_input_name);
		j += "}";
	}
	j += "]";
	snprintf(num, sizeof(num), ",\"qpc_drift_ms\":%lld,\"inputs\":[", (long long)qpc_drift_ms);
	j += num;
	bool first = true;
	for (const GenlockInputRow &r : inputs) {
		if (!first)
			j += ",";
		first = false;
		j += "{\"name\":";
		genlock_json_append_escaped(j, r.name.c_str());
		snprintf(num, sizeof(num), ",\"locked\":%s,\"connected\":%s,\"idle\":%s,\"latency_ms\":%u,",
			 r.locked ? "true" : "false", r.connected ? "true" : "false", r.idle ? "true" : "false",
			 r.latency_ms);
		j += num;
		snprintf(num, sizeof(num),
			 "\"underruns\":%llu,\"relocks\":%llu,\"late_holds\":%llu,\"depth\":%u}",
			 (unsigned long long)r.underruns, (unsigned long long)r.relocks,
			 (unsigned long long)r.late_holds, r.depth);
		j += num;
	}
	j += "]";
	/* #1299 Part 4 (v5): report-only windowed-drift telemetry at the END of the object. Since #1357 the
	 * qpc_drift VERDICT is the wall STEP only (qpc_step) — neither the rate nor the dantesync slew nor
	 * the cumulative qpc_drift_ms above gates. Additive: the parser defaults all three to None. */
	snprintf(num, sizeof(num), ",\"qpc_drift_ppm\":%.3f,\"qpc_expected_ppm\":%.3f,\"qpc_step\":%s",
		 qpc_drift_ppm, qpc_expected_ppm, qpc_step ? "true" : "false");
	j += num;
	/* issue 1372 part D (v7): the media-clock (audio clock) facet -- the verdict the decision used, the
	 * wall-vs-media drift per window (us; the time-weighted rate of the non-step pairs), the window length, whether it has filled, and the Windows
	 * discipline outcome ("n/a" on a box whose kernel disciplines the monotonic clock). */
	j += ",\"media_clock\":{\"state\":";
	genlock_json_append_escaped(j, media_clock_state);
	snprintf(num, sizeof(num), ",\"drift_us\":%lld,\"window_s\":%d,\"ready\":%s,\"discipline\":",
		 (long long)media_clock_drift_us, media_clock_window_s, media_clock_ready ? "true" : "false");
	j += num;
	genlock_json_append_escaped(j, media_clock_discipline);
	j += "}}";
	return j;
}
} // namespace

OBSBasicStatusBar::OBSBasicStatusBar(QWidget *parent)
	: QStatusBar(parent),
	  excellentPixmap(QIcon(":/res/images/network-excellent.svg").pixmap(QSize(16, 16))),
	  goodPixmap(QIcon(":/res/images/network-good.svg").pixmap(QSize(16, 16))),
	  mediocrePixmap(QIcon(":/res/images/network-mediocre.svg").pixmap(QSize(16, 16))),
	  badPixmap(QIcon(":/res/images/network-bad.svg").pixmap(QSize(16, 16))),
	  recordingActivePixmap(QIcon(":/res/images/recording-active.svg").pixmap(QSize(16, 16))),
	  recordingPausePixmap(QIcon(":/res/images/recording-pause.svg").pixmap(QSize(16, 16))),
	  streamingActivePixmap(QIcon(":/res/images/streaming-active.svg").pixmap(QSize(16, 16)))
{
	congestionArray.reserve(congestionUpdateSeconds);

	statusWidget = new StatusBarWidget(this);
	statusWidget->ui->delayInfo->setText("");
	statusWidget->ui->droppedFrames->setText(QTStr("DroppedFrames").arg("0", "0.0"));
	statusWidget->ui->statusIcon->setPixmap(inactivePixmap);
	statusWidget->ui->streamIcon->setPixmap(streamingInactivePixmap);
	statusWidget->ui->streamTime->setDisabled(true);
	statusWidget->ui->recordIcon->setPixmap(recordingInactivePixmap);
	statusWidget->ui->recordTime->setDisabled(true);
	statusWidget->ui->delayFrame->hide();
	statusWidget->ui->issuesFrame->hide();
	statusWidget->ui->kbps->hide();

	addPermanentWidget(statusWidget, 1);
	setMinimumHeight(statusWidget->height());

	UpdateIcons();
	connect(App(), &OBSApp::StyleChanged, this, &OBSBasicStatusBar::UpdateIcons);

	messageTimer = new QTimer(this);
	messageTimer->setSingleShot(true);
	connect(messageTimer, &QTimer::timeout, this, &OBSBasicStatusBar::clearMessage);

	clearMessage();

	/* camera-box #1298: the in-OBS GENLOCK lock indicator — a permanent statusbar label
	 * driven by an ALWAYS-ON 1 Hz timer (not the stream-only refreshTimer), so the operator
	 * sees the live lock state whether or not anything is streaming. This code ships only in
	 * the genlock fork frontend (never stock OBS). The clock facet is polled async from
	 * dantesync :8898 with a 500 ms transfer timeout, so the UI thread never blocks. */
	genlockClock.start();
	genlockClockNam = new QNetworkAccessManager(this);
	genlockLabel = new QLabel(this);
	genlockLabel->setTextFormat(Qt::PlainText);
	genlockLabel->setText("GENLOCK ● …");
	addPermanentWidget(genlockLabel, 0);
	UpdateGenlockLabel();
	genlockTimer = new QTimer(this);
	connect(genlockTimer, &QTimer::timeout, this, &OBSBasicStatusBar::UpdateGenlockLabel);
	genlockTimer->start(1000);
}

void OBSBasicStatusBar::Activate()
{
	if (!active) {
		refreshTimer = new QTimer(this);
		connect(refreshTimer, &QTimer::timeout, this, &OBSBasicStatusBar::UpdateStatusBar);

		int skipped = video_output_get_skipped_frames(obs_get_video());
		int total = video_output_get_total_frames(obs_get_video());

		totalStreamSeconds = 0;
		totalRecordSeconds = 0;
		lastSkippedFrameCount = 0;
		startSkippedFrameCount = skipped;
		startTotalFrameCount = total;

		refreshTimer->start(1000);
		active = true;

		if (streamOutput) {
			statusWidget->ui->statusIcon->setPixmap(inactivePixmap);
		}
	}

	if (streamOutput) {
		statusWidget->ui->streamIcon->setPixmap(streamingActivePixmap);
		statusWidget->ui->streamTime->setDisabled(false);
		statusWidget->ui->issuesFrame->show();
		statusWidget->ui->kbps->show();
		firstCongestionUpdate = true;
	}

	if (recordOutput) {
		statusWidget->ui->recordIcon->setPixmap(recordingActivePixmap);
		statusWidget->ui->recordTime->setDisabled(false);
	}
}

void OBSBasicStatusBar::Deactivate()
{
	OBSBasic *main = qobject_cast<OBSBasic *>(parent());
	if (!main) {
		return;
	}

	if (!streamOutput) {
		statusWidget->ui->streamTime->setText(QString("00:00:00"));
		statusWidget->ui->streamTime->setDisabled(true);
		statusWidget->ui->streamIcon->setPixmap(streamingInactivePixmap);
		statusWidget->ui->statusIcon->setPixmap(inactivePixmap);
		statusWidget->ui->delayFrame->hide();
		statusWidget->ui->issuesFrame->hide();
		statusWidget->ui->kbps->hide();
		totalStreamSeconds = 0;
		congestionArray.clear();
		disconnected = false;
		firstCongestionUpdate = false;
	}

	if (!recordOutput) {
		statusWidget->ui->recordTime->setText(QString("00:00:00"));
		statusWidget->ui->recordTime->setDisabled(true);
		statusWidget->ui->recordIcon->setPixmap(recordingInactivePixmap);
		totalRecordSeconds = 0;
	}

	if (main->outputHandler && !main->outputHandler->Active()) {
		delete refreshTimer;

		statusWidget->ui->delayInfo->setText("");
		statusWidget->ui->droppedFrames->setText(QTStr("DroppedFrames").arg("0", "0.0"));
		statusWidget->ui->kbps->setText("0 kbps");

		delaySecTotal = 0;
		delaySecStarting = 0;
		delaySecStopping = 0;
		reconnectTimeout = 0;
		active = false;
		overloadedNotify = true;

		statusWidget->ui->statusIcon->setPixmap(inactivePixmap);
	}
}

void OBSBasicStatusBar::UpdateDelayMsg()
{
	QString msg;

	if (delaySecTotal) {
		if (delaySecStarting && !delaySecStopping) {
			msg = QTStr("Basic.StatusBar.DelayStartingIn");
			msg = msg.arg(QString::number(delaySecStarting));

		} else if (!delaySecStarting && delaySecStopping) {
			msg = QTStr("Basic.StatusBar.DelayStoppingIn");
			msg = msg.arg(QString::number(delaySecStopping));

		} else if (delaySecStarting && delaySecStopping) {
			msg = QTStr("Basic.StatusBar.DelayStartingStoppingIn");
			msg = msg.arg(QString::number(delaySecStopping), QString::number(delaySecStarting));
		} else {
			msg = QTStr("Basic.StatusBar.Delay");
			msg = msg.arg(QString::number(delaySecTotal));
		}

		if (!statusWidget->ui->delayFrame->isVisible()) {
			statusWidget->ui->delayFrame->show();
		}

		statusWidget->ui->delayInfo->setText(msg);
	}
}

void OBSBasicStatusBar::UpdateBandwidth()
{
	if (!streamOutput) {
		return;
	}

	if (++seconds < bitrateUpdateSeconds) {
		return;
	}

	OBSOutput output = OBSGetStrongRef(streamOutput);
	if (!output) {
		return;
	}

	uint64_t bytesSent = obs_output_get_total_bytes(output);
	uint64_t bytesSentTime = os_gettime_ns();

	if (bytesSent < lastBytesSent) {
		bytesSent = 0;
	}
	if (bytesSent == 0) {
		lastBytesSent = 0;
	}

	uint64_t bitsBetween = (bytesSent - lastBytesSent) * 8;

	double timePassed = double(bytesSentTime - lastBytesSentTime) / 1000000000.0;

	double kbitsPerSec = double(bitsBetween) / timePassed / 1000.0;

	QString text;
	text += QString::number(kbitsPerSec, 'f', 0) + QString(" kbps");

	statusWidget->ui->kbps->setText(text);
	statusWidget->ui->kbps->setMinimumWidth(statusWidget->ui->kbps->width());

	if (!statusWidget->ui->kbps->isVisible()) {
		statusWidget->ui->kbps->show();
	}

	lastBytesSent = bytesSent;
	lastBytesSentTime = bytesSentTime;
	seconds = 0;
}

void OBSBasicStatusBar::UpdateCPUUsage()
{
	OBSBasic *main = qobject_cast<OBSBasic *>(parent());
	if (!main) {
		return;
	}

	QString text;
	text += QString("CPU: ") + QString::number(main->GetCPUUsage(), 'f', 1) + QString("%");

	statusWidget->ui->cpuUsage->setText(text);
	statusWidget->ui->cpuUsage->setMinimumWidth(statusWidget->ui->cpuUsage->width());

	UpdateCurrentFPS();
}

void OBSBasicStatusBar::UpdateCurrentFPS()
{
	struct obs_video_info ovi;
	obs_get_video_info(&ovi);
	float targetFPS = (float)ovi.fps_num / (float)ovi.fps_den;

	QString text = QString::asprintf("%.2f / %.2f FPS", obs_get_active_fps(), targetFPS);

	statusWidget->ui->fpsCurrent->setText(text);
	statusWidget->ui->fpsCurrent->setMinimumWidth(statusWidget->ui->fpsCurrent->width());
}

void OBSBasicStatusBar::UpdateStreamTime()
{
	totalStreamSeconds++;

	int seconds = totalStreamSeconds % 60;
	int totalMinutes = totalStreamSeconds / 60;
	int minutes = totalMinutes % 60;
	int hours = totalMinutes / 60;

	QString text = QString::asprintf("%02d:%02d:%02d", hours, minutes, seconds);
	statusWidget->ui->streamTime->setText(text);
	if (streamOutput && !statusWidget->ui->streamTime->isEnabled()) {
		statusWidget->ui->streamTime->setDisabled(false);
	}

	if (reconnectTimeout > 0) {
		QString msg = QTStr("Basic.StatusBar.Reconnecting")
				      .arg(QString::number(retries), QString::number(reconnectTimeout));
		showMessage(msg);
		disconnected = true;
		statusWidget->ui->statusIcon->setPixmap(disconnectedPixmap);
		congestionArray.clear();
		reconnectTimeout--;

	} else if (retries > 0) {
		QString msg = QTStr("Basic.StatusBar.AttemptingReconnect");
		showMessage(msg.arg(QString::number(retries)));
	}

	if (delaySecStopping > 0 || delaySecStarting > 0) {
		if (delaySecStopping > 0) {
			--delaySecStopping;
		}
		if (delaySecStarting > 0) {
			--delaySecStarting;
		}
		UpdateDelayMsg();
	}
}

extern volatile bool recording_paused;

void OBSBasicStatusBar::UpdateRecordTime()
{
	bool paused = os_atomic_load_bool(&recording_paused);

	if (!paused) {
		totalRecordSeconds++;

		if (recordOutput && !statusWidget->ui->recordTime->isEnabled()) {
			statusWidget->ui->recordTime->setDisabled(false);
		}
	} else {
		statusWidget->ui->recordIcon->setPixmap(streamPauseIconToggle ? recordingPauseInactivePixmap
									      : recordingPausePixmap);

		streamPauseIconToggle = !streamPauseIconToggle;
	}

	UpdateRecordTimeLabel();
}

void OBSBasicStatusBar::UpdateRecordTimeLabel()
{
	int seconds = totalRecordSeconds % 60;
	int totalMinutes = totalRecordSeconds / 60;
	int minutes = totalMinutes % 60;
	int hours = totalMinutes / 60;

	QString text = QString::asprintf("%02d:%02d:%02d", hours, minutes, seconds);
	if (os_atomic_load_bool(&recording_paused)) {
		text += QStringLiteral(" (PAUSED)");
	}

	statusWidget->ui->recordTime->setText(text);
}

void OBSBasicStatusBar::UpdateDroppedFrames()
{
	if (!streamOutput) {
		return;
	}

	OBSOutput output = OBSGetStrongRef(streamOutput);
	if (!output) {
		return;
	}

	int totalDropped = obs_output_get_frames_dropped(output);
	int totalFrames = obs_output_get_total_frames(output);
	double percent = (double)totalDropped / (double)totalFrames * 100.0;

	if (!totalFrames) {
		return;
	}

	QString text = QTStr("DroppedFrames");
	text = text.arg(QString::number(totalDropped), QString::number(percent, 'f', 1));
	statusWidget->ui->droppedFrames->setText(text);

	if (!statusWidget->ui->issuesFrame->isVisible()) {
		statusWidget->ui->issuesFrame->show();
	}

	/* ----------------------------------- *
	 * calculate congestion color          */

	float congestion = obs_output_get_congestion(output);
	float avgCongestion = (congestion + lastCongestion) * 0.5f;
	if (avgCongestion < congestion) {
		avgCongestion = congestion;
	}
	if (avgCongestion > 1.0f) {
		avgCongestion = 1.0f;
	}

	lastCongestion = congestion;

	if (disconnected) {
		return;
	}

	bool update = firstCongestionUpdate;
	float congestionOverTime = avgCongestion;

	if (congestionArray.size() >= congestionUpdateSeconds) {
		congestionOverTime = accumulate(congestionArray.begin(), congestionArray.end(), 0.0f) /
				     (float)congestionArray.size();
		congestionArray.clear();
		update = true;
	} else {
		congestionArray.emplace_back(avgCongestion);
	}

	if (update) {
		if (congestionOverTime <= excellentThreshold + EPSILON) {
			statusWidget->ui->statusIcon->setPixmap(excellentPixmap);
		} else if (congestionOverTime <= goodThreshold) {
			statusWidget->ui->statusIcon->setPixmap(goodPixmap);
		} else if (congestionOverTime <= mediocreThreshold) {
			statusWidget->ui->statusIcon->setPixmap(mediocrePixmap);
		} else if (congestionOverTime <= badThreshold) {
			statusWidget->ui->statusIcon->setPixmap(badPixmap);
		}

		firstCongestionUpdate = false;
	}
}

void OBSBasicStatusBar::OBSOutputReconnect(void *data, calldata_t *params)
{
	OBSBasicStatusBar *statusBar = static_cast<OBSBasicStatusBar *>(data);

	int seconds = (int)calldata_int(params, "timeout_sec");
	QMetaObject::invokeMethod(statusBar, "Reconnect", Q_ARG(int, seconds));
}

void OBSBasicStatusBar::OBSOutputReconnectSuccess(void *data, calldata_t *)
{
	OBSBasicStatusBar *statusBar = static_cast<OBSBasicStatusBar *>(data);

	QMetaObject::invokeMethod(statusBar, "ReconnectSuccess");
}

void OBSBasicStatusBar::Reconnect(int seconds)
{
	OBSBasic *main = qobject_cast<OBSBasic *>(parent());

	if (!retries) {
		main->SysTrayNotify(QTStr("Basic.SystemTray.Message.Reconnecting"), QSystemTrayIcon::Warning);
	}

	reconnectTimeout = seconds;

	if (streamOutput) {
		OBSOutput output = OBSGetStrongRef(streamOutput);
		if (!output) {
			return;
		}

		delaySecTotal = obs_output_get_active_delay(output);
		UpdateDelayMsg();

		retries++;
	}
}

void OBSBasicStatusBar::ReconnectClear()
{
	retries = 0;
	reconnectTimeout = 0;
	seconds = -1;
	lastBytesSent = 0;
	lastBytesSentTime = os_gettime_ns();
	delaySecTotal = 0;
	UpdateDelayMsg();
}

void OBSBasicStatusBar::ReconnectSuccess()
{
	OBSBasic *main = qobject_cast<OBSBasic *>(parent());

	QString msg = QTStr("Basic.StatusBar.ReconnectSuccessful");
	showMessage(msg, 4000);
	main->SysTrayNotify(msg, QSystemTrayIcon::Information);
	ReconnectClear();

	if (streamOutput) {
		OBSOutput output = OBSGetStrongRef(streamOutput);
		if (!output) {
			return;
		}

		delaySecTotal = obs_output_get_active_delay(output);
		UpdateDelayMsg();
		disconnected = false;
		firstCongestionUpdate = true;
	}
}

void OBSBasicStatusBar::UpdateStatusBar()
{
	OBSBasic *main = qobject_cast<OBSBasic *>(parent());

	UpdateBandwidth();

	if (streamOutput) {
		UpdateStreamTime();
	}

	if (recordOutput) {
		UpdateRecordTime();
	}

	UpdateDroppedFrames();

	int skipped = video_output_get_skipped_frames(obs_get_video());
	int total = video_output_get_total_frames(obs_get_video());

	skipped -= startSkippedFrameCount;
	total -= startTotalFrameCount;

	int diff = skipped - lastSkippedFrameCount;
	double percentage = double(skipped) / double(total) * 100.0;

	if (diff > 10 && percentage >= 0.1f) {
		showMessage(QTStr("HighResourceUsage"), 4000);
		if (!main->isVisible() && overloadedNotify) {
			main->SysTrayNotify(QTStr("HighResourceUsage"), QSystemTrayIcon::Warning);
			overloadedNotify = false;
		}
	}

	lastSkippedFrameCount = skipped;
}

void OBSBasicStatusBar::StreamDelayStarting(int sec)
{
	OBSBasic *main = qobject_cast<OBSBasic *>(parent());
	if (!main || !main->outputHandler) {
		return;
	}

	OBSOutputAutoRelease output = obs_frontend_get_streaming_output();
	streamOutput = OBSGetWeakRef(output);

	delaySecTotal = delaySecStarting = sec;
	UpdateDelayMsg();
	Activate();
}

void OBSBasicStatusBar::StreamDelayStopping(int sec)
{
	delaySecTotal = delaySecStopping = sec;
	UpdateDelayMsg();
}

void OBSBasicStatusBar::StreamStarted(obs_output_t *output)
{
	streamOutput = OBSGetWeakRef(output);

	streamSigs.emplace_back(obs_output_get_signal_handler(output), "reconnect", OBSOutputReconnect, this);
	streamSigs.emplace_back(obs_output_get_signal_handler(output), "reconnect_success", OBSOutputReconnectSuccess,
				this);

	retries = 0;
	lastBytesSent = 0;
	lastBytesSentTime = os_gettime_ns();
	Activate();
}

void OBSBasicStatusBar::StreamStopped()
{
	if (streamOutput) {
		streamSigs.clear();

		ReconnectClear();
		streamOutput = nullptr;
		clearMessage();
		Deactivate();
	}
}

void OBSBasicStatusBar::RecordingStarted(obs_output_t *output)
{
	recordOutput = OBSGetWeakRef(output);
	Activate();
}

void OBSBasicStatusBar::RecordingStopped()
{
	recordOutput = nullptr;
	Deactivate();
}

void OBSBasicStatusBar::RecordingPaused()
{
	if (recordOutput) {
		statusWidget->ui->recordIcon->setPixmap(recordingPausePixmap);
		streamPauseIconToggle = true;
	}

	UpdateRecordTimeLabel();
}

void OBSBasicStatusBar::RecordingUnpaused()
{
	if (recordOutput) {
		statusWidget->ui->recordIcon->setPixmap(recordingActivePixmap);
	}

	UpdateRecordTimeLabel();
}

static QPixmap GetPixmap(const QString &filename)
{
	QString path = obs_frontend_is_theme_dark() ? "theme:Dark/" : ":/res/images/";
	return QIcon(path + filename).pixmap(QSize(16, 16));
}

void OBSBasicStatusBar::UpdateIcons()
{
	disconnectedPixmap = GetPixmap("network-disconnected.svg");
	inactivePixmap = GetPixmap("network-inactive.svg");

	streamingInactivePixmap = GetPixmap("streaming-inactive.svg");

	recordingInactivePixmap = GetPixmap("recording-inactive.svg");
	recordingPauseInactivePixmap = GetPixmap("recording-pause-inactive.svg");

	bool streaming = obs_frontend_streaming_active();

	if (!streaming) {
		statusWidget->ui->streamIcon->setPixmap(streamingInactivePixmap);
		statusWidget->ui->statusIcon->setPixmap(inactivePixmap);
	} else {
		if (disconnected) {
			statusWidget->ui->statusIcon->setPixmap(disconnectedPixmap);
		}
	}

	bool recording = obs_frontend_recording_active();

	if (!recording) {
		statusWidget->ui->recordIcon->setPixmap(recordingInactivePixmap);
	}
}

void OBSBasicStatusBar::showMessage(const QString &message, int timeout)
{
	messageTimer->stop();

	statusWidget->ui->message->setText(message);

	if (timeout) {
		messageTimer->start(timeout);
	}
}

void OBSBasicStatusBar::clearMessage()
{
	statusWidget->ui->message->setText("");
}

/* camera-box #1298: poll the dantesync clock facet async (500 ms transfer timeout) so the UI
 * thread never blocks. A successful reply caches is_locked / ntp_failed + stamps
 * genlockClockLastOkMs; an error (timeout / connection refused) caches nothing, so the
 * staleness check in UpdateGenlockLabel flips clock_present false within ~3 s -> UNLOCKED
 * "no clock discipline" (meets the "flips within 5 s" acceptance). The reply is bound to `this`
 * so it auto-disconnects if the widget is destroyed while a poll is in flight. */
void OBSBasicStatusBar::PollGenlockClock()
{
	if (!genlockClockNam)
		return;
	QNetworkRequest req(QUrl("http://127.0.0.1:8898/status"));
	req.setTransferTimeout(500);
	QNetworkReply *reply = genlockClockNam->get(req);
	connect(reply, &QNetworkReply::finished, this, [this, reply]() {
		reply->deleteLater();
		if (reply->error() != QNetworkReply::NoError)
			return;
		const QByteArray body = reply->readAll();
		obs_data_t *d = obs_data_create_from_json(body.constData());
		if (!d)
			return;
		genlockClockLocked = obs_data_get_bool(d, "is_locked");
		genlockClockNtpFailed = obs_data_get_bool(d, "ntp_failed");
		/* #1299 Part 4: the slew the disciplined clock reports it is APPLYING to the wall clock vs the
		 * free crystal (f_ptp servo freq + f_phase slew integral, ppm). #1357: report-only telemetry
		 * (`qpc_expected_ppm` in the JSON) — it no longer feeds the qpc_drift verdict (the wall STEP
		 * does). Absent on an old dantesync -> 0.0. */
		genlockClockFptpPpm = obs_data_get_double(d, "f_ptp_ppm");
		genlockClockFphasePpm = obs_data_get_double(d, "f_phase_ppm");
		genlockClockLastOkMs = genlockClock.elapsed();
		obs_data_release(d);
	});
}

/* camera-box issue 1372 part D: one wall-minus-media offset sample in us. The wall clock (what libobs'
 * genlock_wall_now_ns reads: GetSystemTimePreciseAsFileTime on Windows, CLOCK_REALTIME elsewhere --
 * std::chrono::system_clock) is read between two os_gettime_ns() reads and compared with their
 * midpoint, retried while the bracket is wider than 50 us, so a preemption cannot bias the sample. */
static int64_t genlock_wall_minus_media_us()
{
	int64_t best_offset_us = 0;
	uint64_t best_width_ns = UINT64_MAX;
	for (int attempt = 0; attempt < 4; ++attempt) {
		const uint64_t media_before_ns = os_gettime_ns();
		const int64_t wall_ns = (int64_t)std::chrono::duration_cast<std::chrono::nanoseconds>(
						std::chrono::system_clock::now().time_since_epoch())
						.count();
		const uint64_t media_after_ns = os_gettime_ns();
		const uint64_t width_ns = media_after_ns - media_before_ns;
		if (width_ns < best_width_ns) {
			best_width_ns = width_ns;
			best_offset_us = (wall_ns - (int64_t)(media_before_ns + width_ns / 2)) / 1000;
		}
		if (width_ns <= 50000)
			break;
	}
	return best_offset_us;
}

/* camera-box issue 1372 part D: one tick of the media-clock (audio clock) term. Push this tick's
 * (monotonic ms, wall-minus-media offset us) sample into the GENLOCK_MEDIA_CLOCK_WINDOW_S ring, reduce it
 * with the parity-gated pure rate (wall steps left out, a stalled pair is not a sample), read the Windows discipline outcome (libobs os_gettime_discipline(); Linux has none to read --
 * its kernel disciplines CLOCK_MONOTONIC with the wall), and ask the pure verdict. The widget samples every tick, with or without
 * genlock inputs, so the ring has no gaps. */
OBSBasicStatusBar::GenlockMediaClockTick OBSBasicStatusBar::ReduceGenlockMediaClock(qint64 now_ms, bool clock_present)
{
	genlockMediaClockHistory.emplace_back(now_ms, genlock_wall_minus_media_us());
	while (genlockMediaClockHistory.size() > 1 &&
	       now_ms - genlockMediaClockHistory.front().first > (qint64)GENLOCK_MEDIA_CLOCK_WINDOW_S * 1000)
		genlockMediaClockHistory.pop_front();
	std::vector<int64_t> sample_ms;
	std::vector<int64_t> offset_us;
	std::vector<int64_t> rate_scratch(genlockMediaClockHistory.size());
	sample_ms.reserve(genlockMediaClockHistory.size());
	offset_us.reserve(genlockMediaClockHistory.size());
	for (const auto &sample : genlockMediaClockHistory) {
		sample_ms.push_back((int64_t)sample.first);
		offset_us.push_back(sample.second);
	}
	int64_t counted_ms = 0;
	const int64_t media_drift_us = genlock_media_clock_window_drift_us(
		sample_ms.data(), offset_us.data(), (int)offset_us.size(), GENLOCK_MEDIA_CLOCK_WINDOW_S,
		GENLOCK_MEDIA_CLOCK_MAX_GAP_MS, GENLOCK_MEDIA_CLOCK_BAND_US, rate_scratch.data(), &counted_ms);
	const int media_window_ready = genlock_media_clock_window_ready(counted_ms, GENLOCK_MEDIA_CLOCK_WINDOW_S);
#ifdef _WIN32
	const int media_discipline = os_gettime_discipline();
#else
	const int media_discipline = GENLOCK_MEDIA_DISCIPLINE_NOT_APPLICABLE;
#endif
	GenlockMediaClockTick tick;
	tick.verdict = (int)genlock_media_clock_verdict(media_window_ready, media_drift_us, GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US,
							media_discipline, clock_present ? 1 : 0);
	/* Not published (label, tooltip, JSON) until the window is ready: while it fills, one step pair is
	 * its own centre and would read as a huge rate. The verdict already requires ready. */
	tick.drift_us = media_window_ready ? media_drift_us : 0;
	tick.ready = media_window_ready;
	tick.discipline = media_discipline;
	return tick;
}

/* camera-box #1298: the 1 Hz indicator update. Scalarise the live libobs genlock stats + the
 * cached clock facet into genlock_lock_facets_t, ask the pure genlock_decide_lock_state() for
 * the verdict, and render it (green/amber/red + per-input tooltip). Emits a `genlock-lock:` log
 * line on every state/reason CHANGE (not every tick) so the lock history is greppable too. */
void OBSBasicStatusBar::UpdateGenlockLabel()
{
	if (!genlockLabel)
		return;

	/* kick off the async clock poll; we decide now from the PREVIOUS tick's cached result. */
	PollGenlockClock();

	GenlockScan scan;
	obs_enum_sources(genlock_scan_source, &scan);
	GenlockOutScan out;
	obs_enum_outputs(genlock_scan_output, &out);

	const qint64 now_ms = genlockClock.elapsed();

	/* #1341: classify each CONNECTED input as IDLE (keep-alive-only) from its received-frame DELTA
	 * over the same 60 s window recent_event uses. An idle SongPlayer playlist input keeps a live
	 * NDI connection but sends ~1 frame / 11 s, so its FIFO re-acquires a boundary on each keep-alive
	 * frame (a relock) — which would falsely feed recent_event and DEGRADE the box. An idle input is
	 * excluded from n_locked + n_connected and contributes 0 phase events. A per-input sample ring
	 * (name -> (monotonic ms, cumulative frames_received)) is pruned to the window; a counter DECREASE
	 * re-baselines (reconnect); classification waits until the ring spans ~the full window so a live
	 * source is never mislabelled idle at startup. */
	{
		std::set<std::string> present;
		scan.n_idle = 0;
		for (GenlockInputRow &r : scan.inputs) {
			if (!r.connected)
				continue; /* an absent input is n_absent, never idle (no live connection) */
			present.insert(r.name);
			auto &ring = genlockRxHistory[r.name];
			if (!ring.empty() && r.frames_received < ring.back().second)
				ring.clear(); /* received counter went backward -> reconnect reset, re-baseline */
			ring.emplace_back(now_ms, r.frames_received);
			while (ring.size() > 1 && now_ms - ring.front().first > GENLOCK_IDLE_WINDOW_MS)
				ring.pop_front();
			const qint64 span = ring.back().first - ring.front().first;
			if (span >= GENLOCK_IDLE_WINDOW_MS * 9 / 10) {
				const uint64_t delta = ring.back().second - ring.front().second;
				r.idle = delta < GENLOCK_IDLE_INPUT_MIN_FRAMES;
			}
			if (r.idle) {
				scan.n_idle++;
				if (r.locked)
					scan.n_locked--; /* idle inputs are excluded from n_locked */
			}
		}
		/* bound the remembered state: drop ring entries for inputs no longer present this tick. */
		for (auto it = genlockRxHistory.begin(); it != genlockRxHistory.end();) {
			if (present.count(it->first) == 0)
				it = genlockRxHistory.erase(it);
			else
				++it;
		}
		/* an idle input must never be BLAMED as the unlocked offender: drop idle names from the
		 * unlocked list (the scan built it over all connected-!locked inputs, idle-unaware). */
		if (scan.n_idle > 0) {
			std::set<std::string> idle_names;
			for (const GenlockInputRow &r : scan.inputs)
				if (r.idle)
					idle_names.insert(r.name);
			scan.unlocked_names.erase(
				std::remove_if(scan.unlocked_names.begin(), scan.unlocked_names.end(),
					       [&](const std::string &nm) { return idle_names.count(nm) != 0; }),
				scan.unlocked_names.end());
		}
	}

	/* #1299 Part 3: recompute the recent-event driver as the CONNECTED-only, PHASE-only aggregate
	 * (relocks + late_holds + backward_steps). UNDERRUNS are DROPPED — a latency-budget miss owned
	 * by the genlock-fifo audit + cg-chain-verify (issue 1302), and bursty, so counting it latched
	 * the 60 s window chronically; an ABSENT input (its #1096 rebind churn) contributes 0. Also pick
	 * the top offender — the connected input carrying the most phase events — so a DEGRADED reason
	 * NAMES the culprit (reason=recent_event:<name>). genlock_input_phase_events is the pure rule
	 * shared with src/genlock_lock_state.rs (C-vs-Rust parity-gated). */
	scan.event_sum = 0;
	std::string recent_event_input_name;
	uint64_t recent_event_input_events = 0;
	for (const GenlockInputRow &r : scan.inputs) {
		const uint64_t pe = genlock_input_phase_events(r.connected ? 1 : 0, r.idle ? 1 : 0, r.relocks,
							       r.late_holds, r.backward_steps);
		scan.event_sum += pe;
		if (pe > recent_event_input_events) {
			recent_event_input_events = pe;
			recent_event_input_name = r.name;
		}
	}

	/* recent-event (relock/late-hold/backward-step on a CONNECTED input in the last 60 s): detect an
	 * INCREASE of the aggregate cumulative counter across ticks. A decrease (a reconnect reset
	 * the counters) re-baselines with no event. */
	if (genlockFirstSample) {
		genlockLastEventSum = scan.event_sum;
		genlockFirstSample = false;
	} else if (scan.event_sum > genlockLastEventSum) {
		genlockLastEventMs = now_ms;
		genlockLastEventSum = scan.event_sum;
	} else if (scan.event_sum < genlockLastEventSum) {
		genlockLastEventSum = scan.event_sum;
	}
	const bool recent_event = genlockLastEventMs >= 0 && (now_ms - genlockLastEventMs) < 60000;

	/* clock present iff a successful :8898 poll landed within the last 3 s. */
	const bool clock_present = genlockClockLastOkMs >= 0 && (now_ms - genlockClockLastOkMs) < 3000;

	/* #1299 Part 4 + #1357: the wall-vs-QPC drift verdict. Push this tick's SIGNED cumulative drift +
	 * monotonic timestamp; prune the ring to GENLOCK_QPC_WINDOW_S. Derive the drift delta + elapsed
	 * span across the window (report-only rate telemetry) and the largest single-sample STEP within
	 * it, and ask the parity-gated pure decision whether the wall STEPPED by more than one frame — the
	 * one clock hazard, the same on every box. A steady rate (any box) never pages. qpc_expected_ppm
	 * (f_ptp + f_phase) described the free-crystal Windows clock; since issue 1372 the measured rate is
	 * ~0 on Windows too, so it is report-only context, not an expectation. */
	const double qpc_expected_ppm = clock_present ? (genlockClockFptpPpm + genlockClockFphasePpm) : 0.0;
	if (scan.any_input) {
		/* camera-box issue 1372: BOOK a dantesync fleet date step. The jump of this sample against
		 * the previous one is re-based out of the whole history (every older sample shifted by it), so
		 * neither the step verdict nor the rate telemetry reads it; one genlock-wall-step: line records
		 * it. At most GENLOCK_QPC_WALL_STEPS_PER_WINDOW per window: a second step stays in the history
		 * and DEGRADES, like a jump beyond GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS. */
		while (!genlockQpcBookedSteps.empty() &&
		       now_ms - genlockQpcBookedSteps.front() > (qint64)GENLOCK_QPC_WINDOW_S * 1000)
			genlockQpcBookedSteps.pop_front();
		if (!genlockQpcHistory.empty()) {
			const int64_t jump = scan.qpc_signed_ms - genlockQpcHistory.back().second;
			const int64_t rebase = genlock_qpc_wall_step_rebase_ms(
				jump, GENLOCK_QPC_STEP_BOUND_MS, GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS,
				(int64_t)genlockQpcBookedSteps.size(), GENLOCK_QPC_WALL_STEPS_PER_WINDOW);
			if (rebase != 0) {
				for (auto &sample : genlockQpcHistory)
					sample.second += rebase;
				genlockQpcBookedSteps.push_back(now_ms);
				blog(LOG_INFO,
				     "genlock-wall-step: the wall clock stepped %lld ms against the media clock -- "
				     "qpc_drift re-baselined, not degraded (booked %d in %d s) (issue 1372)",
				     (long long)rebase, (int)genlockQpcBookedSteps.size(), GENLOCK_QPC_WINDOW_S);
			}
		}
		genlockQpcHistory.emplace_back(now_ms, scan.qpc_signed_ms);
	}
	while (genlockQpcHistory.size() > 1 &&
	       now_ms - genlockQpcHistory.front().first > (qint64)GENLOCK_QPC_WINDOW_S * 1000)
		genlockQpcHistory.pop_front();
	long long qpc_delta_ms = 0, qpc_elapsed_ms = 0, qpc_max_step_ms = 0;
	int qpc_rate_ready = 0;
	if (scan.any_input && genlockQpcHistory.size() >= 2) {
		const auto &oldest = genlockQpcHistory.front();
		const auto &newest = genlockQpcHistory.back();
		qpc_elapsed_ms = (long long)(newest.first - oldest.first);
		qpc_delta_ms = (long long)(newest.second - oldest.second);
		/* the rate needs the window ~filled (90%) so the integer-ms delta has usable resolution. */
		qpc_rate_ready = qpc_elapsed_ms >= (long long)GENLOCK_QPC_WINDOW_S * 1000 * 9 / 10 ? 1 : 0;
		int64_t prev = genlockQpcHistory.front().second;
		for (const auto &sample : genlockQpcHistory) {
			int64_t jump = sample.second - prev;
			if (jump < 0)
				jump = -jump;
			if (jump > qpc_max_step_ms)
				qpc_max_step_ms = jump;
			prev = sample.second;
		}
	}
	double qpc_measured_ppm = 0.0;
	const int qpc_beyond = genlock_qpc_drift_beyond_bound(
		qpc_rate_ready, qpc_delta_ms, qpc_elapsed_ms, qpc_max_step_ms, GENLOCK_QPC_STEP_BOUND_MS,
		&qpc_measured_ppm);
	const bool qpc_step = qpc_max_step_ms > GENLOCK_QPC_STEP_BOUND_MS;

	/* camera-box issue 1372 part D: the media-clock (audio clock) term (ReduceGenlockMediaClock). */
	const GenlockMediaClockTick mc = ReduceGenlockMediaClock(now_ms, clock_present);
	const genlock_media_clock_t media_clock = (genlock_media_clock_t)mc.verdict;
	const int64_t media_drift_us = mc.drift_us;
	const int media_window_ready = mc.ready;
	const int media_discipline = mc.discipline;

	genlock_lock_facets_t f;
	f.n_inputs = scan.n_inputs;
	f.n_locked = scan.n_locked;
	f.n_absent = scan.n_absent;
	f.n_idle = scan.n_idle;
	f.recent_event = recent_event ? 1 : 0;
	f.qpc_drift_beyond_bound = qpc_beyond;
	f.clock_present = clock_present ? 1 : 0;
	f.clock_locked = (clock_present && genlockClockLocked) ? 1 : 0;
	f.clock_ntp_failed = (clock_present && genlockClockNtpFailed) ? 1 : 0;
	f.output_present = out.present ? 1 : 0;
	f.output_stamping = out.stamping ? 1 : 0;
	f.audio_unpaired = scan.audio_unpaired ? 1 : 0;
	f.audio_unexpected = scan.audio_unexpected ? 1 : 0;
	f.media_clock = (int)media_clock;

	genlock_lock_reason_t reason;
	const genlock_lock_state_t state = genlock_decide_lock_state(&f, &reason);

	/* human reason text for the label (names the offending input for input_unlocked). */
	const QString reasonText = genlock_reason_text(reason, scan, clock_present, qpc_max_step_ms,
							 recent_event_input_name, media_clock, media_drift_us, media_discipline);

	/* latency display: a single value when all locked inputs share one, else a range. */
	QString latencyText;
	if (scan.any_input) {
		if (scan.min_latency_ms == scan.max_latency_ms)
			latencyText = QString("%1 ms").arg(scan.min_latency_ms);
		else
			latencyText = QString("%1-%2 ms").arg(scan.min_latency_ms).arg(scan.max_latency_ms);
	}

	QString text;
	QString color;
	if (state == GENLOCK_LOCK_LOCKED) {
		/* #1299/#1341: show locked/CONNECTED-live (not /n_inputs), and surface any senderless
		 * (n_absent) OR keep-alive-only (n_idle) inputs together as idle so "LOCKED 2/2 (+10 idle)"
		 * reads honestly instead of an alarming "LOCKED 2/12". */
		const int n_idle_shown = f.n_absent + f.n_idle;
		const int n_connected = f.n_inputs - n_idle_shown > 0 ? f.n_inputs - n_idle_shown : 0;
		text = QString("GENLOCK ● LOCKED %1/%2 @ %3").arg(f.n_locked).arg(n_connected).arg(latencyText);
		if (n_idle_shown > 0)
			text += QString(" (+%1 idle)").arg(n_idle_shown);
		color = "#2ecc71"; /* green */
	} else if (state == GENLOCK_LOCK_DEGRADED) {
		text = QString("GENLOCK ● DEGRADED %1").arg(reasonText);
		color = "#f1c40f"; /* amber */
	} else {
		text = QString("GENLOCK ● UNLOCKED %1").arg(reasonText);
		color = "#e74c3c"; /* red */
	}

	genlockLabel->setText(text);
	genlockLabel->setStyleSheet(QString("QLabel { color: %1; padding: 0 6px; }").arg(color));

	/* tooltip: per-input rows + the clock + output facets. */
	QString tip = QString("Genlock: %1\nClock: %2\nOutput: %3\n")
			      .arg(genlock_state_name(state))
			      .arg(!clock_present ? "absent" : (f.clock_locked ? "locked" : "not locked"))
			      .arg(!out.present ? "n/a" : (out.stamping ? "stamping wall clock" : "NOT stamping"));
	/* #1341: surface the idle/absent input counts (excluded from the DEGRADED gate) so an operator
	 * sees why "LOCKED 2/2" rather than "LOCKED 2/12". */
	if (f.n_idle > 0 || f.n_absent > 0)
		tip += QString("Idle: %1 (absent %2, low-rate %3)\n").arg(f.n_idle + f.n_absent).arg(f.n_absent).arg(f.n_idle);
	/* issue 1372 part D: the audio (media) clock facet. */
	tip += QString("Audio clock: %1 (%2, discipline %3)\n")
		       .arg(genlock_media_clock_name(media_clock))
		       .arg(media_window_ready ? QString("drift %1 ms / %2 min")
							 .arg(QString::number((double)media_drift_us / 1000.0, 'f', 1))
							 .arg(GENLOCK_MEDIA_CLOCK_WINDOW_S / 60)
					       : QString("window filling"))
		       .arg(genlock_media_discipline_name(media_discipline));
	for (const std::string &row : scan.rows)
		tip += "  " + QString::fromStdString(row) + "\n";
	genlockLabel->setToolTip(tip.trimmed());

	/* #1299 Part 3: the human genlock-lock: line NAMES the offending input for a recent_event
	 * (reason=recent_event:cg). The JSON reason stays the bare enum token (the decision matches on
	 * it); the attribution rides recent_event_inputs there. */
	std::string reason_key_str = genlock_reason_key(reason);
	if (reason == GENLOCK_LOCK_REASON_RECENT_EVENT && !recent_event_input_name.empty()) {
		reason_key_str += ":";
		reason_key_str += recent_event_input_name;
	}
	/* issue 1372 part D: name the media-clock sub-kind (reason=media_clock:drift / :undisciplined). */
	if (reason == GENLOCK_LOCK_REASON_MEDIA_CLOCK) {
		reason_key_str += ":";
		reason_key_str += genlock_media_clock_name(media_clock);
	}

	/* issue 1372 part D: the media sub-kind joins the change key while the reason is media_clock. */
	const int media_key = reason == GENLOCK_LOCK_REASON_MEDIA_CLOCK ? (int)media_clock : -1;

	/* greppable log on state/reason change (the #1298 genlock-lock: family). */
	if ((int)state != genlockLastLoggedState || (int)reason != genlockLastLoggedReason ||
	    media_key != genlockLastLoggedMedia) {
		genlockLastLoggedState = (int)state;
		genlockLastLoggedReason = (int)reason;
		genlockLastLoggedMedia = media_key;
		blog(LOG_INFO,
		     "genlock-lock: state=%s inputs=%d/%d latency_ms=%u clock=%s output=%s reason=%s (#1298)",
		     genlock_state_name(state), f.n_locked, f.n_inputs, scan.any_input ? scan.min_latency_ms : 0u,
		     !clock_present ? "absent" : (f.clock_locked ? "locked" : "unlocked"),
		     !out.present ? "absent" : (out.stamping ? "stamping" : "not-stamping"), reason_key_str.c_str());
	}

	/* camera-box #1299: the machine-readable genlock-lock-json: line the dev1 fleet watchdog +
	 * bundle-state read. Emitted on a state/reason CHANGE and on a heartbeat every
	 * GENLOCK_JSON_HEARTBEAT_TICKS ticks, so bundle-state's #1222 bounded TAIL always holds a fresh
	 * one (the change-only line above can fall into the omitted middle on a long-stable box). It
	 * carries the SAME verdict + clock/output tokens the widget decided this tick, so the fleet
	 * facet can never disagree with the statusbar. A state change short-circuits the ++heartbeat
	 * (not incremented this tick) but the emit resets the counter to 0 either way. */
	const bool genlock_json_changed = (int)state != genlockJsonLastState ||
					  (int)reason != genlockJsonLastReason || media_key != genlockJsonLastMedia;
	if (genlock_json_changed || ++genlockJsonHeartbeatTicks >= GENLOCK_JSON_HEARTBEAT_TICKS) {
		genlockJsonHeartbeatTicks = 0;
		genlockJsonLastState = (int)state;
		genlockJsonLastReason = (int)reason;
		genlockJsonLastMedia = media_key;
		/* #1299 Part 3: the offender rides the JSON only when there IS a recent event AND a named
		 * connected phase offender — otherwise recent_event_inputs is an empty list (the parser
		 * omits the key), so a v3 line never fabricates an attribution. */
		const bool has_offender = recent_event && !recent_event_input_name.empty();
		const std::string gl_json = genlock_build_lock_json(
			genlock_state_name(state), genlock_reason_key(reason), f.n_inputs, f.n_locked,
			f.n_absent, f.n_idle, scan.any_input ? scan.min_latency_ms : 0u,
			!clock_present ? "absent" : (f.clock_locked ? "locked" : "unlocked"),
			!out.present ? "absent" : (out.stamping ? "stamping" : "not-stamping"), recent_event,
			has_offender ? recent_event_input_name.c_str() : nullptr, recent_event_input_events,
			scan.max_abs_qpc_drift_ms, qpc_measured_ppm, qpc_expected_ppm, qpc_step ? 1 : 0,
			scan.audio_unexpected_names.empty() ? nullptr : scan.audio_unexpected_names.front().c_str(),
			genlock_media_clock_name(media_clock), media_drift_us, GENLOCK_MEDIA_CLOCK_WINDOW_S,
			media_window_ready, genlock_media_discipline_name(media_discipline), scan.inputs);
		blog(LOG_INFO, "genlock-lock-json: %s (#1299)", gl_json.c_str());
	}
}
