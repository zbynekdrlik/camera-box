#include "OBSProjector.hpp"

#include <OBSApp.hpp>
#include <components/Multiview.hpp>
#include <utility/display-helpers.hpp>
#include <utility/platform.hpp>
#include <widgets/OBSBasic.hpp>

#include <qt-wrappers.hpp>

#include <QEvent>
#include <QScreen>
#include <QWindow>

#include "moc_OBSProjector.cpp"

static QList<OBSProjector *> multiviewProjectors;

static bool updatingMultiview = false, mouseSwitching, transitionOnDoubleClick;

// camera-box #1352: choose the projector's window flags. On Linux (XWayland + NVIDIA
// PRIME) a projector whose GL surface IS the X toplevel window stalls the graphics thread
// ~0.5 s per present, so when a host toplevel parent is provided the display is built as a
// native CHILD (Qt::Widget) and the host is the toplevel. Everywhere else, and on the
// no-host path, it stays a toplevel window (Qt::Window) exactly as before.
static Qt::WindowFlags projectorWindowFlags(QWidget *host)
{
#if defined(__linux__)
	if (host)
		return Qt::Widget;
#else
	(void)host;
#endif
	return Qt::Window;
}

OBSProjector::OBSProjector(QWidget *widget, obs_source_t *source_, int monitor, ProjectorType type_)
	: OBSQTDisplay(widget, projectorWindowFlags(widget)),
	  weakSource(OBSGetWeakRef(source_))
{
	OBSSource source = GetSource();
	if (source) {
		sigs.emplace_back(obs_source_get_signal_handler(source), "rename", OBSSourceRenamed, this);
		sigs.emplace_back(obs_source_get_signal_handler(source), "destroy", OBSSourceDestroyed, this);
	}

	isAlwaysOnTop = config_get_bool(App()->GetUserConfig(), "BasicWindow", "ProjectorAlwaysOnTop");

	if (isAlwaysOnTop) {
		Toplevel()->setWindowFlags(Qt::WindowStaysOnTopHint);
	}

	// Mark the window as a projector so SetDisplayAffinity
	// can skip it. camera-box #1352: set it on the TOPLEVEL's window handle (the
	// projector's own handle when unhosted). On the Linux hosted path the host toplevel
	// is not realized yet at ctor time (so its handle is null there) — harmless, since
	// the property is only ever read on Windows (SetDisplayAffinitySupported() is false
	// on X11).
	if (QWindow *handle = Toplevel()->windowHandle()) {
		handle->setProperty("isOBSProjectorWindow", true);
	}

#if defined(__linux__) || defined(__FreeBSD__) || defined(__DragonFly__)
	// Prevents resizing of projector windows
	setAttribute(Qt::WA_PaintOnScreen, false);
#endif

	type = type_;
#ifndef __APPLE__
	Toplevel()->setWindowIcon(QIcon::fromTheme("obs", QIcon(":/res/images/obs.png")));
#endif

	if (monitor == -1) {
		Toplevel()->resize(480, 270);
	} else {
		SetMonitor(monitor);
	}

	if (source) {
		UpdateProjectorTitle(QT_UTF8(obs_source_get_name(source)));
	} else {
		UpdateProjectorTitle(QString());
	}

	QAction *action = new QAction(this);
	action->setShortcut(Qt::Key_Escape);
	addAction(action);
	connect(action, &QAction::triggered, this, &OBSProjector::EscapeTriggered);

	setAttribute(Qt::WA_DeleteOnClose, true);

	//disable application quit when last window closed
	setAttribute(Qt::WA_QuitOnClose, false);

	installEventFilter(CreateShortcutFilter());

	auto addDrawCallback = [this]() {
		bool isMultiview = type == ProjectorType::Multiview;
		obs_display_add_draw_callback(GetDisplay(), isMultiview ? OBSRenderMultiview : OBSRender, this);
		obs_display_set_background_color(GetDisplay(), 0x000000);
		// camera-box #276: the built-in Multiview projector renders a thumbnail of
		// every scene each frame (9-18ms) on the SAME graphics thread that presents
		// the program output, AFTER output_frames() — at 60fps that 9-18ms overruns
		// the 16.6ms budget and breaks the program render whenever the multiview is
		// open (measured: program 4.9ms/0-skip closed → 13.7-14.2ms with it open).
		// Halve the MULTIVIEW display's render rate (every other frame) so monitoring
		// never steals the program budget. ONLY the multiview is throttled; program
		// output + preview keep divisor 1 (render every frame) and are unaffected.
		if (isMultiview)
			obs_display_set_render_divisor(GetDisplay(), 2);
		// camera-box #1107: a FULLSCREEN (savedMonitor > -1) NON-multiview projector is a
		// program output on a physical display (imag-nb: the HDMI-1 IMAG projection). Mark
		// its display vsync so the EGL present swaps tear-free (eglSwapInterval 1). Windowed
		// projectors, the preview, the OBS main window and the multiview all stay interval-0
		// (no added blocking present — multiview tear is acceptable operator monitoring, and
		// render_divisor <= 1 alone cannot discriminate program from the divisor-0 main window).
		// INVARIANT: exactly ONE fullscreen non-multiview projector is expected (imag-nb: the
		// HDMI-1 program). A second such projector would also arm vsync and stack a second
		// blocking present per tick — safe for imag's single-projector config, not for N.
		if (savedMonitor > -1 && !isMultiview)
			obs_display_set_vsync(GetDisplay(), true);
	};

	connect(this, &OBSQTDisplay::DisplayCreated, this, addDrawCallback);
	connect(App(), &QGuiApplication::screenRemoved, this, &OBSProjector::ScreenRemoved);

	if (type == ProjectorType::Multiview) {
		multiview = new Multiview();

		UpdateMultiview();

		multiviewProjectors.push_back(this);
	}

	App()->IncrementSleepInhibition();

	if (source) {
		obs_source_inc_showing(source);
	}

	ready = true;

	show();

	// We need it here to allow keyboard input in X11 to listen to Escape
	activateWindow();
}

OBSProjector::~OBSProjector()
{
	sigs.clear();

	bool isMultiview = type == ProjectorType::Multiview;
	obs_display_remove_draw_callback(GetDisplay(), isMultiview ? OBSRenderMultiview : OBSRender, this);

	OBSSource source = GetSource();
	if (source) {
		obs_source_dec_showing(source);
	}

	if (isMultiview) {
		delete multiview;
		multiviewProjectors.removeAll(this);
	}

	App()->DecrementSleepInhibition();

	// camera-box #1352: on Linux the projector is a native child of a plain host toplevel.
	// Whatever deletes the projector (Escape, DeleteProjector, monitor-replace,
	// CloseAllProjectors) must also tear down the host so no empty toplevel is left behind.
	// `closing` is set only when the host itself is already handling its own deletion (its
	// Close was intercepted in eventFilter), so we never re-close a host mid-destruction.
	// window() == this when unhosted (Windows / no-host), so this is inert there.
	QWidget *top = Toplevel();
	if (top != this && !closing) {
		top->deleteLater();
	}
}

QWidget *OBSProjector::Toplevel()
{
	return window();
}

bool OBSProjector::eventFilter(QObject *watched, QEvent *event)
{
	// camera-box #1352: the host toplevel is closing (WM "X"). Run the projector's own
	// close bookkeeping first (removes it from OBSBasic::projectors + multiviewProjectors
	// and schedules its deletion) so SaveProjectors stays consistent; then let the host
	// proceed to delete itself (WA_DeleteOnClose) with the projector as its child.
	if (watched == Toplevel() && event->type() == QEvent::Close) {
		if (!closing) {
			closing = true;
			OBSBasic::Get()->DeleteProjector(this);
		}
		return false;
	}

	return OBSQTDisplay::eventFilter(watched, event);
}

void OBSProjector::SetMonitor(int monitor)
{
	savedMonitor = monitor;
	Toplevel()->setGeometry(QGuiApplication::screens()[monitor]->geometry());
	Toplevel()->showFullScreen();
	SetHideCursor();
}

void OBSProjector::SetHideCursor()
{
	if (savedMonitor == -1) {
		return;
	}

	bool hideCursor = config_get_bool(App()->GetUserConfig(), "BasicWindow", "HideProjectorCursor");

	if (hideCursor && type != ProjectorType::Multiview) {
		setCursor(Qt::BlankCursor);
	} else {
		setCursor(Qt::ArrowCursor);
	}
}

void OBSProjector::OBSRenderMultiview(void *data, uint32_t cx, uint32_t cy)
{
	OBSProjector *window = (OBSProjector *)data;

	if (updatingMultiview || !window->ready) {
		return;
	}

	window->multiview->Render(cx, cy);
	// camera-box #1260 lever (1): publish this render's per-cell CPU-timing aggregate into the
	// projector's display audit window (mv_cells/mv_cell_ms/mv_top1/... on the multiview-audit
	// line). Same graphics thread as Render + render_display, so no locks. Report-only.
	window->multiview->ReportCellStats(window->GetDisplay());
}

void OBSProjector::OBSRender(void *data, uint32_t cx, uint32_t cy)
{
	OBSProjector *window = static_cast<OBSProjector *>(data);

	if (!window->ready) {
		return;
	}

	OBSBasic *main = OBSBasic::Get();
	OBSSource source = window->GetSource();

	uint32_t targetCX;
	uint32_t targetCY;
	int x, y;
	int newCX, newCY;
	float scale;

	if (source) {
		targetCX = std::max(obs_source_get_width(source), 1u);
		targetCY = std::max(obs_source_get_height(source), 1u);
	} else {
		struct obs_video_info ovi;
		obs_get_video_info(&ovi);
		targetCX = ovi.base_width;
		targetCY = ovi.base_height;
	}

	GetScaleAndCenterPos(targetCX, targetCY, cx, cy, x, y, scale);

	newCX = int(scale * float(targetCX));
	newCY = int(scale * float(targetCY));

	startRegion(x, y, newCX, newCY, 0.0f, float(targetCX), 0.0f, float(targetCY));

	if (window->type == ProjectorType::Preview && main->IsPreviewProgramMode()) {
		OBSSource curSource = main->GetCurrentSceneSource();

		if (source != curSource) {
			obs_source_dec_showing(source);
			obs_source_inc_showing(curSource);
			source = curSource;
			window->weakSource = OBSGetWeakRef(source);
		}
	} else if (window->type == ProjectorType::Preview && !main->IsPreviewProgramMode()) {
		window->weakSource = nullptr;
	}

	if (source) {
		obs_source_video_render(source);
	} else {
		obs_render_main_texture();
	}

	endRegion();
}

void OBSProjector::OBSSourceRenamed(void *data, calldata_t *params)
{
	OBSProjector *window = static_cast<OBSProjector *>(data);
	QString oldName = calldata_string(params, "prev_name");
	QString newName = calldata_string(params, "new_name");

	QMetaObject::invokeMethod(window, "RenameProjector", Q_ARG(QString, oldName), Q_ARG(QString, newName));
}

void OBSProjector::OBSSourceDestroyed(void *data, calldata_t *)
{
	OBSProjector *window = static_cast<OBSProjector *>(data);
	QMetaObject::invokeMethod(window, "EscapeTriggered");
}

void OBSProjector::mouseDoubleClickEvent(QMouseEvent *event)
{
	OBSQTDisplay::mouseDoubleClickEvent(event);

	if (!mouseSwitching) {
		return;
	}

	if (!transitionOnDoubleClick) {
		return;
	}

	// Only MultiView projectors handle double click
	if (this->type != ProjectorType::Multiview) {
		return;
	}

	OBSBasic *main = (OBSBasic *)obs_frontend_get_main_window();
	if (!main->IsPreviewProgramMode()) {
		return;
	}

	if (event->button() == Qt::LeftButton) {
		QPoint pos = event->pos();
		OBSSource src = multiview->GetSourceByPosition(pos.x(), pos.y());
		if (!src) {
			return;
		}

		if (main->GetProgramSource() != src) {
			main->TransitionToScene(src);
		}
	}
}

void OBSProjector::mousePressEvent(QMouseEvent *event)
{
	OBSQTDisplay::mousePressEvent(event);

	if (event->button() == Qt::RightButton) {
		QMenu *projectorMenu = new QMenu(QTStr("Fullscreen"));
		OBSBasic::AddProjectorMenuMonitors(projectorMenu, this, &OBSProjector::OpenFullScreenProjector);

		QMenu popup(this);
		popup.addMenu(projectorMenu);

		if (GetMonitor() > -1) {
			popup.addAction(QTStr("Windowed"), this, &OBSProjector::OpenWindowedProjector);

		} else if (!Toplevel()->isMaximized()) {
			popup.addAction(QTStr("Projector.ResizeWindowToContent"), this, &OBSProjector::ResizeToContent);
		}

		QAction *alwaysOnTopButton = new QAction(QTStr("Basic.MainMenu.View.AlwaysOnTop"), this);
		alwaysOnTopButton->setCheckable(true);
		alwaysOnTopButton->setChecked(isAlwaysOnTop);

		connect(alwaysOnTopButton, &QAction::toggled, this, &OBSProjector::AlwaysOnTopToggled);

		popup.addAction(alwaysOnTopButton);

		popup.addAction(QTStr("Close"), this, &OBSProjector::EscapeTriggered);
		popup.exec(QCursor::pos());
	} else if (event->button() == Qt::LeftButton) {
		// Only MultiView projectors handle left click
		if (this->type != ProjectorType::Multiview) {
			return;
		}

		if (!mouseSwitching) {
			return;
		}

		QPoint pos = event->pos();
		OBSSource src = multiview->GetSourceByPosition(pos.x(), pos.y());
		if (!src) {
			return;
		}

		OBSBasic *main = (OBSBasic *)obs_frontend_get_main_window();
		if (main->GetCurrentSceneSource() != src) {
			main->SetCurrentScene(src, false);
		}
	}
}

void OBSProjector::EscapeTriggered()
{
	OBSBasic *main = OBSBasic::Get();
	main->DeleteProjector(this);
}

void OBSProjector::UpdateMultiview()
{
	MultiviewLayout multiviewLayout =
		static_cast<MultiviewLayout>(config_get_int(App()->GetUserConfig(), "BasicWindow", "MultiviewLayout"));

	bool drawLabel = config_get_bool(App()->GetUserConfig(), "BasicWindow", "MultiviewDrawNames");

	bool drawSafeArea = config_get_bool(App()->GetUserConfig(), "BasicWindow", "MultiviewDrawAreas");

	mouseSwitching = config_get_bool(App()->GetUserConfig(), "BasicWindow", "MultiviewMouseSwitch");

	transitionOnDoubleClick = config_get_bool(App()->GetUserConfig(), "BasicWindow", "TransitionOnDoubleClick");

	multiview->Update(multiviewLayout, drawLabel, drawSafeArea);
}

void OBSProjector::UpdateProjectorTitle(QString name)
{
	QString title = nullptr;
	switch (type) {
	case ProjectorType::Scene:
		title = QTStr("Projector.Title") + " - " + QTStr("Projector.Title.Scene").arg(name);
		break;
	case ProjectorType::Source:
		title = QTStr("Projector.Title") + " - " + QTStr("Projector.Title.Source").arg(name);
		break;
	case ProjectorType::Preview:
		title = QTStr("Projector.Title") + " - " + QTStr("StudioMode.Preview");
		break;
	case ProjectorType::StudioProgram:
		title = QTStr("Projector.Title") + " - " + QTStr("StudioMode.Program");
		break;
	case ProjectorType::Multiview:
		title = QTStr("Projector.Title") + " - " + QTStr("Projector.Title.Multiview");
		break;
	default:
		title = name;
		break;
	}

	Toplevel()->setWindowTitle(title);
}

OBSSource OBSProjector::GetSource()
{
	return OBSGetStrongRef(weakSource);
}

ProjectorType OBSProjector::GetProjectorType()
{
	return type;
}

int OBSProjector::GetMonitor()
{
	return savedMonitor;
}

void OBSProjector::UpdateMultiviewProjectors()
{
	obs_enter_graphics();
	updatingMultiview = true;
	obs_leave_graphics();

	for (auto &projector : multiviewProjectors) {
		projector->UpdateMultiview();
	}

	obs_enter_graphics();
	updatingMultiview = false;
	obs_leave_graphics();
}

void OBSProjector::RenameProjector(QString oldName, QString newName)
{
	if (oldName == newName) {
		return;
	}

	UpdateProjectorTitle(newName);
}

void OBSProjector::OpenFullScreenProjector()
{
	if (!Toplevel()->isFullScreen()) {
		prevGeometry = Toplevel()->geometry();
	}

	int monitor = sender()->property("monitor").toInt();
	SetMonitor(monitor);

	// camera-box #1107: this projector is now FULLSCREEN. The DisplayCreated mark only runs
	// ONCE, so a runtime windowed->fullscreen toggle would otherwise leave a program output
	// un-vsynced (torn). Re-arm here: vsync a non-multiview (program) projector, not the
	// multiview. Mirrors the startup mark (savedMonitor is now > -1 by SetMonitor above).
	obs_display_set_vsync(GetDisplay(), type != ProjectorType::Multiview);

	OBSSource source = GetSource();
	UpdateProjectorTitle(QT_UTF8(obs_source_get_name(source)));
}

void OBSProjector::OpenWindowedProjector()
{
	Toplevel()->showFullScreen();
	Toplevel()->showNormal();
	setCursor(Qt::ArrowCursor);

	if (!prevGeometry.isNull()) {
		Toplevel()->setGeometry(prevGeometry);
	} else {
		Toplevel()->resize(480, 270);
	}

	savedMonitor = -1;

	// camera-box #1107: now WINDOWED — clear vsync so a windowed projector does not keep a
	// blocking vblank present it acquired while fullscreen.
	obs_display_set_vsync(GetDisplay(), false);

	OBSSource source = GetSource();
	UpdateProjectorTitle(QT_UTF8(obs_source_get_name(source)));
}

void OBSProjector::ResizeToContent()
{
	OBSSource source = GetSource();
	uint32_t targetCX;
	uint32_t targetCY;
	int x, y, newX, newY;
	float scale;

	if (source) {
		targetCX = std::max(obs_source_get_width(source), 1u);
		targetCY = std::max(obs_source_get_height(source), 1u);
	} else {
		struct obs_video_info ovi;
		obs_get_video_info(&ovi);
		targetCX = ovi.base_width;
		targetCY = ovi.base_height;
	}

	QSize size = this->size();
	GetScaleAndCenterPos(targetCX, targetCY, size.width(), size.height(), x, y, scale);

	newX = size.width() - (x * 2);
	newY = size.height() - (y * 2);
	Toplevel()->resize(newX, newY);
}

void OBSProjector::AlwaysOnTopToggled(bool isAlwaysOnTop)
{
	SetIsAlwaysOnTop(isAlwaysOnTop, true);
}

void OBSProjector::closeEvent(QCloseEvent *event)
{
	EscapeTriggered();
	event->accept();
}

bool OBSProjector::IsAlwaysOnTop() const
{
	return isAlwaysOnTop;
}

bool OBSProjector::IsAlwaysOnTopOverridden() const
{
	return isAlwaysOnTopOverridden;
}

void OBSProjector::SetIsAlwaysOnTop(bool isAlwaysOnTop, bool isOverridden)
{
	this->isAlwaysOnTop = isAlwaysOnTop;
	this->isAlwaysOnTopOverridden = isOverridden;

	// camera-box #1352: apply the stays-on-top flag to the host TOPLEVEL, not the hosted
	// child (SetAlwaysOnTop does setWindowFlags + show, which on a child would try to make
	// it a toplevel again). Toplevel() == this when unhosted (byte-identical on Windows).
	SetAlwaysOnTop(Toplevel(), isAlwaysOnTop);
}

void OBSProjector::ScreenRemoved(QScreen *screen)
{
	if (GetMonitor() < 0) {
		return;
	}

	if (screen == Toplevel()->screen()) {
		EscapeTriggered();
	}
}
