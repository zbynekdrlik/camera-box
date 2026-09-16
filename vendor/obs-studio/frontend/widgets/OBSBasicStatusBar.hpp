#pragma once

#include "StatusBarWidget.hpp"

#include <obs.hpp>

#include <QElapsedTimer>
#include <QPointer>
#include <QStatusBar>

#include <cstdint>
#include <deque>
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
	 * ppm) — the EXPECTED wall-vs-QPC drift rate the qpc_drift verdict compares the measured windowed
	 * rate against. Cached by the async :8898 reply, read by UpdateGenlockLabel. */
	double genlockClockFptpPpm = 0.0;
	double genlockClockFphasePpm = 0.0;
	qint64 genlockClockLastOkMs = -1; /* monotonic ms of the last successful :8898 poll; -1 = never */
	/* recent-event (relock/underrun/late-hold/backward-step in the last 60 s) tracking */
	quint64 genlockLastEventSum = 0;
	qint64 genlockLastEventMs = -1; /* monotonic ms of the last observed counter increase */
	bool genlockFirstSample = true;
	/* #1299 Part 4: windowed wall-vs-QPC drift RATE. A ring of (monotonic ms, SIGNED cumulative drift
	 * ms) samples over GENLOCK_QPC_WINDOW_S; the qpc_drift verdict keys on the RATE vs the dantesync
	 * f_ptp+f_phase slew + a single-sample STEP, not the unbounded cumulative offset. */
	std::deque<std::pair<qint64, int64_t>> genlockQpcHistory;
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
