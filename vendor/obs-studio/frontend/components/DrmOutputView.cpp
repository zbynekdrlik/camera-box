/*
 * camera-box issue 1346 / 1424 — the frontend half of the selectable DRM-lease HDMI output view.
 * See DrmOutputView.hpp and libobs/obs-drm-output-view.c for the design (owner 24.9.2026: the
 * strih-snv HDMI output is the imag hardware DRM-lease output, selectable Program / Multiview; the
 * Multiview is the BUILT-IN frontend render, never a custom scene).
 *
 * issue 1424 part 2 (owner 13.10.2026): the module drives up to OBS_DRM_OUTPUT_MAX_OUTPUTS outputs at
 * once (strih-pp: HDMI-0 = the cutter's Multiview, HDMI-1 = the vestibule Program). Each output gets
 * its own Tools-menu pair "HDMI výstup <connector>: Program / Multiview" (a one-output box keeps
 * today's two labels), its own built-in Multiview while it shows the Multiview, and its choice is
 * persisted into ITS OWN entry of the config (obs_drm_output_set_view_of).
 *
 * Threading: everything here runs on the Qt UI thread except RenderDrmMultiview, which libobs
 * calls on the graphics thread with the graphics context held. Each Multiview's Update is fenced
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
#include <QString>

namespace {

/* One HDMI output's frontend state: its Multiview (held only while that output is active AND shows
 * the Multiview) and its Tools-menu pair. */
struct DrmOutputSlot {
	int index = 0;
	Multiview *multiview = nullptr;
	bool updating = false; /* guarded by the graphics context */
	QAction *actionProgram = nullptr;
	QAction *actionMultiview = nullptr;
};

DrmOutputSlot drmSlots[OBS_DRM_OUTPUT_MAX_OUTPUTS];
int drmSlotCount = 0;

void RenderDrmMultiview(void *param, uint32_t cx, uint32_t cy)
{
	DrmOutputSlot *slot = static_cast<DrmOutputSlot *>(param);
	if (slot->updating || !slot->multiview)
		return;
	slot->multiview->Render(cx, cy);
}

/* The SAME BasicWindow settings OBSProjector::UpdateMultiview applies, so the HDMI grid matches
 * the operator's projector Multiview (layout, scene names, safe areas). */
void UpdateDrmMultiview(DrmOutputSlot &slot)
{
	MultiviewLayout layout =
		static_cast<MultiviewLayout>(config_get_int(App()->GetUserConfig(), "BasicWindow", "MultiviewLayout"));
	bool drawLabel = config_get_bool(App()->GetUserConfig(), "BasicWindow", "MultiviewDrawNames");
	bool drawSafeArea = config_get_bool(App()->GetUserConfig(), "BasicWindow", "MultiviewDrawAreas");

	obs_enter_graphics();
	slot.updating = true;
	obs_leave_graphics();

	slot.multiview->Update(layout, drawLabel, drawSafeArea);

	obs_enter_graphics();
	slot.updating = false;
	obs_leave_graphics();
}

void ClearDrmMultiview(DrmOutputSlot &slot)
{
	if (!slot.multiview)
		return;
	/* Serialised with the graphics thread: after this returns no render is in flight. */
	obs_drm_output_set_view_renderer_of(slot.index, nullptr, nullptr);
	delete slot.multiview;
	slot.multiview = nullptr;
	blog(LOG_INFO, "drm-output: frontend Multiview detached from the HDMI output '%s'",
	     obs_drm_output_connector(slot.index));
}

void UpdateMenu()
{
	for (int i = 0; i < drmSlotCount; i++) {
		DrmOutputSlot &slot = drmSlots[i];
		if (!slot.actionProgram || !slot.actionMultiview)
			continue;
		const bool active = obs_drm_output_active_of(slot.index);
		const bool multiview = obs_drm_output_get_view_of(slot.index) == OBS_DRM_OUTPUT_VIEW_MULTIVIEW;
		slot.actionProgram->setChecked(!multiview);
		slot.actionMultiview->setChecked(multiview);
		slot.actionProgram->setEnabled(active);
		slot.actionMultiview->setEnabled(active);
	}
}

/* Hold ONE built-in Multiview per output exactly while that output is active AND shows the
 * Multiview: a Program view (or no output) costs nothing — no extra Multiview render. */
void SyncDrmMultiview()
{
	for (int i = 0; i < drmSlotCount; i++) {
		DrmOutputSlot &slot = drmSlots[i];
		const bool want = obs_drm_output_active_of(slot.index) &&
				  obs_drm_output_get_view_of(slot.index) == OBS_DRM_OUTPUT_VIEW_MULTIVIEW;
		if (want && !slot.multiview) {
			slot.multiview = new Multiview();
			UpdateDrmMultiview(slot);
			obs_drm_output_set_view_renderer_of(slot.index, RenderDrmMultiview, &slot);
			blog(LOG_INFO, "drm-output: frontend Multiview attached to the HDMI output '%s'",
			     obs_drm_output_connector(slot.index));
		} else if (!want && slot.multiview) {
			ClearDrmMultiview(slot);
		}
	}
	UpdateMenu();
}

void SelectView(int idx, enum obs_drm_output_view view)
{
	obs_drm_output_set_view_of(idx, view);
	SyncDrmMultiview();
}

void ClearAllDrmMultiviews()
{
	for (int i = 0; i < drmSlotCount; i++)
		ClearDrmMultiview(drmSlots[i]);
}

void OnFrontendEvent(enum obs_frontend_event event, void *)
{
	switch (event) {
	case OBS_FRONTEND_EVENT_FINISHED_LOADING:
	case OBS_FRONTEND_EVENT_SCENE_COLLECTION_CHANGED:
		SyncDrmMultiview();
		break;
	case OBS_FRONTEND_EVENT_EXIT:
		ClearAllDrmMultiviews();
		break;
	default:
		break;
	}
}

/* The Tools-menu labels of output `idx`: today's two labels on a one-output box, the connector named
 * when the box drives several. */
QString DrmMenuLabel(int idx, int count, const char *view)
{
	if (count <= 1)
		return QString("HDMI výstup: %1").arg(view);
	return QString("HDMI výstup %1: %2").arg(QString::fromUtf8(obs_drm_output_connector(idx)), view);
}

} // namespace

void DrmOutputViewInit()
{
	/* obs_startup (and with it the autostart that configures the outputs) ran before the main window. A
	 * box with no configured output keeps one disabled pair, as before. */
	const int count = obs_drm_output_count();
	drmSlotCount = count > 0 ? (count < OBS_DRM_OUTPUT_MAX_OUTPUTS ? count : OBS_DRM_OUTPUT_MAX_OUTPUTS) : 1;
	bool menu_ok = true;
	for (int i = 0; i < drmSlotCount; i++) {
		DrmOutputSlot &slot = drmSlots[i];
		slot.index = i;
		const QString program = DrmMenuLabel(i, count, "Program");
		const QString multiview = DrmMenuLabel(i, count, "Multiview");
		slot.actionProgram =
			static_cast<QAction *>(obs_frontend_add_tools_menu_qaction(program.toUtf8().constData()));
		slot.actionMultiview =
			static_cast<QAction *>(obs_frontend_add_tools_menu_qaction(multiview.toUtf8().constData()));
		if (!slot.actionProgram || !slot.actionMultiview) {
			slot.actionProgram = nullptr;
			slot.actionMultiview = nullptr;
			menu_ok = false;
			continue;
		}
		slot.actionProgram->setCheckable(true);
		slot.actionMultiview->setCheckable(true);
		QActionGroup *group = new QActionGroup(slot.actionProgram->parent());
		group->setExclusive(true);
		group->addAction(slot.actionProgram);
		group->addAction(slot.actionMultiview);
		QObject::connect(slot.actionProgram, &QAction::triggered, slot.actionProgram,
				 [i] { SelectView(i, OBS_DRM_OUTPUT_VIEW_PROGRAM); });
		QObject::connect(slot.actionMultiview, &QAction::triggered, slot.actionMultiview,
				 [i] { SelectView(i, OBS_DRM_OUTPUT_VIEW_MULTIVIEW); });
	}
	if (!menu_ok) {
		/* the frontend API returns nullptr before its callbacks exist -- no menu, but the view
		 * config still drives the output(s) and the Multiview still attaches on the load events */
		blog(LOG_WARNING, "drm-output: could not add the HDMI view switch to the Tools menu");
	}

	obs_frontend_add_event_callback(OnFrontendEvent, nullptr);
	UpdateMenu();
}

void DrmOutputViewRefresh()
{
	for (int i = 0; i < drmSlotCount; i++)
		if (drmSlots[i].multiview)
			UpdateDrmMultiview(drmSlots[i]);
}

void DrmOutputViewClear()
{
	ClearAllDrmMultiviews();
}

#endif /* defined(__linux__) */
