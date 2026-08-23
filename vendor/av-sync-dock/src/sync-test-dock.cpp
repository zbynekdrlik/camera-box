/*
OBS Audio Video Sync Dock
Copyright (C) 2023 Norihiro Kamae <norihiro@nagater.net>

This program is free software; you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation; either version 2 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License along
with this program; if not, write to the Free Software Foundation, Inc.,
51 Franklin Street, Fifth Floor, Boston, MA  02110-1301, USA.
*/

#include <obs-module.h>
#include <QHBoxLayout>
#include <QVBoxLayout>
#include <QTimer>
#include <QMainWindow>
#include <QShowEvent>
#include <QHideEvent>
#include <obs-frontend-api.h>
#include "plugin-macros.generated.h"
#include "sync-test-dock.hpp"
#include "camera-box-audio.hpp" // #999 -- cb_dock_latency_display_ms (on_sync_found gate-convert)

#define ASSERT_THREAD(type)                                                                     \
	do {                                                                                    \
		if (!obs_in_task_thread(type))                                                  \
			blog(LOG_ERROR, "%s: ASSERT_THREAD failed: Expected " #type, __func__); \
	} while (false)

/* #926: the program-audio source ASRC (issues #803/#806/#912) lives on -- the SAME 'mbc' name
 * scripts/av_sync_measure.py's DEFAULT_OUTER_LOOP_SOURCE already uses. Hardcoded, no env var, per
 * this repo's hard-lock philosophy (issue 257: no forgettable/mysterious knobs). */
#define CAMERA_BOX_ASRC_SOURCE_NAME "mbc"

/* #926: manual ppm-trim step per button click -- half the +/-10ppm outer-loop bias range's
 * granularity gives 20 clicks end-to-end, fine enough for a deliberate small nudge without being
 * fiddly. Clamped at the servo itself (asrc-compensator.c) regardless. */
#define CAMERA_BOX_ASRC_TRIM_STEP_PPM 0.5

/* #926: how often the ASRC section polls CAMERA_BOX_ASRC_SOURCE_NAME -- ASRC has no
 * change-notification signal (unlike the lock state above, which is signal-driven), so this is a
 * plain timer. Frequent enough to feel live, far below any meaningful cost (a handful of double
 * reads off one source). */
#define CAMERA_BOX_ASRC_REFRESH_MS 1000

SyncTestDock::SyncTestDock(QWidget *parent) : QFrame(parent)
{
	QVBoxLayout *mainLayout = new QVBoxLayout();
	QGridLayout *topLayout = new QGridLayout();

	int y = 0;

	startButton = new QPushButton(obs_module_text("Button.Start"), this);
	mainLayout->addWidget(startButton);
	connect(startButton, &QPushButton::clicked, this, &SyncTestDock::on_start_stop);

	QLabel *label;
	// #926: plain-language operator status (requirement 1) sits FIRST, above the raw numbers --
	// "what is measured and whether sync is working", not a raw counter, is the first thing an
	// operator glancing at the dock should read. Starts on the "measuring" text (no lock yet, no
	// stale prior state to mislead).
	statusLabel = new QLabel(obs_module_text("Status.Measuring"), this);
	statusLabel->setObjectName("statusLabel");
	statusLabel->setProperty("class", "text-large");
	mainLayout->addWidget(statusLabel);

	label = new QLabel(obs_module_text("Label.Latency"), this);
	label->setProperty("class", "text-large");
	topLayout->addWidget(label, y, 0);

	latencyDisplay = new QLabel("-", this);
	latencyDisplay->setObjectName("latencyDisplay");
	latencyDisplay->setProperty("class", "text-large");
	topLayout->addWidget(latencyDisplay, y++, 1);

	latencyPolarity = new QLabel("-", this);
	latencyPolarity->setObjectName("latencyPolarity");
	topLayout->addWidget(latencyPolarity, y++, 1);

	label = new QLabel(obs_module_text("Label.Index"), this);
	topLayout->addWidget(label, y, 0);

	indexDisplay = new QLabel("-", this);
	indexDisplay->setObjectName("indexDisplay");
	topLayout->addWidget(indexDisplay, y++, 1);

	label = new QLabel(obs_module_text("Label.Frequency"), this);
	topLayout->addWidget(label, y, 0);

	frequencyDisplay = new QLabel("-", this);
	frequencyDisplay->setObjectName("frequencyDisplay");
	topLayout->addWidget(frequencyDisplay, y++, 1);

	label = new QLabel(obs_module_text("Label.VideoIndex"), this);
	topLayout->addWidget(label, y, 0);

	videoIndexDisplay = new QLabel("-", this);
	videoIndexDisplay->setObjectName("videoIndexDisplay");
	topLayout->addWidget(videoIndexDisplay, y++, 1);

	label = new QLabel(obs_module_text("Label.AudioIndex"), this);
	topLayout->addWidget(label, y, 0);

	audioIndexDisplay = new QLabel("-", this);
	audioIndexDisplay->setObjectName("audioIndexDisplay");
	topLayout->addWidget(audioIndexDisplay, y++, 1);

	mainLayout->addLayout(topLayout);

	// #926: ASRC transparency + controls (requirement 3) -- current state/numbers read from
	// CAMERA_BOX_ASRC_SOURCE_NAME, refreshed on a timer (see refresh_asrc_ui()); the toggle and
	// trim buttons write straight through to the already-existing core obs_source_set_asrc_*
	// setters (issues #803/#806/#912) -- no new mechanism, only UI exposure of what was already
	// live but only reachable via the OBS WebSocket or a Python watchdog until now.
	QGridLayout *asrcLayout = new QGridLayout();
	int ay = 0;

	QLabel *asrcHeader = new QLabel(obs_module_text("Label.AsrcSection"), this);
	asrcHeader->setProperty("class", "text-large");
	asrcLayout->addWidget(asrcHeader, ay++, 0, 1, 2);

	label = new QLabel(obs_module_text("Label.AsrcState"), this);
	asrcLayout->addWidget(label, ay, 0);
	asrcStateLabel = new QLabel("-", this);
	asrcStateLabel->setObjectName("asrcStateLabel");
	asrcLayout->addWidget(asrcStateLabel, ay++, 1);

	label = new QLabel(obs_module_text("Label.AsrcEstimated"), this);
	asrcLayout->addWidget(label, ay, 0);
	asrcEstimatedLabel = new QLabel("-", this);
	asrcEstimatedLabel->setObjectName("asrcEstimatedLabel");
	asrcLayout->addWidget(asrcEstimatedLabel, ay++, 1);

	label = new QLabel(obs_module_text("Label.AsrcApplied"), this);
	asrcLayout->addWidget(label, ay, 0);
	asrcAppliedLabel = new QLabel("-", this);
	asrcAppliedLabel->setObjectName("asrcAppliedLabel");
	asrcLayout->addWidget(asrcAppliedLabel, ay++, 1);

	label = new QLabel(obs_module_text("Label.AsrcTrim"), this);
	asrcLayout->addWidget(label, ay, 0);
	asrcTrimLabel = new QLabel("-", this);
	asrcTrimLabel->setObjectName("asrcTrimLabel");
	asrcLayout->addWidget(asrcTrimLabel, ay++, 1);

	mainLayout->addLayout(asrcLayout);

	QHBoxLayout *asrcButtonsLayout = new QHBoxLayout();
	asrcToggleButton = new QPushButton(obs_module_text("Button.AsrcToggleOff"), this);
	asrcToggleButton->setObjectName("asrcToggleButton");
	asrcButtonsLayout->addWidget(asrcToggleButton);
	connect(asrcToggleButton, &QPushButton::clicked, this, &SyncTestDock::on_asrc_toggle_clicked);

	asrcTrimDownButton = new QPushButton(obs_module_text("Button.AsrcTrimDown"), this);
	asrcTrimDownButton->setObjectName("asrcTrimDownButton");
	asrcButtonsLayout->addWidget(asrcTrimDownButton);
	connect(asrcTrimDownButton, &QPushButton::clicked, this, &SyncTestDock::on_asrc_trim_down_clicked);

	asrcTrimUpButton = new QPushButton(obs_module_text("Button.AsrcTrimUp"), this);
	asrcTrimUpButton->setObjectName("asrcTrimUpButton");
	asrcButtonsLayout->addWidget(asrcTrimUpButton);
	connect(asrcTrimUpButton, &QPushButton::clicked, this, &SyncTestDock::on_asrc_trim_up_clicked);

	mainLayout->addLayout(asrcButtonsLayout);

	setLayout(mainLayout);

	asrcRefreshTimer = new QTimer(this);
	connect(asrcRefreshTimer, &QTimer::timeout, this, &SyncTestDock::refresh_asrc_ui);
	asrcRefreshTimer->start(CAMERA_BOX_ASRC_REFRESH_MS);
	refresh_asrc_ui(); // first paint immediately, don't wait a full interval on load

	// #690: auto-start once OBS has finished loading — see cb_frontend_event()'s own comment for
	// why (the dock used to sit stopped on dashes after every OBS relaunch until someone noticed
	// and clicked Start; that is exactly the "forgettable toggle" the standing HARD rule bans).
	obs_frontend_add_event_callback(cb_frontend_event, this);
}

SyncTestDock::~SyncTestDock()
{
	obs_frontend_remove_event_callback(cb_frontend_event, this);
	if (sync_test) {
		obs_output_stop(sync_test);
		sync_test = nullptr;
	}
}

// #926 fix-up (review finding 13): the 1s ASRC poll timer should only run while the dock is
// actually visible -- no point hitting obs_get_source_by_name once a second when the dock is
// hidden (a different OBS dock tab active, or the dock undocked and minimized).
void SyncTestDock::showEvent(QShowEvent *event)
{
	QFrame::showEvent(event);
	if (asrcRefreshTimer && !asrcRefreshTimer->isActive()) {
		asrcRefreshTimer->start(CAMERA_BOX_ASRC_REFRESH_MS);
		refresh_asrc_ui(); // repaint immediately, don't wait a full interval after becoming visible
	}
}

void SyncTestDock::hideEvent(QHideEvent *event)
{
	QFrame::hideEvent(event);
	if (asrcRefreshTimer)
		asrcRefreshTimer->stop();
}

void SyncTestDock::cb_frontend_event(enum obs_frontend_event event, void *param)
{
	// #690: OBS_FRONTEND_EVENT_FINISHED_LOADING fires once, after scene collection + all sources
	// are up — video/audio core is already live by then (module load, where this dock itself is
	// created, runs before it). Idempotent: only starts if not already running, so a user who
	// manually stopped it BEFORE this fires (impossible in practice — the event fires once, very
	// early) is never fought with.
	if (event != OBS_FRONTEND_EVENT_FINISHED_LOADING)
		return;
	auto *dock = (SyncTestDock *)param;
	if (!dock->sync_test)
		dock->start();
}

extern "C" QWidget *create_sync_test_dock()
{
	const auto main_window = static_cast<QMainWindow *>(obs_frontend_get_main_window());
	return static_cast<QWidget *>(new SyncTestDock(main_window));
}

#define CD_TO_LOCAL(type, name, get_func) \
	type name;                        \
	if (!get_func(cd, #name, &name))  \
		return;

void SyncTestDock::cb_video_marker_found(void *param, calldata_t *cd)
{
	auto *dock = (SyncTestDock *)param;

	CD_TO_LOCAL(video_marker_found_s *, data, calldata_get_ptr);
	video_marker_found_s found = *data;

	QMetaObject::invokeMethod(dock, [dock, found]() { dock->on_video_marker_found(found); });
};

void SyncTestDock::cb_audio_marker_found(void *param, calldata_t *cd)
{
	auto *dock = (SyncTestDock *)param;

	CD_TO_LOCAL(audio_marker_found_s *, data, calldata_get_ptr);
	audio_marker_found_s found = *data;

	QMetaObject::invokeMethod(dock, [dock, found]() { dock->on_audio_marker_found(found); });
};

void SyncTestDock::cb_sync_found(void *param, calldata_t *cd)
{
	auto *dock = (SyncTestDock *)param;

	CD_TO_LOCAL(sync_index *, data, calldata_get_ptr);
	sync_index found = *data;

	QMetaObject::invokeMethod(dock, [dock, found]() { dock->on_sync_found(found); });
}

void SyncTestDock::cb_lock_state_changed(void *param, calldata_t *cd)
{
	auto *dock = (SyncTestDock *)param;

	CD_TO_LOCAL(bool, locked, calldata_get_bool);

	QMetaObject::invokeMethod(dock, [dock, locked]() { dock->on_lock_state_changed(locked); });
}

// #1177: the measurement input went STALE (no marker/QR decode advance) or recovered.
void SyncTestDock::cb_sync_stale_changed(void *param, calldata_t *cd)
{
	auto *dock = (SyncTestDock *)param;

	CD_TO_LOCAL(bool, stale, calldata_get_bool);

	QMetaObject::invokeMethod(dock, [dock, stale]() { dock->on_sync_stale_changed(stale); });
}

void SyncTestDock::start()
{
	OBSOutputAutoRelease o = obs_output_create(OUTPUT_ID, "sync-test-output", nullptr, nullptr);
	if (!o) {
		blog(LOG_ERROR, "Failed to create sync-test-output.");
		return;
	}

	last_video_ix = last_audio_ix = -1;
	missed_video_ix = missed_audio_ix = 0;
	received_video_ix = received_audio_ix = 0;
	received_video_index_max = 256;
	received_audio_index_max = 256;
	audio_index_max = 256;

	// #926 fix-up (review finding 11): reset the plain-language status back to "measuring" on
	// every (re)start -- otherwise a manual Stop+Start (or the #690 auto-start racing a leftover
	// UI state) could leave the STALE "Locked"/"No test signal" text on screen even though the
	// output was just freshly created and has decided nothing yet.
	if (statusLabel)
		statusLabel->setText(obs_module_text("Status.Measuring"));
	// #1177: clear any leftover STALE greying/label from a prior run on (re)start.
	if (latencyDisplay)
		latencyDisplay->setStyleSheet("");

	auto *sh = obs_output_get_signal_handler(o);
	signal_handler_connect(sh, "video_marker_found", cb_video_marker_found, this);
	signal_handler_connect(sh, "audio_marker_found", cb_audio_marker_found, this);
	signal_handler_connect(sh, "sync_found", cb_sync_found, this);
	signal_handler_connect(sh, "lock_state_changed", cb_lock_state_changed, this); // #926
	signal_handler_connect(sh, "sync_stale_changed", cb_sync_stale_changed, this); // #1177

	bool success = obs_output_start(o);

	if (!success)
		latencyPolarity->setText(obs_module_text("Display.Polarity.Failure"));

	if (startButton)
		startButton->setText(obs_module_text("Button.Stop"));

	sync_test = o;
}

void SyncTestDock::on_start_stop()
{
	if (!sync_test) /* request to start */ {
		start();
	}
	else /* request to stop */ {
		obs_output_stop(sync_test);
		sync_test = nullptr;

		if (startButton)
			startButton->setText(obs_module_text("Button.Start"));
		// #926 fix-up (review finding 11): a manually-stopped dock must say so, not keep
		// claiming "Locked -- holding sync" from before the stop.
		if (statusLabel)
			statusLabel->setText(obs_module_text("Status.Stopped"));
	}
}

static int missed_markers(int index, int last_index, int max_index)
{
	if (index == last_index + 1 || last_index < 0 || max_index <= 0)
		return 0;
	return (max_index + index - last_index - 1) % max_index;
}

void SyncTestDock::on_video_marker_found(struct video_marker_found_s data)
{
	const int index = data.qr_data.index;
	missed_video_ix += missed_markers(index, last_video_ix, received_video_index_max);
	last_video_ix = index;
	received_video_index_max = data.qr_data.index_max;
	received_video_ix++;
	frequencyDisplay->setText(QStringLiteral("%1 Hz").arg(data.qr_data.f));
	int missed = missed_video_ix * 100 / (received_video_ix + missed_video_ix);
	videoIndexDisplay->setText(QStringLiteral("%1 (%2% missed)").arg(index).arg(missed));
}

void SyncTestDock::on_audio_marker_found(struct audio_marker_found_s data)
{
	const int index = data.index;
	// #398: camera-box's audio index is the sparse frame_id low byte (not a +1 counter), so the
	// missed% is meaningless — show the locked index alone. `audio_marker_found` is emitted only for
	// a marker whose offset agrees with the locked cluster, so this index is a genuine one.
	if (data.sparse_index) {
		received_audio_ix++;
		last_audio_ix = index;
		audioIndexDisplay->setText(QStringLiteral("%1").arg(index));
		return;
	}
	missed_audio_ix += missed_markers(index, last_audio_ix, received_audio_index_max);
	last_audio_ix = index;
	received_audio_index_max = data.index_max;
	received_audio_ix++;
	int missed = missed_audio_ix * 100 / (received_audio_ix + missed_audio_ix);
	audioIndexDisplay->setText(QStringLiteral("%1 (%2% missed)").arg(index).arg(missed));
}

void SyncTestDock::on_sync_found(sync_index data)
{
	// #999 -- gate-convert camera-box's own direct-ring events (data.gate_convention == true) the
	// SAME way #953 already converts every OBS log line; norihiro's legacy events
	// (gate_convention == false) reproduce the original behavior byte-for-byte.
	int64_t dock_native_ts_ns = (int64_t)data.audio_ts - (int64_t)data.video_ts;
	camerabox::CbLatencyDisplay disp =
		camerabox::cb_dock_latency_display_ms(dock_native_ts_ns, data.gate_convention);
	latencyDisplay->setText(QStringLiteral("%1 ms").arg(disp.display_ms, 2, 'f', 1));
	// #1177: a fresh sync_found is a genuinely LIVE value -- clear any STALE greying immediately
	// (the diag-tick RecoveredLive transition may lag this by up to one ~10s interval).
	latencyDisplay->setStyleSheet("");
	indexDisplay->setText(QStringLiteral("%1").arg(data.index));
	if (disp.polarity == camerabox::CbLatencyPolarity::Positive)
		latencyPolarity->setText(obs_module_text("Display.Polarity.Positive"));
	else if (disp.polarity == camerabox::CbLatencyPolarity::Negative)
		latencyPolarity->setText(obs_module_text("Display.Polarity.Negative"));
}

// #926: plain-language operator status (requirement 1) -- driven by the lock_state_changed signal
// (fired on the ACTUAL Locked/Unlocked boundary crossing, never spamming on every Updated). Before
// the first lock, statusLabel stays on its constructor default ("measuring").
void SyncTestDock::on_lock_state_changed(bool locked)
{
	statusLabel->setText(obs_module_text(locked ? "Status.Locked" : "Status.NoSignal"));
}

// #1177: the measurement INPUT itself disappeared (EVENT mode: cam2 QPSK marker + dual-QR off) so
// the decode counters stopped advancing -- distinct from lock_state_changed above, which is driven
// by a DECODED marker and so can NEVER fire when the input is exactly what went away. Show an
// explicit STALE / NO-SIGNAL status and grey + label the frozen offset so an operator reads it as
// "not live", never as a current measurement. A later sync_found (input resumed) clears the greying.
void SyncTestDock::on_sync_stale_changed(bool stale)
{
	if (stale) {
		statusLabel->setText(obs_module_text("Status.Stale"));
		latencyDisplay->setStyleSheet("color: gray;");
		latencyPolarity->setText(obs_module_text("Display.Polarity.Stale"));
	} else {
		statusLabel->setText(obs_module_text("Status.Measuring"));
		latencyDisplay->setStyleSheet("");
	}
}

// #926: poll CAMERA_BOX_ASRC_SOURCE_NAME's current ASRC state/numbers and paint the section.
// Called on a timer (no change-notification signal exists for ASRC) and once immediately after
// construction so the dock never sits on placeholder dashes until the first tick.
void SyncTestDock::refresh_asrc_ui()
{
	OBSSourceAutoRelease src = obs_get_source_by_name(CAMERA_BOX_ASRC_SOURCE_NAME);
	if (!src) {
		// #926 fix-up (review finding 16): a clear "not on this box" state -- STRIH runs the
		// SAME plugin DLL but has no 'mbc' source, so a bare "-" reads as "broken" rather than
		// "this box doesn't have it". Log the situation ONCE, not once per 1s poll tick.
		asrcStateLabel->setText(obs_module_text("AsrcState.NotOnThisBox"));
		asrcEstimatedLabel->setText("-");
		asrcAppliedLabel->setText("-");
		asrcTrimLabel->setText("-");
		if (asrcToggleButton)
			asrcToggleButton->setEnabled(false);
		if (asrcTrimDownButton)
			asrcTrimDownButton->setEnabled(false);
		if (asrcTrimUpButton)
			asrcTrimUpButton->setEnabled(false);
		if (!asrcSourceMissingLogged) {
			asrcSourceMissingLogged = true;
			blog(LOG_WARNING,
			     "av-sync-dock: ASRC section unavailable -- source '%s' not found on this box "
			     "(further occurrences suppressed until it appears)",
			     CAMERA_BOX_ASRC_SOURCE_NAME);
		}
		return;
	}
	asrcSourceMissingLogged = false;

	bool enabled = obs_source_get_asrc_enabled(src);
	double estimated = obs_source_get_asrc_estimated_ppm(src);
	double applied = obs_source_get_asrc_applied_ppm(src);
	double trim = obs_source_get_asrc_outer_bias_ppm(src);

	asrcStateLabel->setText(obs_module_text(enabled ? "AsrcState.On" : "AsrcState.Off"));
	asrcEstimatedLabel->setText(QStringLiteral("%1 ppm").arg(estimated, 0, 'f', 2));
	asrcAppliedLabel->setText(QStringLiteral("%1 ppm").arg(applied, 0, 'f', 2));
	asrcTrimLabel->setText(QStringLiteral("%1 ppm").arg(trim, 0, 'f', 1));
	if (asrcToggleButton) {
		asrcToggleButton->setEnabled(true);
		asrcToggleButton->setText(obs_module_text(enabled ? "Button.AsrcToggleOn" : "Button.AsrcToggleOff"));
	}
	if (asrcTrimDownButton)
		asrcTrimDownButton->setEnabled(true);
	if (asrcTrimUpButton)
		asrcTrimUpButton->setEnabled(true);
}

// #926: flip CAMERA_BOX_ASRC_SOURCE_NAME's ASRC on/off -- a plain forward to the already-existing
// core setter (issue #803), immediately re-read back into the UI (never wait for the next poll
// tick after an operator's own click).
void SyncTestDock::on_asrc_toggle_clicked()
{
	OBSSourceAutoRelease src = obs_get_source_by_name(CAMERA_BOX_ASRC_SOURCE_NAME);
	if (!src)
		return;
	bool enabled = obs_source_get_asrc_enabled(src);
	obs_source_set_asrc_enabled(src, !enabled);
	refresh_asrc_ui();
}

// #926: nudge CAMERA_BOX_ASRC_SOURCE_NAME's manual ppm trim (the existing outer-loop bias, issue
// #806) down/up by CAMERA_BOX_ASRC_TRIM_STEP_PPM -- a plain forward to the already-existing core
// setter, which clamps to +/-10ppm at the servo itself regardless of what is requested here.
void SyncTestDock::on_asrc_trim_down_clicked()
{
	OBSSourceAutoRelease src = obs_get_source_by_name(CAMERA_BOX_ASRC_SOURCE_NAME);
	if (!src)
		return;
	double trim = obs_source_get_asrc_outer_bias_ppm(src);
	obs_source_set_asrc_outer_bias_ppm(src, trim - CAMERA_BOX_ASRC_TRIM_STEP_PPM);
	refresh_asrc_ui();
}

void SyncTestDock::on_asrc_trim_up_clicked()
{
	OBSSourceAutoRelease src = obs_get_source_by_name(CAMERA_BOX_ASRC_SOURCE_NAME);
	if (!src)
		return;
	double trim = obs_source_get_asrc_outer_bias_ppm(src);
	obs_source_set_asrc_outer_bias_ppm(src, trim + CAMERA_BOX_ASRC_TRIM_STEP_PPM);
	refresh_asrc_ui();
}
