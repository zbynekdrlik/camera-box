#pragma once
#include <QFrame>
#include <QPushButton>
#include <QLabel>
#include <obs.hpp>
#include <obs-frontend-api.h>
#include "sync-test-output.hpp"

class SyncTestDock : public QFrame {
	Q_OBJECT

public:
	SyncTestDock(QWidget *parent = nullptr);
	~SyncTestDock();

private:
	QPushButton *startButton = nullptr;

	QLabel *latencyDisplay = nullptr;
	QLabel *latencyPolarity = nullptr;
	QLabel *indexDisplay = nullptr;
	QLabel *frequencyDisplay = nullptr;
	QLabel *videoIndexDisplay = nullptr;
	QLabel *audioIndexDisplay = nullptr;

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

	static void cb_video_marker_found(void *param, calldata_t *cd);
	static void cb_audio_marker_found(void *param, calldata_t *cd);
	static void cb_sync_found(void *param, calldata_t *cd);

	// #690: auto-start the measurement output once OBS has finished loading (vs. sitting stopped on
	// dashes after every OBS relaunch, waiting for a manual click — the "forgettable toggle" this
	// ticket fixes). Registered in the constructor, removed in the destructor.
	static void cb_frontend_event(enum obs_frontend_event event, void *param);
};
