#pragma once

#include <obs.hpp>

#include <QWidget>

class QTimer;

#define GREY_COLOR_BACKGROUND 0xFF4C4C4C

class OBSQTDisplay : public QWidget {
	Q_OBJECT
	Q_PROPERTY(QColor displayBackgroundColor MEMBER backgroundColor READ GetDisplayBackgroundColor WRITE
			   SetDisplayBackgroundColor)

	OBSDisplay display;
	bool destroying = false;
	/* camera-box #1358: a window drag emits a resize per step; applying each one
	 * (obs_display_resize -> gs_resize on the shared graphics thread) stalled the
	 * PROGRAM render. resizeEvent (re)starts this single-shot timer and
	 * ApplyDisplayResize applies the final size once. The first resize after the
	 * display is created stays immediate (resizeImmediate). */
	QTimer *resizeDebounce = nullptr;
	bool resizeImmediate = false;
	void ApplyDisplayResize();

protected:
	virtual void paintEvent(QPaintEvent *event) override;
	virtual void moveEvent(QMoveEvent *event) override;
	virtual void resizeEvent(QResizeEvent *event) override;
	virtual bool nativeEvent(const QByteArray &eventType, void *message, qintptr *result) override;

signals:
	void DisplayCreated(OBSQTDisplay *window);
	void DisplayResized();

public:
	OBSQTDisplay(QWidget *parent = nullptr, Qt::WindowFlags flags = Qt::WindowFlags());
	~OBSQTDisplay() { display = nullptr; }

	virtual QPaintEngine *paintEngine() const override;

	inline obs_display_t *GetDisplay() const { return display; }

	uint32_t backgroundColor = GREY_COLOR_BACKGROUND;

	QColor GetDisplayBackgroundColor() const;
	void SetDisplayBackgroundColor(const QColor &color);
	void UpdateDisplayBackgroundColor();
	void CreateDisplay();
	void DestroyDisplay()
	{
		display = nullptr;
		destroying = true;
	};

	void OnMove();
	void OnDisplayChange();
};
