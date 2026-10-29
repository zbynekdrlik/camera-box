/* camera-box issue 1387 -- the JPEG XS status-bar indicator (see GenlockJxsWidget.hpp). This file
 * only reads the table and shows the report; the grading is GenlockJxsReport.cpp. */
#include "GenlockJxsWidget.hpp"
#include "GenlockJxsReport.hpp"

#include <obs.h>

#include <QString>
#include <QTimer>

#include <cstring>
#include <vector>

namespace {

enum class GenlockJxsRead { None, Rows, Unreadable };

/* The rows of DistroAV's JPEG XS state table, read through its proc handler. None = there is no
 * such procedure (a build without the JPEG XS source): the indicator then reads OFF, never an error.
 * Unreadable = the procedure answered but the JSON did not parse (obs_data_create_from_json then
 * hands back an EMPTY object, never NULL): that must not read as OFF. The JSON is the plugin's own
 * copy (calldata owns it until calldata_free).
 * obs_get_proc_handler() dereferences the libobs core without a check: safe here because the status
 * bar exists only between OBSApp's StartupOBS and obs_shutdown (which runs with no event loop, so no
 * tick follows it). Keep that order. */
GenlockJxsRead genlock_jxs_read_rows(std::vector<GenlockJxsRow> &rows)
{
	calldata_t cd;
	calldata_init(&cd);
	const bool have = proc_handler_call(obs_get_proc_handler(), "camera_box_jxs_state", &cd);
	const char *json = have ? calldata_string(&cd, "json") : nullptr;
	obs_data_t *table = json && *json ? obs_data_create_from_json(json) : nullptr;
	calldata_free(&cd);
	if (!table)
		return have ? GenlockJxsRead::Unreadable : GenlockJxsRead::None;
	/* the schema version marks a table that parsed (a failed parse is an empty object) */
	if (!obs_data_has_user_value(table, "v")) {
		obs_data_release(table);
		return GenlockJxsRead::Unreadable;
	}
	obs_data_array_t *entries = obs_data_get_array(table, "entries");
	const size_t n = entries ? obs_data_array_count(entries) : 0;
	for (size_t i = 0; i < n; i++) {
		obs_data_t *e = obs_data_array_item(entries, i);
		if (!e)
			continue;
		GenlockJxsRow r;
		const char *kind = obs_data_get_string(e, "kind");
		r.kind = kind && strcmp(kind, "output") == 0 ? 1 : 0;
		const char *name = obs_data_get_string(e, "name");
		const char *detail = obs_data_get_string(e, "detail");
		r.name = name ? name : "";
		r.detail = detail ? detail : "";
		r.age_ms = (int64_t)obs_data_get_int(e, "age_ms");
		r.window = obs_data_get_bool(e, "window");
		r.fps_x100 = (uint32_t)obs_data_get_int(e, "fps_x100");
		r.bpp_x100 = (uint32_t)obs_data_get_int(e, "bpp_x100");
		r.lost = (uint64_t)obs_data_get_int(e, "lost");
		r.undecodable = (uint64_t)obs_data_get_int(e, "undecodable");
		r.dropped = (uint64_t)obs_data_get_int(e, "dropped");
		r.errors = (uint64_t)obs_data_get_int(e, "errors");
		r.report_age_ms = (int64_t)obs_data_get_int(e, "report_age_ms");
		rows.push_back(r);
		obs_data_release(e);
	}
	obs_data_array_release(entries);
	obs_data_release(table);
	return GenlockJxsRead::Rows;
}

} // namespace

GenlockJxsWidget::GenlockJxsWidget(QWidget *parent) : QLabel(parent)
{
	setTextFormat(Qt::PlainText);
	setText("JPEG XS: …");
	Tick();
	timer = new QTimer(this);
	connect(timer, &QTimer::timeout, this, [this]() { Tick(); });
	timer->start(1000);
}

void GenlockJxsWidget::Tick()
{
	std::vector<GenlockJxsRow> rows;
	const GenlockJxsRead read = genlock_jxs_read_rows(rows);
	const GenlockJxsReport rep = genlock_jxs_report(rows, read == GenlockJxsRead::Unreadable);

	setText(QString::fromStdString(rep.label));
	setStyleSheet(QString("QLabel { color: %1; padding: 0 6px; }").arg(QString::fromStdString(rep.colour)));
	setToolTip(QString::fromStdString(rep.tooltip));

	/* one jxs-state: line per change of state, reason or the row behind it */
	if (genlock_jxs_log_due(logKey, rep))
		blog(LOG_INFO, "%s", rep.log_line.c_str());
}
