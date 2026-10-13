#!/usr/bin/env python3
"""drm_output_config.py -- the in-OBS HDMI output's config grammar and its writers (issues 1346, 1424).

The vendored libobs DRM output module (vendor/obs-studio/libobs/obs-drm-output*.c,
.claude/rules/obs-drm-output.md) reads ONE file of the OBS user, ~/.camera-box/drm-output.json. This
module is its Python twin, split out of strih_scenes.py (issue 1424 part 2, which kept that file under
its line budget): the pure readers (the single-object grammar of issues 1152/1346 and the "outputs"
array of up to two outputs, all or nothing -- ONE table with the C,
tests/fixtures/drm_output_outputs_parity.tsv) and the writers setup-strih step 6 and
`strih_scenes.py --projector` use. No WebSocket and no OBS dependency.

strih_scenes.py re-exports every name, so `strih_scenes.drm_output_*` keeps working. setup-strih.sh
step 6 installs this file next to strih_scenes.py in /usr/local/bin (strih_scenes imports it at
module load, so the launcher's `import strih_scenes` preflight covers it too).
"""

import json
import math
import os

# --- issue 1346: the HDMI output = the in-OBS DRM-lease output, view Program / Multiview ------------
# Owner ROZHODNUTE (24.9.2026, supersedes the 19.9. OBS fullscreen projector): the strih-snv HDMI
# output is the SAME fixed hardware output imag has -- the vendored libobs DRM-lease output (issue
# 1152, .claude/rules/obs-drm-output.md) -- selectable between the Program and the BUILT-IN Multiview.
# It is never a projector window and never the desktop. The module's activation contract is ONE
# file of the OBS user: ~/.camera-box/drm-output.json ({"enabled":true,"connector":"HDMI-0",
# "argb":2105376,"view":"multiview"}, provisioned by setup-strih.sh step 6 only when an HDMI monitor
# is plugged in). The operator's switch lives in OBS (Tools > HDMI vystup: Program / Multiview),
# which switches live and writes "view" back; `--projector program|multiview` is the scripted twin
# (effective at the next OBS start).
DRM_OUTPUT_CONF = "~/.camera-box/drm-output.json"
DRM_OUTPUT_VIEWS = ("program", "multiview")
# issue 1346: how the output leaves the X desktop -- the X RandR lease (the default = the absent key) or
# the NVIDIA Vulkan direct display. Mirrors obs-drm-output-backend.c drm_output_parse_backend.
DRM_OUTPUT_BACKENDS = ("lease", "vk-direct")
# issue 1424 part 2 (owner 13.10.2026): the module drives up to this many fixed outputs at once, from an
# "outputs" array in the same config (obs-drm-output-outputs.c); the M1 dark grey is each one's default
# solid; a connector name is 1..63 bytes (the C's bound).
DRM_OUTPUT_MAX_OUTPUTS = 2
DRM_OUTPUT_DEFAULT_ARGB = 2105376
DRM_OUTPUT_CONNECTOR_MAX = 64


# --- issue 1424 part 2: the C reads the file with jansson, which refuses what Python's json accepts --

_JSON_INT_MIN = -(1 << 63)
_JSON_INT_MAX = (1 << 63) - 1


def _drm_json_int(text):
    """jansson keeps an integer in a long long and refuses one outside it ("too big integer")."""
    value = int(text)
    if not _JSON_INT_MIN <= value <= _JSON_INT_MAX:
        raise ValueError("integer %s out of the 64-bit range jansson reads" % text)
    return value


def _drm_json_float(text):
    """jansson refuses a real that overflows a double ("real number overflow")."""
    value = float(text)
    if not math.isfinite(value):
        raise ValueError("real %s overflows a double" % text)
    return value


def _drm_json_constant(name):
    """jansson has no NaN / Infinity literal: the whole file is unreadable."""
    raise ValueError("%s is not JSON" % name)


def drm_json_loads(text):
    """Parse a drm-output config the way the C (OBS's UTF-8 reader, then jansson) does: a leading
    byte-order mark is dropped; a NaN / Infinity literal, an integer outside 64 bits or an overflowing
    real makes the WHOLE file unreadable (ValueError), never a value the C would not see."""
    if text.startswith("\ufeff"):
        text = text[1:]
    return json.loads(text, parse_int=_drm_json_int, parse_float=_drm_json_float,
                      parse_constant=_drm_json_constant)


# --- issue 1346: DRM-lease HDMI output helpers (pure + the file read/write; no WS dependency) --------

def drm_output_view_of(value):
    """Pure: the "view" JSON value -> "program" | "multiview" | None (unknown). Mirrors the vendored C
    grammar (obs-drm-output-view.c drm_output_parse_view) row for row -- ONE shared table,
    tests/fixtures/drm_output_view_parity.tsv. Absent (None) / empty / a non-string (which
    obs_data_get_string reads as "") -> "program" (the issue-1152 default, imag unchanged); only the
    exact lowercase names count."""
    if value is None or not isinstance(value, str) or value == "":
        return "program"
    return value if value in DRM_OUTPUT_VIEWS else None


def drm_output_lease_connector(config_text):
    """Pure: the connector IFF the drm-output config arms the in-OBS DRM output, else "".
    The C module's OWN contract (and imag_scenes.drm_output_lease_connector's, pinned equal by
    tests/python/test_strih_drm_output_1346.py): a full JSON parse, a boolean "enabled": true, a
    non-empty string "connector" AND (issue 1346) a known "backend" -- the C keeps the output dormant on
    an unknown one, so the wrapper must not take the connector out of X for it (a black HDMI).
    Empty / malformed / disabled / unknown backend -> "" (dormant), never a raise.
    issue 1424 part 2: an "outputs" array config -> its FIRST connector when the whole array is valid
    (drm_output_instances), else "" (the C starts no output of a malformed array)."""
    if not config_text:
        return ""
    try:
        cfg = drm_json_loads(config_text)
        if cfg.get("enabled") is not True:
            return ""
        if "outputs" in cfg:
            connectors = drm_output_lease_connectors(config_text)
            return connectors[0] if connectors else ""
        if drm_output_backend_of(cfg.get("backend")) is None:
            return ""
        return _drm_cfg_connector(cfg) or ""
    except (ValueError, AttributeError):
        return ""


def drm_output_view_token(config_text):
    """Pure: the config's view as ONE token for verify-strih -- "program" | "multiview" | "unknown".
    An unreadable config is "program" (it arms nothing; the connector half reports that)."""
    try:
        cfg = drm_json_loads(config_text) if config_text else {}
    except ValueError:
        return "program"
    if not isinstance(cfg, dict):
        return "program"
    return drm_output_view_of(cfg.get("view")) or "unknown"


def drm_output_backend_of(value):
    """Pure: the "backend" JSON value -> "lease" | "vk-direct" | None (unknown). Mirrors the vendored C
    grammar (obs-drm-output-backend.c drm_output_parse_backend) row for row -- ONE shared table,
    tests/fixtures/drm_output_backend_parity.tsv. Absent (None) / empty / a non-string (which
    obs_data_get_string reads as "") -> "lease" (the issue-1152 default, imag unchanged); only the exact
    lowercase names count. The C keeps the output DORMANT on an unknown value."""
    if value is None or not isinstance(value, str) or value == "":
        return "lease"
    return value if value in DRM_OUTPUT_BACKENDS else None


def drm_output_backend_token(config_text):
    """Pure: the config's backend as ONE token for verify-strih -- "lease" | "vk-direct" | "unknown". An
    unreadable config is "lease" (it arms nothing; the connector half reports that)."""
    try:
        cfg = drm_json_loads(config_text) if config_text else {}
    except ValueError:
        return "lease"
    if not isinstance(cfg, dict):
        return "lease"
    return drm_output_backend_of(cfg.get("backend")) or "unknown"


def drm_output_config_text(path=DRM_OUTPUT_CONF):
    """The drm-output config text of THIS user (or `path`), "" when absent/unreadable (never raises
    -- the strih-obs-start.sh launch path must never abort on it)."""
    try:
        with open(os.path.expanduser(path)) as fh:
            return fh.read()
    except OSError:
        return ""


def read_drm_view(path=DRM_OUTPUT_CONF):
    """The persisted view ("program" | "multiview"), None when the config is absent/unreadable or
    its view is unknown."""
    text = drm_output_config_text(path)
    if not text:
        return None
    tok = drm_output_view_token(text)
    return None if tok == "unknown" else tok


def _rewrite_drm_config(path, mutate):
    """Rewrite an EXISTING drm-output config through `mutate(cfg)`: every other key kept in order, ONE
    compact line (the machine-written contract obs-drm-output.md pins), an atomic temp-file rename.
    Raises ValueError on an absent file (the output is not provisioned -- setup-strih.sh step 6 writes
    it only with an HDMI monitor plugged in) or a config that is not a JSON object (never rewritten)."""
    real = os.path.expanduser(path)
    try:
        with open(real) as fh:
            cfg = drm_json_loads(fh.read())
    except OSError as e:
        raise ValueError("%s not readable (%s) -- the HDMI output is not provisioned; attach the HDMI "
                         "monitor and re-run setup-strih.sh" % (path, e))
    except ValueError as e:
        raise ValueError("%s is not valid JSON (%s) -- not rewriting it" % (path, e))
    if not isinstance(cfg, dict):
        raise ValueError("%s is not a JSON object -- not rewriting it" % path)
    mutate(cfg)
    tmp = real + ".tmp"
    with open(tmp, "w") as fh:
        fh.write(json.dumps(cfg, separators=(",", ":")) + "\n")
    os.replace(tmp, real)


def write_drm_view(view, path=DRM_OUTPUT_CONF, connector=None):
    """Persist `view` into an EXISTING drm-output config (every other key kept -- see
    _rewrite_drm_config). issue 1424 part 2: an "outputs" array config sets the view of the entry whose
    connector is `connector` (the twin of obs_drm_output_set_view_of); `connector` may be omitted only
    when the array has ONE entry. The single-object form sets its top-level view (a `connector` given
    must be its connector). Raises ValueError on an unknown view, an unknown / unnamed connector or an
    unprovisioned / broken config."""
    if view not in DRM_OUTPUT_VIEWS:
        raise ValueError("unknown HDMI output view %r (want 'program' or 'multiview')" % (view,))

    def mutate(cfg):
        if "outputs" not in cfg:
            if connector is not None and cfg.get("connector") != connector:
                raise ValueError("%s drives %r, not %r" % (path, cfg.get("connector"), connector))
            cfg["view"] = view
            return
        entries = [e for e in cfg["outputs"] if isinstance(e, dict)] if isinstance(cfg["outputs"], list) else []
        names = [e.get("connector") for e in entries]
        if connector is None:
            if len(entries) != 1:
                raise ValueError("%s drives %d outputs (%s) -- name one with --connector"
                                 % (path, len(entries), ", ".join(str(n) for n in names)))
            entries[0]["view"] = view
            return
        for entry in entries:
            if entry.get("connector") == connector:
                entry["view"] = view
                return
        raise ValueError("%s drives no output %r (it drives %s)" % (path, connector, ", ".join(str(n) for n in names)))

    _rewrite_drm_config(path, mutate)


def write_drm_backend(backend, path=DRM_OUTPUT_CONF):
    """issue 1346: persist the box's HDMI-output backend (the fact STRIH_HDMI_OUTPUT_BACKEND) into an
    EXISTING drm-output config, keeping the operator's view and every other key. `lease` is the ABSENT
    key (a lease config stays byte-identical to the pre-backend shape); `vk-direct` is written
    explicitly. Raises ValueError on an unknown backend or an unprovisioned / broken config."""
    if backend not in DRM_OUTPUT_BACKENDS:
        raise ValueError("unknown HDMI output backend %r (want 'lease' or 'vk-direct')" % (backend,))

    def mutate(cfg):
        # issue 1424 part 2: the backend is a BOX fact, so every "outputs" entry carries it
        targets = [cfg]
        if "outputs" in cfg:
            targets = [e for e in cfg["outputs"] if isinstance(e, dict)] if isinstance(cfg["outputs"], list) else []
        for t in targets:
            if backend == "lease":
                t.pop("backend", None)
            else:
                t["backend"] = backend

    _rewrite_drm_config(path, mutate)


def write_drm_outputs(spec, backend, path=DRM_OUTPUT_CONF):
    """issue 1424 part 2: bring an EXISTING drm-output config onto the box fact STRIH_HDMI_OUTPUTS
    (`spec`, e.g. "HDMI-0:multiview HDMI-1:program") and the backend fact: the "outputs" array lists the
    fact's connectors in the fact's order. The operator's persisted view of a connector is KEPT (a
    connector the config already drives -- single-object or array form -- keeps its "view"; a new one
    gets the fact's default view), as is its "argb"; the single-object form's connector / view / argb /
    backend keys move into their entry; every other top-level key ("enabled", "program", ...) stays.
    Raises ValueError on a malformed spec, an unknown backend or an unprovisioned / broken config."""
    wanted = drm_output_spec_of(spec)
    if not wanted:
        raise ValueError("STRIH_HDMI_OUTPUTS %r names no connector list" % (spec,))
    if backend not in DRM_OUTPUT_BACKENDS:
        raise ValueError("unknown HDMI output backend %r (want 'lease' or 'vk-direct')" % (backend,))

    def mutate(cfg):
        known = {}
        if "outputs" in cfg:
            for entry in cfg["outputs"] if isinstance(cfg["outputs"], list) else []:
                if isinstance(entry, dict) and isinstance(entry.get("connector"), str):
                    known[entry["connector"]] = entry
        elif isinstance(cfg.get("connector"), str):
            known[cfg["connector"]] = {k: cfg[k] for k in ("view", "argb") if k in cfg}
        for k in ("connector", "view", "argb", "backend"):
            cfg.pop(k, None)
        outputs = []
        for conn, view in wanted:
            old = known.get(conn, {})
            keep_view = drm_output_view_of(_drm_cfg_str(old.get("view"))) if "view" in old else None
            argb = old["argb"] if "argb" in old else DRM_OUTPUT_DEFAULT_ARGB
            outputs.append(drm_output_outputs_entry(conn, keep_view or view, backend, argb))
        cfg["outputs"] = outputs

    _rewrite_drm_config(path, mutate)


# --- issue 1424 part 2: up to DRM_OUTPUT_MAX_OUTPUTS fixed outputs at once ---------------------------

def _drm_cfg_str(value):
    """A JSON value read the way the C (jansson / obs_data_get_string) reads an item: the string, else ""."""
    return value if isinstance(value, str) else ""


def _drm_cfg_connector(obj):
    """The "connector" of one config object: a string of 1..63 UTF-8 bytes (the C's bound), else None."""
    value = obj.get("connector")
    if not isinstance(value, str):
        return None
    n = len(value.encode("utf-8"))
    return value if 0 < n < DRM_OUTPUT_CONNECTOR_MAX else None


def _drm_cfg_argb(obj):
    """"argb": absent -> the default dark grey; an integer -> its low 32 bits; a real -> truncated to an
    integer first; any other present value (a bool, a string, null ...) -> 0. The C reading of it."""
    if "argb" not in obj:
        return DRM_OUTPUT_DEFAULT_ARGB
    value = obj["argb"]
    if isinstance(value, bool):
        return 0
    if isinstance(value, int):
        return value & 0xFFFFFFFF
    if isinstance(value, float):
        return int(value) & 0xFFFFFFFF
    return 0


def drm_output_instances(config_text):
    """Pure: the drm-output config -> (verdict, outputs). The vendored C grammar
    (obs-drm-output-outputs.c drm_output_config_classify) row for row -- ONE shared table,
    tests/fixtures/drm_output_outputs_parity.tsv. `outputs` (only on "ok") is a list of dicts
    {connector, view, backend, argb}, in config order. Verdicts: ok | unreadable | disabled |
    outputs-not-array | outputs-empty | outputs-too-many | entry-not-object | connector-invalid |
    connector-duplicate | view-unknown | backend-unknown.

    The single-object form (no "outputs" key) is the issue-1152/1346 grammar: an unknown view reads as
    the Program there. The "outputs" form is all or nothing: one bad entry and the C starts NO output."""
    try:
        cfg = drm_json_loads(config_text) if config_text else None
    except ValueError:
        cfg = None
    if not isinstance(cfg, dict):
        return "unreadable", []
    if cfg.get("enabled") is not True:
        return "disabled", []
    if "outputs" not in cfg:
        conn = _drm_cfg_connector(cfg)
        if conn is None:
            return "connector-invalid", []
        backend = drm_output_backend_of(_drm_cfg_str(cfg.get("backend")))
        if backend is None:
            return "backend-unknown", []
        view = drm_output_view_of(_drm_cfg_str(cfg.get("view"))) or "program"
        return "ok", [{"connector": conn, "view": view, "backend": backend, "argb": _drm_cfg_argb(cfg)}]
    outputs = cfg["outputs"]
    if not isinstance(outputs, list):
        return "outputs-not-array", []
    if not outputs:
        return "outputs-empty", []
    if len(outputs) > DRM_OUTPUT_MAX_OUTPUTS:
        return "outputs-too-many", []
    got = []
    for entry in outputs:
        if not isinstance(entry, dict):
            return "entry-not-object", []
        conn = _drm_cfg_connector(entry)
        if conn is None:
            return "connector-invalid", []
        view = drm_output_view_of(_drm_cfg_str(entry.get("view")))
        if view is None:
            return "view-unknown", []
        backend = drm_output_backend_of(_drm_cfg_str(entry.get("backend")))
        if backend is None:
            return "backend-unknown", []
        if any(o["connector"] == conn for o in got):
            return "connector-duplicate", []
        got.append({"connector": conn, "view": view, "backend": backend, "argb": _drm_cfg_argb(entry)})
    return "ok", got


def drm_output_lease_connectors(config_text):
    """Pure: every connector the drm-output config arms (config order), [] when it arms none. The
    launcher takes each of them out of the X layout before OBS starts (the vk-direct acquire and the
    lease both need an idle connector)."""
    verdict, outputs = drm_output_instances(config_text)
    return [o["connector"] for o in outputs] if verdict == "ok" else []


def drm_output_spec_of(spec):
    """Pure: the box fact STRIH_HDMI_OUTPUTS -> [(connector, view), ...] (`auto` / `none` -> []).
    Raises ValueError on a malformed value (the fact loader refuses the same)."""
    if spec in ("auto", "none"):
        return []
    out = []
    for token in spec.split():
        conn, sep, view = token.partition(":")
        if not sep or not conn or view not in DRM_OUTPUT_VIEWS or not all(
                c.isascii() and (c.isalnum() or c in "._-") for c in conn):
            raise ValueError("bad HDMI output %r in %r (want CONNECTOR:program|multiview)" % (token, spec))
        if any(c == conn for c, _ in out):
            raise ValueError("connector %r listed twice in %r" % (conn, spec))
        out.append((conn, view))
    if not 1 <= len(out) <= DRM_OUTPUT_MAX_OUTPUTS:
        raise ValueError("%r lists %d outputs (want 1..%d)" % (spec, len(out), DRM_OUTPUT_MAX_OUTPUTS))
    return out


def drm_output_outputs_entry(connector, view, backend, argb=DRM_OUTPUT_DEFAULT_ARGB):
    """One "outputs" element in the machine-written key order (connector, argb, view, backend; `lease` =
    the absent backend key, the single-object form's rule)."""
    entry = {"connector": connector, "argb": argb, "view": view}
    if backend != "lease":
        entry["backend"] = backend
    return entry
