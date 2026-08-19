#pragma once

#include <string>

#include <obs.h>
#include <util/base.h>

/* camera-box #1106: NULL-safe wrapper for obs_data_get_json(). obs_data_get_json() returns NULL when
 * json_dumps() fails (libobs/obs-data.c -- an intermittent jansson serialisation/alloc failure on a
 * perfectly valid obs_data_t) or when handed a NULL obs_data_t. Constructing a std::string directly
 * from that NULL is undefined behaviour -- the c0000005 NULL-deref class fixed for
 * OBSBasicProperties::CheckSettings in #773. Every frontend consumer that builds a std::string from
 * the result routes through this helper instead: it coalesces NULL to "" and logs the anomaly once
 * (so a json_dumps failure leaves a trace in the OBS log), degrading the undo/redo (or
 * clipboard/drop/transform) action to an empty JSON payload rather than crashing. Kept header-only +
 * pure so tests/frontend_obs_data_json_null_guard_1106.rs can lift and compile it under -Werror. */
static inline std::string OBSDataGetJsonSafe(obs_data_t *data, const char *context)
{
	const char *json = obs_data_get_json(data);
	if (!json)
		blog(LOG_WARNING,
		     "camera-box #1106: obs_data_get_json() returned NULL at %s -- json_dumps failure or "
		     "NULL settings; using empty JSON to avoid a std::string(NULL) crash",
		     context ? context : "(unknown)");
	return std::string(json ? json : "");
}
