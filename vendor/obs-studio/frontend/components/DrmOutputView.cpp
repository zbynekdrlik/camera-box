/*
 * camera-box issue 1346 — the frontend half of the selectable DRM-lease HDMI output view.
 * See DrmOutputView.hpp and libobs/obs-drm-output-view.c for the design (owner 24.9.2026: the
 * strih-lx HDMI output is the imag hardware DRM-lease output, selectable Program / Multiview; the
 * Multiview is the BUILT-IN frontend render, never a custom scene).
 *
 * Threading: everything here runs on the Qt UI thread except RenderDrmMultiview, which libobs
 * calls on the graphics thread with the graphics context held. The Multiview's Update is fenced
 * exactly like OBSProjector::UpdateMultiviewProjectors (a flag flipped under the graphics
 * context), and the renderer is unregistered (a graphics-context-serialised call) BEFORE the
 * Multiview is deleted, so a render can never touch a freed instance.
 */

#include "DrmOutputView.hpp"

#if defined(__linux__)

#include <OBSApp.hpp>
#include <components/Multiview.hpp>

#include <obs-drm-output.h>
#include <obs-frontend-api.h>

#include <QAction>
#include <QActionGroup>
#include <QObject>

namespace {

Multiview *drmMultiview = nullptr;
bool drmMultiviewUpdating = false; /* guarded by the graphics context */
QAction *actionProgram = nullptr;
QAction *actionMultiview = nullptr;

void RenderDrmMultiview(void *, uint32_t cx, uint32_t cy)
{
	if (drmMultiviewUpdating || !drmMultiview)
		return;
	drmMultiview->Render(cx, cy);
}

/* The SAME BasicWindow settings OBSProjector::UpdateMultiview applies, so the HDMI grid matches
 * the operator's projector Multiview (layout, scene names, safe areas). */
void UpdateDrmMultiview()
{
	MultiviewLayout layout =
		static_cast<MultiviewLayout>(config_get_int(App()->GetUserConfig(), "BasicWindow", "MultiviewLayout"));
	bool drawLabel = config_get_bool(App()->GetUserConfig(), "BasicWindow", "MultiviewDrawNames");
	bool drawSafeArea = config_get_bool(App()->GetUserConfig(), "BasicWindow", "MultiviewDrawAreas");

	obs_enter_graphics();
	drmMultiviewUpdating = true;
	obs_leave_graphics();

	drmMultiview->Update(layout, drawLabel, drawSafeArea);

	obs_enter_graphics();
	drmMultiviewUpdating = false;
	obs_leave_graphics();
}

void ClearDrmMultiview()
{
	if (!drmMultiview)
		return;
	/* Serialised with the graphics thread: after this returns no render is in flight. */
	obs_drm_output_set_view_renderer(nullptr, nullptr);
	delete drmMultiview;
	drmMultiview = nullptr;
	blog(LOG_INFO, "drm-output: frontend Multiview detached from the HDMI output");
}

void UpdateMenu()
{
	if (!actionProgram || !actionMultiview)
		return;
	const bool active = obs_drm_output_active();
	const bool multiview = obs_drm_output_get_view() == OBS_DRM_OUTPUT_VIEW_MULTIVIEW;
	actionProgram->setChecked(!multiview);
	actionMultiview->setChecked(multiview);
	actionProgram->setEnabled(active);
	actionMultiview->setEnabled(active);
}

/* Hold ONE built-in Multiview exactly while the output is active AND the view is MULTIVIEW: a
 * Program view (or no leased output) costs nothing — no extra Multiview, no shown twin scenes. */
void SyncDrmMultiview()
{
	const bool want = obs_drm_output_active() && obs_drm_output_get_view() == OBS_DRM_OUTPUT_VIEW_MULTIVIEW;
	if (want && !drmMultiview) {
		drmMultiview = new Multiview();
		UpdateDrmMultiview();
		obs_drm_output_set_view_renderer(RenderDrmMultiview, nullptr);
		blog(LOG_INFO, "drm-output: frontend Multiview attached to the HDMI output");
	} else if (!want && drmMultiview) {
		ClearDrmMultiview();
	}
	UpdateMenu();
}

void SelectView(enum obs_drm_output_view view)
{
	obs_drm_output_set_view(view);
	SyncDrmMultiview();
}

void OnFrontendEvent(enum obs_frontend_event event, void *)
{
	switch (event) {
	case OBS_FRONTEND_EVENT_FINISHED_LOADING:
	case OBS_FRONTEND_EVENT_SCENE_COLLECTION_CHANGED:
		SyncDrmMultiview();
		break;
	case OBS_FRONTEND_EVENT_EXIT:
		ClearDrmMultiview();
		break;
	default:
		break;
	}
}

} // namespace

void DrmOutputViewInit()
{
	actionProgram = static_cast<QAction *>(obs_frontend_add_tools_menu_qaction("HDMI výstup: Program"));
	actionMultiview = static_cast<QAction *>(obs_frontend_add_tools_menu_qaction("HDMI výstup: Multiview"));
	actionProgram->setCheckable(true);
	actionMultiview->setCheckable(true);
	QActionGroup *group = new QActionGroup(actionProgram->parent());
	group->setExclusive(true);
	group->addAction(actionProgram);
	group->addAction(actionMultiview);
	QObject::connect(actionProgram, &QAction::triggered, [] { SelectView(OBS_DRM_OUTPUT_VIEW_PROGRAM); });
	QObject::connect(actionMultiview, &QAction::triggered, [] { SelectView(OBS_DRM_OUTPUT_VIEW_MULTIVIEW); });

	obs_frontend_add_event_callback(OnFrontendEvent, nullptr);
	UpdateMenu();
}

void DrmOutputViewRefresh()
{
	if (drmMultiview)
		UpdateDrmMultiview();
}

void DrmOutputViewClear()
{
	ClearDrmMultiview();
}

#endif /* defined(__linux__) */
