#pragma once

/*
 * camera-box issue 1346 — the frontend half of the selectable DRM-lease HDMI output view (Linux).
 *
 * libobs (obs-drm-output-view.c) scans out either the Program or a renderer the frontend
 * registers. This component owns ONE built-in Multiview (the stock class every Multiview
 * projector uses — labels, PVW/PGM tally, the issue-1242 twin cells) while the output is active
 * AND the view is MULTIVIEW, registers it as that renderer, and adds the operator's in-OBS
 * switch (Tools menu: HDMI výstup Program / Multiview), which switches live and persists.
 *
 * Built only on Linux (frontend/cmake/os-linux.cmake); every call site is __linux__-guarded, so
 * Windows stays byte-identical.
 */

/* OBSBasic::OBSInit, before the scene collection loads: the Tools menu pair + the event hook. */
void DrmOutputViewInit();

/* OBSProjector::UpdateMultiviewProjectors: re-read the Multiview layout/scenes, exactly like the
 * projectors (no-op while the HDMI view is Program). */
void DrmOutputViewRefresh();

/* OBSBasic::ClearSceneData, next to ClearProjectors(): drop the Multiview before the sources go. */
void DrmOutputViewClear();
