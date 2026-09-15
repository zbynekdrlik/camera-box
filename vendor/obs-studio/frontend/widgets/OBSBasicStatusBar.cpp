#include "OBSBasicStatusBar.hpp"
#include "ui_StatusBarWidget.h"
#include "GenlockLockState.hpp"

#include <widgets/OBSBasic.hpp>

#include <QLabel>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QTimer>
#include <QUrl>

#include <cstdint>
#include <cstdio>
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
 * and renders the verdict. GENLOCK_QPC_DRIFT_BOUND_MS is the wall-vs-monotonic drift that trips
 * DEGRADED; 100 ms is generous (the #800 drift is the concern over a day, not a tick). */
static constexpr int64_t GENLOCK_QPC_DRIFT_BOUND_MS = 100;

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

namespace {
/* camera-box #1299: the structured per-input record the genlock-lock-json: line carries (the
 * tooltip `rows` above are pre-formatted human strings; this is the machine-readable sibling). */
struct GenlockInputRow {
	std::string name;
	bool locked = false;
	uint32_t latency_ms = 0;
	uint64_t underruns = 0;
	uint64_t relocks = 0;
	uint64_t late_holds = 0;
	uint32_t depth = 0;
};
struct GenlockScan {
	int n_inputs = 0;
	int n_locked = 0;
	quint64 event_sum = 0;
	int64_t max_abs_qpc_drift_ms = 0;
	uint32_t min_latency_ms = 0;
	uint32_t max_latency_ms = 0;
	bool any_input = false;
	bool audio_unpaired = false;                    /* #1303: an audio-enabled genlock source is unpaired with its video FIFO hold */
	std::vector<std::string> unlocked_names;
	std::vector<std::string> audio_unpaired_names;  /* #1303: which sources tripped the audio pairing bound */
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
	scan->event_sum += st.underruns + st.relocks + st.late_holds + st.backward_steps;
	const int64_t d = st.wall_qpc_drift_ms < 0 ? -st.wall_qpc_drift_ms : st.wall_qpc_drift_ms;
	if (d > scan->max_abs_qpc_drift_ms)
		scan->max_abs_qpc_drift_ms = d;
	if (!scan->any_input || st.latency_ms < scan->min_latency_ms)
		scan->min_latency_ms = st.latency_ms;
	if (!scan->any_input || st.latency_ms > scan->max_latency_ms)
		scan->max_latency_ms = st.latency_ms;
	scan->any_input = true;
	const char *name = obs_source_get_name(source);
	const std::string nm = name ? name : "?";
	if (!st.locked)
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
	rec.latency_ms = st.latency_ms;
	rec.underruns = st.underruns;
	rec.relocks = st.relocks;
	rec.late_holds = st.late_holds;
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
	default:
		return "qpc_drift";
	}
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
				    int n_locked, uint32_t latency_ms, const char *clock_str,
				    const char *output_str, bool recent_event, int64_t qpc_drift_ms,
				    const std::vector<GenlockInputRow> &inputs)
{
	std::string j = "{\"v\":1,\"state\":";
	genlock_json_append_escaped(j, state_name);
	j += ",\"reason\":";
	genlock_json_append_escaped(j, reason_key);
	char num[96];
	snprintf(num, sizeof(num), ",\"n_inputs\":%d,\"n_locked\":%d,\"latency_ms\":%u,", n_inputs,
		 n_locked, latency_ms);
	j += num;
	j += "\"clock\":";
	genlock_json_append_escaped(j, clock_str);
	j += ",\"output\":";
	genlock_json_append_escaped(j, output_str);
	j += ",\"recent_event\":";
	j += recent_event ? "true" : "false";
	snprintf(num, sizeof(num), ",\"qpc_drift_ms\":%lld,\"inputs\":[", (long long)qpc_drift_ms);
	j += num;
	bool first = true;
	for (const GenlockInputRow &r : inputs) {
		if (!first)
			j += ",";
		first = false;
		j += "{\"name\":";
		genlock_json_append_escaped(j, r.name.c_str());
		snprintf(num, sizeof(num), ",\"locked\":%s,\"latency_ms\":%u,",
			 r.locked ? "true" : "false", r.latency_ms);
		j += num;
		snprintf(num, sizeof(num),
			 "\"underruns\":%llu,\"relocks\":%llu,\"late_holds\":%llu,\"depth\":%u}",
			 (unsigned long long)r.underruns, (unsigned long long)r.relocks,
			 (unsigned long long)r.late_holds, r.depth);
		j += num;
	}
	j += "]}";
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
		genlockClockLastOkMs = genlockClock.elapsed();
		obs_data_release(d);
	});
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

	/* recent-event (relock/underrun/late-hold/backward-step in the last 60 s): detect an
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

	genlock_lock_facets_t f;
	f.n_inputs = scan.n_inputs;
	f.n_locked = scan.n_locked;
	f.recent_event = recent_event ? 1 : 0;
	f.qpc_drift_beyond_bound = scan.max_abs_qpc_drift_ms > GENLOCK_QPC_DRIFT_BOUND_MS ? 1 : 0;
	f.clock_present = clock_present ? 1 : 0;
	f.clock_locked = (clock_present && genlockClockLocked) ? 1 : 0;
	f.clock_ntp_failed = (clock_present && genlockClockNtpFailed) ? 1 : 0;
	f.output_present = out.present ? 1 : 0;
	f.output_stamping = out.stamping ? 1 : 0;
	f.audio_unpaired = scan.audio_unpaired ? 1 : 0;

	genlock_lock_reason_t reason;
	const genlock_lock_state_t state = genlock_decide_lock_state(&f, &reason);

	/* human reason text for the label (names the offending input for input_unlocked). */
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
		reasonText = "recent relock/underrun";
		break;
	case GENLOCK_LOCK_REASON_NTP_FAILED:
		reasonText = "clock NTP failed";
		break;
	case GENLOCK_LOCK_REASON_QPC_DRIFT:
		reasonText = QString("clock drift %1 ms").arg(scan.max_abs_qpc_drift_ms);
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
	}

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
		text = QString("GENLOCK ● LOCKED %1/%2 @ %3").arg(f.n_locked).arg(f.n_inputs).arg(latencyText);
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
	for (const std::string &row : scan.rows)
		tip += "  " + QString::fromStdString(row) + "\n";
	genlockLabel->setToolTip(tip.trimmed());

	/* greppable log on state/reason change (the #1298 genlock-lock: family). */
	if ((int)state != genlockLastLoggedState || (int)reason != genlockLastLoggedReason) {
		genlockLastLoggedState = (int)state;
		genlockLastLoggedReason = (int)reason;
		blog(LOG_INFO,
		     "genlock-lock: state=%s inputs=%d/%d latency_ms=%u clock=%s output=%s reason=%s (#1298)",
		     genlock_state_name(state), f.n_locked, f.n_inputs, scan.any_input ? scan.min_latency_ms : 0u,
		     !clock_present ? "absent" : (f.clock_locked ? "locked" : "unlocked"),
		     !out.present ? "absent" : (out.stamping ? "stamping" : "not-stamping"), genlock_reason_key(reason));
	}

	/* camera-box #1299: the machine-readable genlock-lock-json: line the dev1 fleet watchdog +
	 * bundle-state read. Emitted on a state/reason CHANGE and on a heartbeat every
	 * GENLOCK_JSON_HEARTBEAT_TICKS ticks, so bundle-state's #1222 bounded TAIL always holds a fresh
	 * one (the change-only line above can fall into the omitted middle on a long-stable box). It
	 * carries the SAME verdict + clock/output tokens the widget decided this tick, so the fleet
	 * facet can never disagree with the statusbar. A state change short-circuits the ++heartbeat
	 * (not incremented this tick) but the emit resets the counter to 0 either way. */
	const bool genlock_json_changed =
		(int)state != genlockJsonLastState || (int)reason != genlockJsonLastReason;
	if (genlock_json_changed || ++genlockJsonHeartbeatTicks >= GENLOCK_JSON_HEARTBEAT_TICKS) {
		genlockJsonHeartbeatTicks = 0;
		genlockJsonLastState = (int)state;
		genlockJsonLastReason = (int)reason;
		const std::string gl_json = genlock_build_lock_json(
			genlock_state_name(state), genlock_reason_key(reason), f.n_inputs, f.n_locked,
			scan.any_input ? scan.min_latency_ms : 0u,
			!clock_present ? "absent" : (f.clock_locked ? "locked" : "unlocked"),
			!out.present ? "absent" : (out.stamping ? "stamping" : "not-stamping"), recent_event,
			scan.max_abs_qpc_drift_ms, scan.inputs);
		blog(LOG_INFO, "genlock-lock-json: %s (#1299)", gl_json.c_str());
	}
}
