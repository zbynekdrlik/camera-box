/*
 * camera-box issue 1424 part 2 (owner 13.10.2026, answer "1"; design comment 6150029770) — the DRM
 * output module drives up to DRM_OUTPUT_MAX_OUTPUTS fixed HDMI outputs at once. strih-pp: HDMI-1 = the
 * Program + its audio for the vestibule (Blackmagic HDMI-to-SDI) AND HDMI-0 = the cutter's Multiview.
 *
 * This TU owns:
 *   - the CONFIG GRAMMAR of ~/.camera-box/drm-output.json: the single-object form (one output, issue
 *     1152 / 1346, byte-unchanged on imag and strih-snv) or {"enabled":true,"outputs":[{connector,
 *     view, backend, argb}, ...]} -- read with jansson directly, so a malformed array is seen as
 *     malformed (obs_data's JSON loader silently drops a non-object array member and would start half
 *     a pair). A malformed array starts NO output and logs ONE named line. The pure grammar is the
 *     marked block below, lift-compiled against real jansson + truth-tabled by
 *     tests/drm_output_outputs_1424.rs over tests/fixtures/drm_output_outputs_parity.tsv, the SAME
 *     table the Python mirror (strih_scenes.drm_output_instances) reads;
 *   - the INSTANCE REGISTRY (struct drm_output_inst, obs-drm-output-internal.h): one per configured
 *     output, statically allocated, each with its own backend state, view, present clock and audio leg;
 *   - the public entry points (start / stop / active / count / the frame hook) and the mailbox seam the
 *     view TU renders through, routed to the instance's backend (lease: obs-drm-output.c, vk-direct:
 *     obs-drm-output-backend.c);
 *   - the persisted view: obs_drm_output_set_view_of() rewrites only the "view" of that output's own
 *     entry (one compact line, every other key kept, an atomic temp-file rename).
 *
 * An instance that fails to start never stops the other one. Linux-only, built only via
 * libobs/cmake/os-linux.cmake.
 */

#if defined(__linux__)

#include <jansson.h>
#include <pthread.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "obs.h"
#include "obs-drm-output.h"
#include "obs-drm-output-internal.h"
#include "obs-drm-output-audio.h"
#include "graphics/vec4.h"
#include "util/platform.h"
#include "util/threading.h"

/* ---- BEGIN pure config grammar (lift-compiled by tests/drm_output_outputs_1424.rs) ---- */
/* The verdict of a drm-output config. One token each (drm_output_cfg_verdict_name), the SAME tokens the
 * Python mirror strih_scenes.drm_output_instances returns. */
#define DRM_OUTPUT_CFG_OK 0
#define DRM_OUTPUT_CFG_UNREADABLE 1  /* not JSON / not a JSON object */
#define DRM_OUTPUT_CFG_DISABLED 2    /* no boolean "enabled": true */
#define DRM_OUTPUT_CFG_NOT_ARRAY 3   /* "outputs" is present but not an array */
#define DRM_OUTPUT_CFG_EMPTY 4       /* "outputs": [] */
#define DRM_OUTPUT_CFG_TOO_MANY 5    /* more entries than the module drives */
#define DRM_OUTPUT_CFG_NOT_OBJECT 6  /* an "outputs" entry is not a JSON object */
#define DRM_OUTPUT_CFG_CONNECTOR 7   /* no "connector" string of 1..63 bytes */
#define DRM_OUTPUT_CFG_DUPLICATE 8   /* a connector named twice */
#define DRM_OUTPUT_CFG_VIEW 9        /* an "outputs" entry with an unknown "view" */
#define DRM_OUTPUT_CFG_BACKEND 10    /* an unknown "backend" */
#define DRM_OUTPUT_CFG_AUDIO 11      /* an "outputs" entry whose "audio" is present but not a boolean */
#define DRM_OUTPUT_CFG_CONNECTOR_MAX 64
/* The M1 dark-grey solid (0x202020): the image before the first rendered frame + the fail-open one. */
#define DRM_OUTPUT_CFG_DEFAULT_ARGB 0x00202020u

/* One configured output, as the grammar read it. view: 0 = program, 1 = multiview; backend: 0 = lease,
 * 1 = vk-direct (the drm_output_view_from_config / drm_output_backend_from_config values). */
struct drm_output_spec {
	char connector[DRM_OUTPUT_CFG_CONNECTOR_MAX];
	int view;
	bool view_fallback; /* the single-object form read an unknown "view" as the Program (WARN) */
	int backend;
	uint32_t argb;
	int config_slot; /* -1 = the single-object form, else the "outputs" array position */
	bool audio;      /* issue 1424 part 3: false = the connector's PCM is owned elsewhere, no audio leg */
};

static const char *drm_output_cfg_verdict_name(int verdict)
{
	switch (verdict) {
	case DRM_OUTPUT_CFG_OK:
		return "ok";
	case DRM_OUTPUT_CFG_UNREADABLE:
		return "unreadable";
	case DRM_OUTPUT_CFG_DISABLED:
		return "disabled";
	case DRM_OUTPUT_CFG_NOT_ARRAY:
		return "outputs-not-array";
	case DRM_OUTPUT_CFG_EMPTY:
		return "outputs-empty";
	case DRM_OUTPUT_CFG_TOO_MANY:
		return "outputs-too-many";
	case DRM_OUTPUT_CFG_NOT_OBJECT:
		return "entry-not-object";
	case DRM_OUTPUT_CFG_CONNECTOR:
		return "connector-invalid";
	case DRM_OUTPUT_CFG_DUPLICATE:
		return "connector-duplicate";
	case DRM_OUTPUT_CFG_VIEW:
		return "view-unknown";
	case DRM_OUTPUT_CFG_BACKEND:
		return "backend-unknown";
	case DRM_OUTPUT_CFG_AUDIO:
		return "audio-invalid";
	default:
		return "unknown";
	}
}

/* A JSON value read the way obs_data_get_string reads an item: the string, else "" (a non-string). */
static const char *drm_output_cfg_str(const json_t *v)
{
	return json_is_string(v) ? json_string_value(v) : "";
}

/* "argb": absent -> the default dark grey; an integer -> its low 32 bits; a real -> truncated; any other
 * present value -> 0 (black), the obs_data_get_int reading of a non-number. */
static uint32_t drm_output_cfg_argb(const json_t *obj)
{
	const json_t *v = json_object_get(obj, "argb");
	if (!v)
		return DRM_OUTPUT_CFG_DEFAULT_ARGB;
	if (json_is_integer(v))
		return (uint32_t)json_integer_value(v);
	if (json_is_real(v))
		return (uint32_t)(long long)json_real_value(v);
	return 0u;
}

/* The "connector" of `obj` into `out`: a JSON string of 1..63 bytes, else false. */
static bool drm_output_cfg_connector(const json_t *obj, char *out)
{
	const json_t *v = json_object_get(obj, "connector");
	if (!json_is_string(v))
		return false;
	const char *s = json_string_value(v);
	const size_t len = strlen(s);
	if (len == 0 || len >= DRM_OUTPUT_CFG_CONNECTOR_MAX)
		return false;
	memcpy(out, s, len + 1);
	return true;
}

/* Classify a parsed config `root` into at most `cap` output specs. Returns a DRM_OUTPUT_CFG_* verdict;
 * on DRM_OUTPUT_CFG_OK specs[0..*n) are filled. *bad = the offending "outputs" position (0 otherwise).
 * *program = the optional "program" key (absent = true; false only for a boolean false or any
 * non-true value present -- the obs_data_get_bool reading).
 *   single-object form (no "outputs" key): exactly the issue-1152/1346 grammar -- a disabled config,
 *     then a missing connector, then an unknown backend keep it dormant; an unknown view reads as the
 *     Program (view_fallback, WARNed by the caller);
 *   "outputs" form: all or nothing -- not an array, empty, more than `cap`, then per entry in order a
 *     non-object, an invalid connector, an unknown view, an unknown backend, a non-boolean "audio", a
 *     duplicate connector. "audio" (issue 1424 part 3): absent / true = the output's own HDMI audio leg,
 *     false = its PCM is owned elsewhere (the intercom hub); the single-object form always has audio. */
static int drm_output_config_classify(const json_t *root, struct drm_output_spec *specs, size_t cap, size_t *n,
				      size_t *bad, bool *program)
{
	*n = 0;
	*bad = 0;
	*program = true;
	if (!root || !json_is_object(root))
		return DRM_OUTPUT_CFG_UNREADABLE;
	const json_t *prog = json_object_get(root, "program");
	if (prog)
		*program = json_is_true(prog);
	if (!json_is_true(json_object_get(root, "enabled")))
		return DRM_OUTPUT_CFG_DISABLED;

	const json_t *outputs = json_object_get(root, "outputs");
	if (!outputs) {
		struct drm_output_spec *sp = &specs[0];
		memset(sp, 0, sizeof(*sp));
		if (cap < 1 || !drm_output_cfg_connector(root, sp->connector))
			return DRM_OUTPUT_CFG_CONNECTOR;
		sp->view = drm_output_view_from_config(drm_output_cfg_str(json_object_get(root, "view")));
		if (sp->view < 0) {
			sp->view = 0;
			sp->view_fallback = true;
		}
		sp->backend = drm_output_backend_from_config(drm_output_cfg_str(json_object_get(root, "backend")));
		if (sp->backend < 0)
			return DRM_OUTPUT_CFG_BACKEND;
		sp->argb = drm_output_cfg_argb(root);
		sp->config_slot = -1;
		sp->audio = true;
		*n = 1;
		return DRM_OUTPUT_CFG_OK;
	}

	if (!json_is_array(outputs))
		return DRM_OUTPUT_CFG_NOT_ARRAY;
	const size_t count = json_array_size(outputs);
	if (count == 0)
		return DRM_OUTPUT_CFG_EMPTY;
	if (count > cap) {
		*bad = cap;
		return DRM_OUTPUT_CFG_TOO_MANY;
	}
	for (size_t i = 0; i < count; i++) {
		const json_t *e = json_array_get(outputs, i);
		struct drm_output_spec *sp = &specs[i];
		memset(sp, 0, sizeof(*sp));
		*bad = i;
		if (!json_is_object(e))
			return DRM_OUTPUT_CFG_NOT_OBJECT;
		if (!drm_output_cfg_connector(e, sp->connector))
			return DRM_OUTPUT_CFG_CONNECTOR;
		sp->view = drm_output_view_from_config(drm_output_cfg_str(json_object_get(e, "view")));
		if (sp->view < 0)
			return DRM_OUTPUT_CFG_VIEW;
		sp->backend = drm_output_backend_from_config(drm_output_cfg_str(json_object_get(e, "backend")));
		if (sp->backend < 0)
			return DRM_OUTPUT_CFG_BACKEND;
		const json_t *audio = json_object_get(e, "audio");
		if (audio && !json_is_boolean(audio))
			return DRM_OUTPUT_CFG_AUDIO;
		sp->audio = !audio || json_is_true(audio);
		for (size_t j = 0; j < i; j++) {
			if (strcmp(specs[j].connector, sp->connector) == 0)
				return DRM_OUTPUT_CFG_DUPLICATE;
		}
		sp->argb = drm_output_cfg_argb(e);
		sp->config_slot = (int)i;
	}
	*bad = 0;
	*n = count;
	return DRM_OUTPUT_CFG_OK;
}

/* Set the "view" of the output on `connector` in a parsed config: the single-object form's top-level
 * "view" (its "connector" must be `connector`), else the "outputs" entry whose "connector" is
 * `connector`. Every other key is kept. false = the config no longer names that output. */
static bool drm_output_config_set_view(json_t *root, const char *connector, const char *view)
{
	if (!json_is_object(root))
		return false;
	json_t *outputs = json_object_get(root, "outputs");
	json_t *target = NULL;
	if (!outputs) {
		if (strcmp(drm_output_cfg_str(json_object_get(root, "connector")), connector) == 0)
			target = root;
	} else if (json_is_array(outputs)) {
		for (size_t i = 0; i < json_array_size(outputs) && !target; i++) {
			json_t *e = json_array_get(outputs, i);
			if (json_is_object(e) && strcmp(drm_output_cfg_str(json_object_get(e, "connector")), connector) == 0)
				target = e;
		}
	}
	if (!target)
		return false;
	return json_object_set_new(target, "view", json_string(view)) == 0;
}
/* ---- END pure config grammar ---- */

/* The config as jansson reads it, after OBS's own UTF-8 reader (which drops a byte-order mark, as the
 * issue-1152/1346 obs_data loader did): a config with a BOM keeps starting. NULL = absent/unparseable. */
static json_t *drm_output_config_load(const char *path)
{
	char *text = os_quick_read_utf8_file(path);
	if (!text)
		return NULL;
	json_error_t err;
	json_t *root = json_loads(text, 0, &err);
	bfree(text);
	return root;
}

_Static_assert(DRM_OUTPUT_CFG_CONNECTOR_MAX == DRM_OUTPUT_CONNECTOR_MAX, "one connector length bound");
_Static_assert(OBS_DRM_OUTPUT_VIEW_PROGRAM == 0 && OBS_DRM_OUTPUT_VIEW_MULTIVIEW == 1, "the spec's view values");
_Static_assert(OBS_DRM_OUTPUT_BACKEND_LEASE == 0 && OBS_DRM_OUTPUT_BACKEND_VK_DIRECT == 1, "the spec's backends");

/* -------------------------------------------------------------------------------------------------
 * The instance registry. g_outs[0 .. g_outs_count) are configured; an instance is fully written before
 * the count that publishes it (os_atomic, seq-cst) and never removed or freed.
 * ------------------------------------------------------------------------------------------------- */
static struct drm_output_inst g_outs[DRM_OUTPUT_MAX_OUTPUTS];
static volatile long g_outs_count;
static pthread_mutex_t g_outs_lock = PTHREAD_MUTEX_INITIALIZER; /* add / start / stop transitions */
static pthread_mutex_t g_persist_lock = PTHREAD_MUTEX_INITIALIZER;
static char g_config_path[1024]; /* the config the autostart read (g_persist_lock) */

void drm_output_registry_lock(void)
{
	pthread_mutex_lock(&g_outs_lock);
}

void drm_output_registry_unlock(void)
{
	pthread_mutex_unlock(&g_outs_lock);
}

int drm_output_inst_count(void)
{
	return (int)os_atomic_load_long(&g_outs_count);
}

struct drm_output_inst *drm_output_inst_at(int idx)
{
	if (idx < 0 || idx >= drm_output_inst_count())
		return NULL;
	return &g_outs[idx];
}

/* Add an instance for `sp` (caller holds g_outs_lock). NULL when every slot is taken. */
static struct drm_output_inst *drm_output_inst_add_locked(const struct drm_output_spec *sp, bool program)
{
	const int idx = drm_output_inst_count();
	if (idx >= DRM_OUTPUT_MAX_OUTPUTS)
		return NULL;
	struct drm_output_inst *inst = &g_outs[idx];
	memset(inst, 0, sizeof(*inst));
	inst->index = idx;
	inst->config_slot = sp->config_slot;
	snprintf(inst->connector, sizeof(inst->connector), "%s", sp->connector);
	inst->backend = sp->backend == OBS_DRM_OUTPUT_BACKEND_VK_DIRECT ? OBS_DRM_OUTPUT_BACKEND_VK_DIRECT
									 : OBS_DRM_OUTPUT_BACKEND_LEASE;
	inst->solid_argb = sp->argb;
	inst->program = program;
	inst->audio_enabled = sp->audio;
	inst->ops = inst->backend == OBS_DRM_OUTPUT_BACKEND_VK_DIRECT ? &drm_output_vk_direct_backend
								      : &drm_output_lease_backend;
	drm_output_view_configure(inst, sp->view);
	os_atomic_set_long(&g_outs_count, idx + 1);
	return inst;
}

static struct drm_output_inst *drm_output_inst_by_connector(const char *connector)
{
	for (int i = 0; i < drm_output_inst_count(); i++)
		if (strcmp(g_outs[i].connector, connector) == 0)
			return &g_outs[i];
	return NULL;
}

/* Start one instance: its EDID first (read while X still lists the connector), then the backend, then its
 * audio leg (after the mode is driven: the connector's ELD appears only then). An output whose config turns
 * "audio" off (issue 1424 part 3: its PCM is owned elsewhere) reads no EDID, resolves and opens nothing.
 * Caller holds g_outs_lock. */
bool drm_output_inst_start_locked(struct drm_output_inst *inst)
{
	uint8_t edid[512];
	size_t edid_len = 0;
	if (inst->audio_enabled)
		edid_len = drm_output_audio_read_edid(inst->connector, edid, sizeof(edid));
	if (!inst->ops->start(inst)) {
		inst->started = false;
		return false;
	}
	inst->started = true;
	if (!inst->audio_enabled)
		blog(LOG_INFO, "drm-output audio: '%s' off by config (owned elsewhere)", inst->connector);
	else if (!inst->audio) /* camera-box issue 1424: THIS connector's own HDMI audio leg */
		inst->audio = drm_output_audio_open(inst->connector, edid, edid_len, &inst->clock, &inst->view.view);
	return true;
}

/* Stop one instance: its audio leg first (the mix callback disconnected, the PCM closed), then its
 * scanout. Caller holds g_outs_lock. */
void drm_output_inst_stop_locked(struct drm_output_inst *inst)
{
	drm_output_audio_close(inst->audio);
	inst->audio = NULL;
	inst->ops->stop(inst);
	inst->started = false;
}

bool obs_drm_output_start(const struct obs_drm_output_config *cfg)
{
	if (!cfg || !cfg->connector_name || !cfg->connector_name[0] ||
	    strlen(cfg->connector_name) >= DRM_OUTPUT_CONNECTOR_MAX) {
		blog(LOG_WARNING, "drm-output: start called with no (or an over-long) connector — ignored");
		return false;
	}
	pthread_mutex_lock(&g_outs_lock);
	struct drm_output_inst *inst = drm_output_inst_by_connector(cfg->connector_name);
	if (!inst) {
		struct drm_output_spec sp;
		memset(&sp, 0, sizeof(sp));
		snprintf(sp.connector, sizeof(sp.connector), "%s", cfg->connector_name);
		sp.view = OBS_DRM_OUTPUT_VIEW_PROGRAM;
		sp.backend = cfg->backend;
		sp.argb = cfg->solid_argb;
		sp.config_slot = -1;
		sp.audio = true;
		inst = drm_output_inst_add_locked(&sp, cfg->program);
	}
	bool ok = false;
	if (!inst)
		blog(LOG_WARNING, "drm-output: start of '%s' refused — all %d output slots are taken",
		     cfg->connector_name, DRM_OUTPUT_MAX_OUTPUTS);
	else
		ok = drm_output_inst_start_locked(inst);
	pthread_mutex_unlock(&g_outs_lock);
	return ok;
}

void obs_drm_output_stop(void)
{
	/* issue 1424: the retry supervisor first -- it takes g_outs_lock itself, and no attempt may start
	 * an output this stop has already stopped */
	drm_output_retry_stop();
	pthread_mutex_lock(&g_outs_lock);
	for (int i = 0; i < drm_output_inst_count(); i++)
		drm_output_inst_stop_locked(&g_outs[i]);
	pthread_mutex_unlock(&g_outs_lock);
}

bool obs_drm_output_active_of(int idx)
{
	struct drm_output_inst *inst = drm_output_inst_at(idx);
	return inst && inst->ops->active(inst);
}

bool obs_drm_output_active(void)
{
	for (int i = 0; i < drm_output_inst_count(); i++)
		if (obs_drm_output_active_of(i))
			return true;
	return false;
}

int obs_drm_output_count(void)
{
	return drm_output_inst_count();
}

const char *obs_drm_output_connector(int idx)
{
	struct drm_output_inst *inst = drm_output_inst_at(idx);
	return inst ? inst->connector : "";
}

void obs_drm_output_maybe_autostart(void)
{
	const char *home = getenv("HOME");
	char path[1024];
	if (!home || home[0] == '\0') {
		blog(LOG_INFO, "drm-output: autostart disabled (no HOME) — module present, dormant");
		return;
	}
	int n = snprintf(path, sizeof(path), "%s/.camera-box/drm-output.json", home);
	if (n < 0 || (size_t)n >= sizeof(path)) {
		blog(LOG_INFO, "drm-output: autostart disabled (config path too long) — dormant");
		return;
	}

	if (access(path, R_OK) != 0) {
		blog(LOG_INFO, "drm-output: autostart disabled (no config at %s) — module present, dormant",
		     path);
		return;
	}
	/* The path the live view switch persists into, recorded even for a disabled config. */
	pthread_mutex_lock(&g_persist_lock);
	snprintf(g_config_path, sizeof(g_config_path), "%s", path);
	pthread_mutex_unlock(&g_persist_lock);

	json_t *root = drm_output_config_load(path);
	struct drm_output_spec specs[DRM_OUTPUT_MAX_OUTPUTS];
	size_t count = 0, bad = 0;
	bool program = true;
	const int verdict = drm_output_config_classify(root, specs, DRM_OUTPUT_MAX_OUTPUTS, &count, &bad, &program);
	const bool array_form = json_is_object(root) && json_object_get(root, "outputs") != NULL;
	const bool single_audio_key = !array_form && json_is_object(root) && json_object_get(root, "audio") != NULL;
	json_decref(root);

	switch (verdict) {
	case DRM_OUTPUT_CFG_OK:
		break;
	case DRM_OUTPUT_CFG_UNREADABLE:
		blog(LOG_WARNING, "drm-output: autostart config %s present but unparseable — dormant", path);
		return;
	case DRM_OUTPUT_CFG_DISABLED:
		blog(LOG_INFO, "drm-output: autostart disabled ({\"enabled\":false} in %s) — dormant", path);
		return;
	case DRM_OUTPUT_CFG_CONNECTOR:
		if (!array_form) {
			blog(LOG_WARNING, "drm-output: autostart config %s has no \"connector\" — dormant", path);
			return;
		}
		/* fall through: an "outputs" entry -- named below */
	default:
		if (array_form) {
			blog(LOG_WARNING,
			     "drm-output: autostart config %s \"outputs\" is malformed (%s at entry %zu) — NO output "
			     "started (all or none)",
			     path, drm_output_cfg_verdict_name(verdict), bad);
		} else {
			blog(LOG_WARNING,
			     "drm-output: autostart config %s has an unknown \"backend\" (want lease or vk-direct) — "
			     "dormant",
			     path);
		}
		return;
	}

	if (array_form)
		blog(LOG_INFO, "drm-output: autostart ENABLED from %s — %zu outputs", path, count);
	if (single_audio_key) /* issue 1424 review: a hand edit there must not look like it worked */
		blog(LOG_WARNING,
		     "drm-output: \"audio\" in %s is ignored -- the single-object form always has its audio leg; use the \"outputs\" form",
		     path);
	pthread_mutex_lock(&g_outs_lock);
	struct drm_output_inst *insts[DRM_OUTPUT_MAX_OUTPUTS] = {NULL};
	for (size_t i = 0; i < count; i++) {
		if (specs[i].view_fallback)
			blog(LOG_WARNING,
			     "drm-output: unknown \"view\" in %s -- using program (want program or multiview)", path);
		if (!array_form)
			blog(LOG_INFO, "drm-output: autostart ENABLED from %s — connector='%s'", path,
			     specs[i].connector);
		if (specs[i].backend == OBS_DRM_OUTPUT_BACKEND_VK_DIRECT)
			blog(LOG_INFO, "drm-output: backend=vk-direct for '%s' (from %s) — NVIDIA Vulkan direct display",
			     specs[i].connector, path);
		insts[i] = drm_output_inst_add_locked(&specs[i], program);
	}
	/* Every configured output starts on its own: one that fails never stops the other. */
	for (size_t i = 0; i < count; i++) {
		if (insts[i] && !drm_output_inst_start_locked(insts[i]))
			blog(LOG_WARNING, "drm-output: output '%s' did not start — %s", insts[i]->connector,
			     count > 1 ? "the other output keeps running" : "dormant");
	}
	pthread_mutex_unlock(&g_outs_lock);
	/* issue 1424 (design 6386467303 layer 3): a vk-direct output that did not start (its monitor off,
	 * the acquire refused) or that loses its scanout later is retried, never left dark until the next
	 * OBS start */
	drm_output_retry_start();
}

/* The graphics-thread frame hook: each instance's backend renders its own view (obs-video.c). */
void obs_drm_output_on_frame(void)
{
	const int count = drm_output_inst_count();
	for (int i = 0; i < count; i++)
		g_outs[i].ops->on_frame(&g_outs[i]);
}

/* -------------------------------------------------------------------------------------------------
 * The ONE raw blit both views and both backends use (issue 1152 M2 / 1346; moved here from
 * obs-drm-output.c with the per-instance split, issue 1424 part 2). The pure aspect-fit helper is
 * lift-compiled + truth-tabled by tests/drm_output_program_1152.rs.
 * ------------------------------------------------------------------------------------------------- */
/* Aspect-fit `src` into `dst` (centred letterbox/pillarbox, integer maths — the rig case is a
 * 1:1 1920x1080 pass-through). Any zero input fails open to the full destination rect. */
static void drm_output_fit_rect(uint32_t src_w, uint32_t src_h, uint32_t dst_w, uint32_t dst_h, uint32_t *out_x,
				uint32_t *out_y, uint32_t *out_w, uint32_t *out_h)
{
	if (src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0) {
		*out_x = 0;
		*out_y = 0;
		*out_w = dst_w;
		*out_h = dst_h;
		return;
	}
	uint32_t w, h;
	if ((uint64_t)src_h * dst_w <= (uint64_t)src_w * dst_h) {
		w = dst_w;
		h = (uint32_t)((uint64_t)src_h * dst_w / src_w);
	} else {
		h = dst_h;
		w = (uint32_t)((uint64_t)src_w * dst_h / src_h);
	}
	*out_x = (dst_w - w) / 2u;
	*out_y = (dst_h - h) / 2u;
	*out_w = w;
	*out_h = h;
}

/* Raw SDR copy of `src` into scanout buffer `idx` (graphics thread, context held; the caller
 * claimed `idx`). Non-sRGB sampling + framebuffer sRGB encode OFF + blending OFF preserves the
 * texture BYTES exactly — the Program canvas and (issue 1346) the Multiview texrender are both
 * sRGB-ENCODED render targets, and the dma-buf scanout buffer is linear storage, so a byte copy
 * is the faithful transfer (the same values a monitor on the X desktop shows). Aspect-fit
 * letterboxes a size mismatch. Ends with gs_flush(): the kernel page-flip then waits on the BO's
 * implicit fence (i915/Xe dma-resv), so scanout never observes a half-rendered buffer — no
 * glFinish stall. Known limitation: SDR only (HDR would need a tonemap pass). Returns true iff
 * the buffer was rendered (the caller publishes only then). */
bool drm_output_blit_raw(struct drm_output_inst *inst, gs_texture_t *src, int idx)
{
	gs_effect_t *effect = obs_get_base_effect(OBS_EFFECT_DEFAULT);
	gs_texture_t *dst = drm_output_render_buf_texture(inst, idx);
	if (!src || !effect || !dst)
		return false;

	uint32_t src_w = gs_texture_get_width(src);
	uint32_t src_h = gs_texture_get_height(src);
	uint32_t dst_w, dst_h; /* camera-box issue 1346: the owning backend's mode (lease or vk-direct) */
	drm_output_mode_size(inst, &dst_w, &dst_h);
	uint32_t fx, fy, fw, fh;
	drm_output_fit_rect(src_w, src_h, dst_w, dst_h, &fx, &fy, &fw, &fh);

	gs_viewport_push();
	gs_projection_push();
	gs_matrix_push();
	gs_matrix_identity();

	gs_set_render_target(dst, NULL);
	struct vec4 black;
	vec4_zero(&black);
	gs_clear(GS_CLEAR_COLOR, &black, 0.0f, 0);
	gs_set_viewport((int)fx, (int)fy, (int)fw, (int)fh);
	gs_ortho(0.0f, (float)src_w, 0.0f, (float)src_h, -100.0f, 100.0f);

	gs_enable_depth_test(false);
	gs_set_cull_mode(GS_NEITHER);
	const bool prev_srgb = gs_framebuffer_srgb_enabled();
	gs_enable_framebuffer_srgb(false);
	gs_enable_blending(false);

	gs_eparam_t *param = gs_effect_get_param_by_name(effect, "image");
	gs_effect_set_texture(param, src);
	while (gs_effect_loop(effect, "Draw"))
		gs_draw_sprite(src, 0, 0, 0);

	gs_enable_blending(true);
	gs_enable_framebuffer_srgb(prev_srgb);
	gs_set_render_target(NULL, NULL);

	gs_matrix_pop();
	gs_projection_pop();
	gs_viewport_pop();

	gs_flush();
	return true;
}

/* ---- the mailbox seam (graphics thread, context held), routed to the instance's backend ---- */

int drm_output_claim_render_buf(struct drm_output_inst *inst)
{
	return inst->ops->claim(inst);
}

void drm_output_publish_render_buf(struct drm_output_inst *inst, int idx)
{
	drm_output_present_note_published(&inst->clock, idx); /* camera-box issue 1424: this frame's render tick */
	inst->ops->publish(inst, idx);
}

gs_texture_t *drm_output_render_buf_texture(struct drm_output_inst *inst, int idx)
{
	return inst->ops->texture(inst, idx);
}

void drm_output_mode_size(struct drm_output_inst *inst, uint32_t *w, uint32_t *h)
{
	inst->ops->mode_size(inst, w, h);
}

/* ---- the persisted view ---- */

const char *drm_output_config_persist_view(const struct drm_output_inst *inst, const char *view)
{
	const char *reason = NULL;
	pthread_mutex_lock(&g_persist_lock);
	if (g_config_path[0] == '\0') {
		reason = "no drm-output config path (the autostart found no config)";
	} else {
		json_t *root = drm_output_config_load(g_config_path);
		if (!root || !json_is_object(root)) {
			reason = "config unreadable (missing or not a JSON object)";
		} else if (!drm_output_config_set_view(root, inst->connector, view)) {
			reason = "the config no longer names this output";
		} else {
			char *json = json_dumps(root, JSON_COMPACT | JSON_PRESERVE_ORDER);
			if (!json) {
				reason = "config unreadable (JSON serialisation failed)";
			} else {
				const size_t len = strlen(json);
				char *line = malloc(len + 2);
				if (!line) {
					reason = "out of memory";
				} else {
					memcpy(line, json, len);
					line[len] = '\n';
					line[len + 1] = '\0';
					reason = os_quick_write_utf8_file_safe(g_config_path, line, len + 1, false, "tmp",
									       NULL)
							 ? NULL
							 : "write failed (temp file or rename)";
					free(line);
				}
				free(json);
			}
		}
		json_decref(root);
	}
	pthread_mutex_unlock(&g_persist_lock);
	return reason;
}

#endif /* defined(__linux__) */
