#pragma once

/* camera-box issue 1387 -- the JPEG XS indicator in the status bar, beside the genlock LOCK
 * widget (owner request 29.10.2026, design 6506884476). A permanent label with its own always-on
 * 1 Hz timer: it reads DistroAV's JPEG XS state table through the libobs proc handler
 * `camera_box_jxs_state`, grades it (GenlockJxsReport.cpp over the parity-gated GenlockJxsState.hpp)
 * and shows `JPEG XS: ...` in the LOCK widget's colours, a per-row tooltip, and one `jxs-state:` log
 * line on every change. No procedure (a build without the JPEG XS source, the Windows bundle) =
 * OFF. No meta-object macro: the timer connects to a lambda, so the class needs no moc. */

#include <QLabel>

#include <string>

class QTimer;

class GenlockJxsWidget : public QLabel {
public:
	explicit GenlockJxsWidget(QWidget *parent = nullptr);

private:
	void Tick();

	QTimer *timer = nullptr;
	/* the jxs-state: line's change key (state, reason, offender) */
	int lastState = -1;
	int lastReason = -1;
	std::string lastOffender;
};
