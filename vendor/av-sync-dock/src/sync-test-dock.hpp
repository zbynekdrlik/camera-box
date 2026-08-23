#pragma once
#include <QFrame>
#include <QPushButton>
#include <QLabel>
#include <QTimer>
#include <obs.hpp>
#include <obs-frontend-api.h>
#include "sync-test-output.hpp"

class QShowEvent;
class QHideEvent;

class SyncTestDock : public QFrame {
	Q_OBJECT

public:
	SyncTestDock(QWidget *parent = nullptr);
	~SyncTestDock();

protected:
	// #926 fix-up (review finding 13): start/stop the ASRC poll timer with dock visibility -- no
	// point polling CAMERA_BOX_ASRC_SOURCE_NAME once a second while the dock isn't even shown.
	void showEvent(QShowEvent *event) override;
	void hideEvent(QHideEvent *event) override;

private:
	QPushButton *startButton = nullptr;

	QLabel *latencyDisplay = nullptr;
	QLabel *latencyPolarity = nullptr;
	QLabel *indexDisplay = nullptr;
	QLabel *frequencyDisplay = nullptr;
	QLabel *videoIndexDisplay = nullptr;
	QLabel *audioIndexDisplay = nullptr;

	// #926: plain-language operator status ("measuring" / "locked, holding sync" / "no test
	// signal, holding last correction") -- see requirement 1 (understandable status).
	QLabel *statusLabel = nullptr;

	// #926: ASRC transparency section (requirement 3) -- current state/numbers read from the
	// program-audio source (CAMERA_BOX_ASRC_SOURCE_NAME), refreshed on a timer since ASRC has no
	// change-notification signal; the enable toggle and ppm-trim buttons write straight through to
	// the already-existing core obs_source_set_asrc_* setters (issues #803/#806/#912 -- no new
	// mechanism, only UI exposure of what was already live).
	QLabel *asrcStateLabel = nullptr;
	QLabel *asrcEstimatedLabel = nullptr;
	QLabel *asrcAppliedLabel = nullptr;
	QLabel *asrcTrimLabel = nullptr;
	QPushButton *asrcToggleButton = nullptr;
	QPushButton *asrcTrimDownButton = nullptr;
	QPushButton *asrcTrimUpButton = nullptr;
	QTimer *asrcRefreshTimer = nullptr;

	// #926 fix-up (review finding 16): latches the "source not on this box" log to ONCE (STRIH
	// runs the same DLL but has no CAMERA_BOX_ASRC_SOURCE_NAME) instead of one line per 1s poll.
	bool asrcSourceMissingLogged = false;

	// #1177: the plain-language status shown BEFORE the input went STALE. lock_state_changed is
	// edge-triggered (#926) and the held cluster lock is never dropped during EVENT mode (that is the
	// exact defect this ticket fixes), so on a stale->recover cycle no lock_state_changed re-fires to
	// correct the text. Restoring this remembered key on recovery keeps the header honest (Locked /
	// NoSignal / Measuring as it truly was) instead of a generic "Measuring" over a live locked value.
	const char *lastStatusKey = "Status.Measuring";

private:
	OBSOutput sync_test;

private:
	int last_video_ix;
	int last_audio_ix;
	int missed_video_ix;
	int missed_audio_ix;
	int received_video_ix;
	int received_audio_ix;
	int received_video_index_max = 0;
	int received_audio_index_max = 0;
	int audio_index_max = 0;

private:
	void on_start_stop();
	void start(); // #690: the "request to start" half of on_start_stop(), also called auto-on-load.

	void on_video_marker_found(video_marker_found_s data);
	void on_audio_marker_found(audio_marker_found_s data);
	void on_sync_found(sync_index data);
	void on_lock_state_changed(bool locked); // #926
	void on_sync_stale_changed(bool stale);  // #1177

	static void cb_video_marker_found(void *param, calldata_t *cd);
	static void cb_audio_marker_found(void *param, calldata_t *cd);
	static void cb_sync_found(void *param, calldata_t *cd);
	static void cb_lock_state_changed(void *param, calldata_t *cd); // #926
	static void cb_sync_stale_changed(void *param, calldata_t *cd); // #1177

	// #690: auto-start the measurement output once OBS has finished loading (vs. sitting stopped on
	// dashes after every OBS relaunch, waiting for a manual click — the "forgettable toggle" this
	// ticket fixes). Registered in the constructor, removed in the destructor.
	static void cb_frontend_event(enum obs_frontend_event event, void *param);

	// #926: ASRC section handlers -- see the member doc comments above.
	void refresh_asrc_ui();
	void on_asrc_toggle_clicked();
	void on_asrc_trim_down_clicked();
	void on_asrc_trim_up_clicked();
};
