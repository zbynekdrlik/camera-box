// issue 1470: restore an OBS user.ini DockState with the REAL Qt and report each dock's placement.
//
// Proves the seed scripts/obs_dock_state.py writes is a state Qt accepts and that it puts the
// A/V-sync dock where the seed claims (docked in the right area, visible). The harness builds the
// window the way OBS does: every dock the state names exists as a QDockWidget with that object name,
// the plugin dock is added like `obs_frontend_add_dock_by_id` adds it (right area, then hidden +
// floating), and only then is the state restored (OBSBasic.cpp restores after the plugins load).
//
//   dock_state_restore_1470 <base64 DockState> <dock name> [<dock name> ...]
//
// Prints `restore=ok|failed`, then one line per dock: `<name> visible=0|1 floating=0|1 area=<n>`
// (Qt::DockWidgetArea: 1 left, 2 right, 4 top, 8 bottom, 0 none), then `state=<base64>` = the
// window's saveState() after the restore: what OBS writes back to user.ini on exit. Run with
// QT_QPA_PLATFORM=offscreen.
#include <QApplication>
#include <QByteArray>
#include <QDockWidget>
#include <QLabel>
#include <QMainWindow>
#include <cstdio>
#include <vector>

int main(int argc, char **argv)
{
	if (argc < 3) {
		std::fprintf(stderr, "usage: %s <base64 DockState> <dock name> [...]\n", argv[0]);
		return 2;
	}
	QApplication app(argc, argv);
	QMainWindow win;
	win.setCentralWidget(new QLabel("central"));
	std::vector<QDockWidget *> docks;
	for (int i = 2; i < argc; i++) {
		auto *d = new QDockWidget(QString::fromUtf8(argv[i]), &win);
		d->setObjectName(QString::fromUtf8(argv[i]));
		d->setWidget(new QLabel(argv[i]));
		// The first name is the plugin dock: OBS adds it to the right area, then hides it and floats it.
		win.addDockWidget(i == 2 ? Qt::RightDockWidgetArea : Qt::BottomDockWidgetArea, d);
		if (i == 2) {
			d->setVisible(false);
			d->setFloating(true);
		}
		docks.push_back(d);
	}
	win.show();
	const bool ok = win.restoreState(QByteArray::fromBase64(QByteArray(argv[1])));
	std::printf("restore=%s\n", ok ? "ok" : "failed");
	for (QDockWidget *d : docks)
		std::printf("%s visible=%d floating=%d area=%d\n", d->objectName().toUtf8().constData(),
			    d->isHidden() ? 0 : 1, d->isFloating() ? 1 : 0, (int)win.dockWidgetArea(d));
	std::printf("state=%s\n", win.saveState().toBase64().constData());
	return ok ? 0 : 1;
}
