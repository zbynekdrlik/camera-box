#pragma once

#include "StatusBarWidget.hpp"

#include <obs.hpp>

#include <QElapsedTimer>
#include <QPointer>
#include <QStatusBar>

#include <cstdint>
#include <deque>
#include <map>
#include <string>
#include <utility>

class QTimer;
class QLabel;
class QNetworkAccessManager;
class QNetworkReply;

class OBSBasicStatusBar : public QStatusBar {
	Q_OBJECT

private:
	StatusBarWidget *statusWidget = nullptr;

	OBSWeakOutputAutoRelease streamOutput;
	std::vector<OBSSignal> streamSigs;
	OBSWeakOutputAutoRelease recordOutput;
	bool active = false;
	bool overloadedNotify = true;
	bool streamPauseIconToggle = false;
	bool disconnected = false;
	bool firstCongestionUpdate = false;

	std::vector<float> congestionArray;

	int retries = 0;
	int totalStreamSeconds = 0;
	int totalRecordSeconds = 0;

	int reconnectTimeout = 0;

	int delaySecTotal = 0;
	int delaySecStarting = 0;
	int delaySecStopping = 0;

	int startSkippedFrameCount = 0;
	int startTotalFrameCount = 0;
	int lastSkippedFrameCount = 0;

	int seconds = 0;
	uint64_t lastBytesSent = 0;
	uint64_t lastBytesSentTime = 0;

	QPixmap excellentPixmap;
	QPixmap goodPixmap;
	QPixmap mediocrePixmap;
	QPixmap badPixmap;
	QPixmap disconnectedPixmap;
	QPixmap inactivePixmap;

	QPixmap recordingActivePixmap;
	QPixmap recordingPausePixmap;
	QPixmap recordingPauseInactivePixmap;
	QPixmap recordingInactivePixmap;
	QPixmap streamingActivePixmap;
	QPixmap streamingInactivePixmap;

	float lastCongestion = 0.0f;

	QPointer<QTimer> refreshTimer;
	QPointer<QTimer> messageTimer;

	/* camera-box #1298: the in-OBS GENLOCK lock indicator. A permanent statusbar label
	 * driven by an always-on 1 Hz timer (NOT the stream-only refreshTimer) plus an async
	 * dantesync clock poll. genlockClock is a monotonic reference started in the ctor; the
	 * recent-event window and the clock-staleness check are both measured against it. */
	QLabel *genlockLabel = nullptr;
	QPointer<QTimer> genlockTimer;
	QNetworkAccessManager *genlockClockNam = nullptr;
	QElapsedTimer genlockClock;
	/* cached dantesync clock facet (written by the async reply, read by UpdateGenlock) */
	bool genlockClockLocked = false;
	bool genlockClockNtpFailed = false;
	/* #1299 Part 4: the slew the disciplined clock reports it applies (f_ptp servo + f_phase integral,
	 * ppm) — report-only qpc_expected_ppm telemetry (#1357: it no longer feeds the qpc_drift verdict,
	 * the wall STEP does). Cached by the async :8898 reply, read by UpdateGenlockLabel. */
	double genlockClockFptpPpm = 0.0;
	double genlockClockFphasePpm = 0.0;
	qint64 genlockClockLastOkMs = -1; /* monotonic ms of the last successful :8898 poll; -1 = never */
	/* recent-event (relock/underrun/late-hold/backward-step in the last 60 s) tracking */
	quint64 genlockLastEventSum = 0;
	qint64 genlockLastEventMs = -1; /* monotonic ms of the last observed counter increase */
	bool genlockFirstSample = true;
	/* #1299 Part 4 + #1357: a ring of (monotonic ms, SIGNED cumulative drift ms) samples over
	 * GENLOCK_QPC_WINDOW_S. The qpc_drift verdict keys on the largest single-sample wall STEP in it; the
	 * windowed RATE it also yields is report-only telemetry (never the unbounded cumulative offset). */
	std::deque<std::pair<qint64, int64_t>> genlockQpcHistory;
	/* camera-box issue 1372 part D: (monotonic ms, wall-minus-media offset us) samples the widget takes
	 * itself each tick, over GENLOCK_MEDIA_CLOCK_WINDOW_S. The media-clock term keys on their trimmed-mean
	 * per-pair RATE (wall steps fall outside its band): since part A the media clock follows the
	 * disciplined wall on every box. */
	std::deque<std::pair<qint64, int64_t>> genlockMediaClockHistory;
	/* #1341: per-input received-frame history (input name -> ring of (monotonic ms, cumulative
	 * frames_received)) over GENLOCK_IDLE_WINDOW_MS. An input whose received DELTA over the window is
	 * below GENLOCK_IDLE_INPUT_MIN_FRAMES is IDLE (keep-alive-only) and excluded from the DEGRADED
	 * gate; entries for inputs no longer present are pruned each tick so the state stays bounded. */
	std::map<std::string, std::deque<std::pair<qint64, uint64_t>>> genlockRxHistory;
	/* genlock-lock: log-on-change de-dup */
	int genlockLastLoggedState = -1;
	int genlockLastLoggedReason = -1;
	/* camera-box #1299: the machine-readable genlock-lock-json: line is emitted on every
	 * state/reason change AND every GENLOCK_JSON_HEARTBEAT_TICKS ticks, so bundle-state's
	 * #1222 bounded TAIL always holds a fresh one despite the change-only de-dup above. Its
	 * own change-tracking is SEPARATE from genlockLastLogged* (which the key=value block
	 * updates before the json block runs). */
	int genlockJsonHeartbeatTicks = 0;
	int genlockJsonLastState = -1;
	int genlockJsonLastReason = -1;
	/* camera-box issue 1372 part D: the media-clock sub-kind (drift / undisciplined) is part of the
	 * change key while the reason is media_clock, so a switch between the two logs at once. */
	int genlockLastLoggedMedia = -1;
	int genlockJsonLastMedia = -1;

	/* camera-box issue 1372 part D: one tick of the media-clock (audio clock) term. */
	struct GenlockMediaClockTick {
		int verdict = 0;      /* genlock_media_clock_t */
		int64_t drift_us = 0; /* the trimmed-mean rate scaled to the window (us per window) */
		int ready = 0;        /* the window spans >= 90 % */
		int discipline = 0;   /* genlock_media_discipline_t */
	};
	GenlockMediaClockTick ReduceGenlockMediaClock(qint64 now_ms, bool clock_present);

	void UpdateGenlockLabel();
	void PollGenlockClock();

	obs_output_t *GetOutput();

	void Activate();
	void Deactivate();

	void UpdateDelayMsg();
	void UpdateBandwidth();
	void UpdateStreamTime();
	void UpdateRecordTime();
	void UpdateRecordTimeLabel();
	void UpdateDroppedFrames();

	static void OBSOutputReconnect(void *data, calldata_t *params);
	static void OBSOutputReconnectSuccess(void *data, calldata_t *params);

public slots:
	void UpdateCPUUsage();

	void clearMessage();
	void showMessage(const QString &message, int timeout = 0);

private slots:
	void Reconnect(int seconds);
	void ReconnectSuccess();
	void UpdateStatusBar();
	void UpdateCurrentFPS();
	void UpdateIcons();

public:
	OBSBasicStatusBar(QWidget *parent);

	void StreamDelayStarting(int sec);
	void StreamDelayStopping(int sec);
	void StreamStarted(obs_output_t *output);
	void StreamStopped();
	void RecordingStarted(obs_output_t *output);
	void RecordingStopped();
	void RecordingPaused();
	void RecordingUnpaused();

	void ReconnectClear();
};
