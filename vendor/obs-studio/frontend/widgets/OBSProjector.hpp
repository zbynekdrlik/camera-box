#pragma once

#include "OBSQTDisplay.hpp"

class Multiview;

enum class ProjectorType {
	Source,
	Scene,
	Preview,
	StudioProgram,
	Multiview,
};

class OBSProjector : public OBSQTDisplay {
	Q_OBJECT

private:
	OBSWeakSourceAutoRelease weakSource;
	std::vector<OBSSignal> sigs;

	static void OBSRenderMultiview(void *data, uint32_t cx, uint32_t cy);
	static void OBSRender(void *data, uint32_t cx, uint32_t cy);
	static void OBSSourceRenamed(void *data, calldata_t *params);
	static void OBSSourceDestroyed(void *data, calldata_t *params);

	void mousePressEvent(QMouseEvent *event) override;
	void mouseDoubleClickEvent(QMouseEvent *event) override;
	void closeEvent(QCloseEvent *event) override;
	// camera-box #1352: on Linux the projector is a native CHILD of a plain host
	// toplevel. This filter catches the host window's Close (WM "X") so the projector's
	// own close bookkeeping (multiviewProjectors / SaveProjectors) runs before the host
	// is deleted. Inert when unhosted (the filter is only installed on Linux).
	bool eventFilter(QObject *watched, QEvent *event) override;

	// camera-box #1352: the toplevel window this projector drives. window() IS `this`
	// when the projector is unhosted (Windows / no-host path — byte-identical), and the
	// host toplevel when the projector is a hosted child (Linux).
	QWidget *Toplevel();

	bool isAlwaysOnTop;
	bool isAlwaysOnTopOverridden = false;
	// camera-box #1352: re-entrancy guard shared by the two host-teardown directions
	// (host Close -> DeleteProjector, and projector destroyed -> host deleteLater).
	bool closing = false;
	int savedMonitor = -1;
	ProjectorType type = ProjectorType::Source;

	Multiview *multiview = nullptr;

	bool ready = false;

	void UpdateMultiview();
	void UpdateProjectorTitle(QString name);

	QRect prevGeometry;
	void SetMonitor(int monitor);

private slots:
	void EscapeTriggered();
	void OpenFullScreenProjector();
	void ResizeToContent();
	void OpenWindowedProjector();
	void AlwaysOnTopToggled(bool alwaysOnTop);
	void ScreenRemoved(QScreen *screen);
	void RenameProjector(QString oldName, QString newName);

public:
	OBSProjector(QWidget *widget, obs_source_t *source_, int monitor, ProjectorType type_);
	~OBSProjector();

	OBSSource GetSource();
	ProjectorType GetProjectorType();
	int GetMonitor();
	static void UpdateMultiviewProjectors();
	void SetHideCursor();

	bool IsAlwaysOnTop() const;
	bool IsAlwaysOnTopOverridden() const;
	void SetIsAlwaysOnTop(bool isAlwaysOnTop, bool isOverridden);
};
