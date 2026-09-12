#pragma once

#include "StatusBarWidget.hpp"

#include <obs.hpp>

#include <QElapsedTimer>
#include <QPointer>
#include <QStatusBar>

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
	qint64 genlockClockLastOkMs = -1; /* monotonic ms of the last successful :8898 poll; -1 = never */
	/* recent-event (relock/underrun/late-hold/backward-step in the last 60 s) tracking */
	quint64 genlockLastEventSum = 0;
	qint64 genlockLastEventMs = -1; /* monotonic ms of the last observed counter increase */
	bool genlockFirstSample = true;
	/* genlock-lock: log-on-change de-dup */
	int genlockLastLoggedState = -1;
	int genlockLastLoggedReason = -1;

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
