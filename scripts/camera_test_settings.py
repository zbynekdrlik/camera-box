#!/usr/bin/env python3
"""E2E test-camera shutter/ISO enforce -- the PURE decision half (issue 1371; REST since issue 1452).

The ONE test camera (a BMPCC, fed through the HDMI splitter into every cambox) must start every
E2E run from the same exposure. Owner, 25.9.2026: "shutter a iso boli na zlych hodnotach tak tie
si mas uz ty vediet pri teste skontrolovat a nastavit". The E2E pauses the bkshading relay on the
source cambox + cam2 (issue 808), so at [0/8] the step talks to the camera alone. Since issue 1452
(shading#6, owner 9.11.2026) the camera is driven over the official Blackmagic REST API on its own
USB-C network link (CDC-NCM), exactly as the shading v0.2.x relay does; gphoto2/PTP is gone. The thin
bash transport (`scripts/lib/camera-test-settings.sh`) runs `curl` on the cambox and hands the raw
lines here; THIS module holds every decision:

  * which REST endpoints and fields are read/enforced -- the shading contract's (docs/CONTRACT.md
    v0.2.2, `tests/fixtures/shading_pinned_rest/CONTRACT.md`; a Tier-0 test pins every endpoint here
    to a contract row);
  * the checked-in baseline (`scripts/camera-test-baseline.json`, schema 2) -- load + validate;
  * parse the transport's `CTS_GET <endpoint> <http code> <body>` lines, diff vs the baseline, build
    the PUT list (`SETREQ <endpoint> <json>`);
  * grade the read-back after a set;
  * the presence/ack/pinned decision matrix (see `decide`).

Keys (the REST field names):
  REQUIRED  iso (`video/iso`) and the shutter (`video/shutter`), which the camera reports EITHER as
            `shutterAngle` (degrees x100, 18000 = 180 deg) OR as `shutterSpeed` (1/N s), per its own
            shutter-display setting. The step reads and sets it in the unit the camera reports
            (the contract's rule); the baseline may pin both units, and a camera reporting a unit the
            baseline leaves null is a named refusal (EXIT_SHUTTER_UNIT), never a guessed conversion.
            Pinned = iso AND at least one shutter unit.
  OPTIONAL  whiteBalance (K), whiteBalanceTint, apertureStop (the true f-stop; only `apertureStop`
            moves the iris, the contract) -- null = read + logged only, a value = enforced.
  CONTEXT   fps (`system/format` frameRate) -- read only to log the shutter as 1/N s; NEVER set (fps
            is the issue-809 grab-mode coupling, not a test-exposure setting).
  An optional endpoint the firmware does not serve (404/501, e.g. the Pocket 6K at fw 8.6) reads None.

CLI (used by the bash lib; every subcommand is pure: stdin/args in, stdout + exit code out):
  status  --baseline F                     -> "pinned" | "unpinned"         (exit 2 = invalid file)
  decide  --present 0|1 --acked 0|1 --pinned 0|1   -> the action word (see `decide`)
  read-endpoints                           -> the endpoints of the ONE read session
  plan    --baseline F  < read text        -> BEFORE/SHUTTER/SET/SETREQ lines (exit 3 = unreadable,
                                              exit 8 = the camera's shutter unit is not pinned)
  suggest < read text                      -> a baseline JSON with the camera's CURRENT values
  grade   --baseline F  < read text        -> AFTER/MISMATCH lines (exit 5 = a set did not read back)

The PRODUCTION-EXPOSURE snapshot (owner, 26.9.2026: "ked vypina sa development tak ze aj vratis iso
a uzavierku naspat"): right before the FIRST set of a development period the E2E stores the values
it is about to overwrite (the owner's production ISO + shutter) on the runner; the rig-mode EVENT
switch restores them, reads them back and moves the file aside as consumed.
  snapshot-path                            -> the snapshot path ($CAMERA_PROD_EXPOSURE_SNAPSHOT, else
                                              ~/.camera-box/camera-prod-exposure.json)
  snapshot --baseline F --snapshot P --box B < read text
                                           -> SNAPSHOT saved|kept|extended (exit 6 = no record)
  restore-status --snapshot P              -> the one-line Slovak summary (exit 4 / 7)
  restore-plan   --snapshot P < read text  -> NOW/RESTORE/RESTOREREQ lines (exit 3 / 7 / 8)
  restore-grade  --snapshot P < read text  -> AFTER/RESTORED/MISMATCH lines (exit 5 = not restored)
  consume / restore-failed / snapshot-state  (unchanged since issue 1371)
A schema-1 (gphoto2-era) snapshot is read through LEGACY_SNAPSHOT_KEYS: `iso`, `d002` (the shutter
angle x100 = REST `shutterAngle`), `d004`/`d005` (K / tint), `f-number` (`f/4.0` = apertureStop 4.0),
the project fps `context.d006` (x100) or the legacy whole-fps `context.d007` (issue 1402).
"""
import argparse
import datetime
import json
import os
import re
import sys

# (key, endpoint, JSON field) -- the contract's names.
FIELDS = (
    ("iso", "video/iso", "iso"),
    ("shutterAngle", "video/shutter", "shutterAngle"),
    ("shutterSpeed", "video/shutter", "shutterSpeed"),
    ("whiteBalance", "video/whiteBalance", "whiteBalance"),
    ("whiteBalanceTint", "video/whiteBalanceTint", "whiteBalanceTint"),
    ("apertureStop", "lens/iris", "apertureStop"),
)
SHUTTER_KEYS = ("shutterAngle", "shutterSpeed")
REQUIRED_KEYS = ("iso",) + SHUTTER_KEYS  # iso + ONE shutter unit (see baseline_pinned)
OPTIONAL_KEYS = ("whiteBalance", "whiteBalanceTint", "apertureStop")
ENFORCEABLE_KEYS = tuple(k for k, _e, _f in FIELDS)
ENDPOINT_OF = {k: e for k, e, _f in FIELDS}
FIELD_OF = {k: f for k, _e, f in FIELDS}
CONTEXT_ENDPOINT = "system/format"
READ_ENDPOINTS = tuple(dict.fromkeys([e for _k, e, _f in FIELDS] + [CONTEXT_ENDPOINT]))
# the endpoints whose absence makes a read unreadable (the rest are optional per firmware)
REQUIRED_ENDPOINTS = ("video/iso", "video/shutter")
NOT_SERVED_CODES = ("404", "501")
FLOAT_KEYS = ("apertureStop",)

KEY_LABELS = {
    "iso": "ISO",
    "shutterAngle": "shutter angle x100",
    "shutterSpeed": "shutter 1/N s",
    "whiteBalance": "white balance K",
    "whiteBalanceTint": "tint",
    "apertureStop": "f-stop",
    "fps": "project fps",
}
# Plausible bounds: a value outside is a typo in the baseline, never sent to a camera.
VALUE_BOUNDS = {
    "iso": (25, 102400),
    "shutterAngle": (1, 36000),
    "shutterSpeed": (1, 100000),
    "whiteBalance": (1000, 20000),
    "whiteBalanceTint": (-100, 100),
    "apertureStop": (0.5, 64.0),
}

# LEGACY (schema 1, gphoto2 era): snapshot key -> REST key.
LEGACY_SNAPSHOT_KEYS = {"iso": "iso", "d002": "shutterAngle", "d004": "whiteBalance",
                        "d005": "whiteBalanceTint", "f-number": "apertureStop"}
LEGACY_CONTEXT_FPS_KEY = "d007"

BASELINE_SCHEMA = 2

EXIT_OK = 0
EXIT_BAD_BASELINE = 2
EXIT_UNREADABLE = 3
EXIT_MISMATCH = 5
EXIT_NO_SNAPSHOT = 4
EXIT_SNAPSHOT_FAILED = 6
EXIT_BAD_SNAPSHOT = 7
EXIT_SHUTTER_UNIT = 8

SNAPSHOT_SCHEMA = 2
SNAPSHOT_ENV = "CAMERA_PROD_EXPOSURE_SNAPSHOT"
SNAPSHOT_DIR = ".camera-box"
SNAPSHOT_BASENAME = "camera-prod-exposure.json"
CONSUMED_STAMP_RE = re.compile(r"^\d{8}T\d{6}Z(-\d+)?$")
# box label + UTC time are logged and parsed back as `key=value` words: plain tokens only
SNAPSHOT_FIELD_RE = re.compile(r"^[A-Za-z0-9._:+-]+$")
FPS_RE = re.compile(r"^[0-9]+(\.[0-9]+)?$")

# decide() actions
ENFORCE = "enforce"
ABORT_UNPINNED = "abort-unpinned"
ABORT_STALE_ACK = "abort-stale-ack"
ABORT_ABSENT = "abort-absent"
UNVERIFIED_ACKED = "unverified-acked"
UNVERIFIED_UNPINNED = "unverified-unpinned"


class BaselineError(ValueError):
    """The baseline file is malformed (never silently treated as 'unpinned')."""


class SnapshotError(ValueError):
    """A production-exposure snapshot that cannot be taken or trusted (never silently skipped)."""


class ShutterUnitError(ValueError):
    """The camera reports its shutter in a unit the baseline / snapshot does not hold."""


def _number(key, value, error=BaselineError):
    """A validated camera value (int, or float for apertureStop) or None. A bool, a string, a value
    outside VALUE_BOUNDS or a non-integral value for an int key raises `error`."""
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise error("%s must be a number or null, got %r" % (key, value))
    lo, hi = VALUE_BOUNDS[key]
    if not lo <= value <= hi:
        raise error("%s %r is outside %s..%s" % (key, value, lo, hi))
    if key in FLOAT_KEYS:
        return float(value)
    if float(value) != int(value):
        raise error("%s must be a whole number, got %r" % (key, value))
    return int(value)


def same(key, a, b):
    """Camera value equality: apertureStop to 1/100 stop (the camera reports 2 decimals)."""
    if a is None or b is None:
        return a is None and b is None
    if key in FLOAT_KEYS:
        return abs(float(a) - float(b)) < 0.005
    return int(a) == int(b)


def load_baseline(text):
    """Parse + validate the baseline JSON text -> {key: number|None} over ENFORCEABLE_KEYS.

    Every key must be PRESENT (null = not pinned), no unknown key is allowed, and a value must be a
    plausible number. Raises BaselineError on anything else."""
    try:
        doc = json.loads(text)
    except ValueError as e:
        raise BaselineError("baseline is not valid JSON: %s" % e)
    if not isinstance(doc, dict):
        raise BaselineError("baseline must be a JSON object")
    if doc.get("schema") != BASELINE_SCHEMA:
        raise BaselineError("baseline schema must be %d, got %r" % (BASELINE_SCHEMA, doc.get("schema")))
    values = doc.get("values")
    if not isinstance(values, dict):
        raise BaselineError("baseline must carry a 'values' object")
    unknown = sorted(set(values) - set(ENFORCEABLE_KEYS))
    if unknown:
        raise BaselineError("baseline has unknown key(s) %s; allowed: %s" % (unknown, list(ENFORCEABLE_KEYS)))
    missing = [k for k in ENFORCEABLE_KEYS if k not in values]
    if missing:
        raise BaselineError("baseline is missing key(s) %s (write null for 'not pinned')" % missing)
    return {k: _number(k, values[k]) for k in ENFORCEABLE_KEYS}


def baseline_pinned(baseline):
    """True when ISO and at least one shutter unit have a value."""
    return baseline.get("iso") is not None and any(baseline.get(k) is not None for k in SHUTTER_KEYS)


def camera_shutter_key(current):
    """The shutter unit the camera reports (`shutterAngle` wins when a body carried both), or None."""
    for k in SHUTTER_KEYS:
        if current.get(k) is not None:
            return k
    return None


def enforced_keys(baseline, current):
    """The keys this run enforces on THIS camera: iso, the shutter in the unit the camera reports,
    and every pinned optional key. Raises ShutterUnitError when the camera's unit is not pinned."""
    unit = camera_shutter_key(current)
    if unit is None:
        raise ShutterUnitError("the camera reported no shutter value")
    if baseline.get(unit) is None:
        other = [k for k in SHUTTER_KEYS if k != unit][0]
        raise ShutterUnitError("the camera reports its shutter as %s, but the baseline pins only %s -- pin "
                               "%s too (the same exposure in the camera's unit), or switch the camera's "
                               "shutter display" % (unit, other, unit))
    keys = ["iso", unit] + [k for k in OPTIONAL_KEYS if baseline.get(k) is not None]
    return [k for k in ENFORCEABLE_KEYS if k in keys]


def parse_rest_lines(text):
    """{endpoint: (http code, body text)} from the transport's `CTS_GET <ep> <code> <body>` lines."""
    out = {}
    for raw in text.splitlines():
        parts = raw.strip().split(" ", 3)
        if len(parts) >= 3 and parts[0] == "CTS_GET":
            out[parts[1]] = (parts[2], parts[3] if len(parts) == 4 else "")
    return out


def _json_obj(body):
    try:
        doc = json.loads(body)
    except ValueError:
        return None
    return doc if isinstance(doc, dict) else None


def _fps_text(value):
    """The project fps as plain text (`60`, `59.94`) or None."""
    if isinstance(value, bool) or value is None:
        return None
    text = ("%s" % value).strip()
    if isinstance(value, float):
        text = ("%.3f" % value).rstrip("0").rstrip(".")
    return text if FPS_RE.match(text) else None


def parse_read(text):
    """{key: number | None, "fps": text | None} over ENFORCEABLE_KEYS, or None when unreadable: a
    required endpoint missing / not 200 / not a JSON object, no ISO, or no shutter value. An optional
    endpoint answering 404/501 (not served by this firmware) or anything else reads None."""
    got = parse_rest_lines(text)
    bodies = {}
    for ep in READ_ENDPOINTS:
        code, body = got.get(ep, (None, ""))
        if code == "200":
            bodies[ep] = _json_obj(body)
        else:
            bodies[ep] = None
        if ep in REQUIRED_ENDPOINTS and bodies[ep] is None:
            return None
    current = {}
    for key, ep, field in FIELDS:
        body = bodies.get(ep)
        raw = body.get(field) if body else None
        try:
            current[key] = _number(key, raw, error=ValueError)
        except ValueError:
            current[key] = None
    fmt = bodies.get(CONTEXT_ENDPOINT)
    current["fps"] = _fps_text(fmt.get("frameRate")) if fmt else None
    if current["iso"] is None or camera_shutter_key(current) is None:
        return None
    return current


def _json_body(field, value):
    return json.dumps({field: value}, separators=(",", ":"))


def set_requests(pairs):
    """[(endpoint, json body)] for [(key, value)]: one PUT per key, in FIELDS order."""
    return [(ENDPOINT_OF[k], _json_body(FIELD_OF[k], v)) for k, v in pairs]


def plan_sets(current, baseline):
    """[(key, value)] for every enforced key whose current value differs from the baseline."""
    return [(k, baseline[k]) for k in enforced_keys(baseline, current) if not same(k, current.get(k), baseline[k])]


def grade_readback(readback, baseline):
    """[(key, want, got)] for every enforced key the camera does not report at the baseline value."""
    return [(k, baseline[k], readback.get(k)) for k in enforced_keys(baseline, readback)
            if not same(k, readback.get(k), baseline[k])]


def shutter_denominator(key, value, fps):
    """The shutter as a 1/N denominator, or None. Log context only. `shutterSpeed` IS N;
    `shutterAngle` (x100) at the project fps: round-half-up of 360 * fps / angle (the relay's
    `convert_angle_or_denom` with both x100)."""
    if value is None:
        return None
    if key == "shutterSpeed":
        return int(value)
    try:
        fps_v = float(fps)
    except (TypeError, ValueError):
        return None
    if value <= 0 or fps_v <= 0:
        return None
    return max(1, int(360.0 * fps_v * 100 / value + 0.5))


def _fmt(key, value):
    if value is None:
        return "-"
    if key in FLOAT_KEYS:
        return ("%.2f" % value).rstrip("0").rstrip(".")
    return str(value)


def _x100_text(value):
    """An x100 camera value as plain text: 2160 -> `21.6`."""
    return ("%.2f" % (int(value) / 100.0)).rstrip("0").rstrip(".")


def decide(present, acked, pinned):
    """The single decision matrix of the step.

    camera on USB:
      acked            -> abort-stale-ack   (the cambox-offline-ack contract: an ack for a thing
                                             that IS there must be removed, loudly)
      baseline null    -> abort-unpinned    (refuse loudly; the lib prints the values to pin)
      baseline pinned  -> enforce           (read, set what differs, read back, abort on mismatch)
    camera NOT on USB:
      acked            -> unverified-acked  (loud, report-only: operator says it is offline)
      baseline null    -> unverified-unpinned (loud, report-only: nothing to enforce yet)
      baseline pinned  -> abort-absent      (a named abort: the test can no longer vouch for exposure)
    """
    if present:
        if acked:
            return ABORT_STALE_ACK
        return ENFORCE if pinned else ABORT_UNPINNED
    if acked:
        return UNVERIFIED_ACKED
    return ABORT_ABSENT if pinned else UNVERIFIED_UNPINNED


def _before_lines(tag, current):
    lines = []
    for k in ENFORCEABLE_KEYS:
        lines.append("%s %s %s  (%s)" % (tag, k, _fmt(k, current.get(k)), KEY_LABELS[k]))
    lines.append("%s fps %s  (%s)" % (tag, current.get("fps") or "-", KEY_LABELS["fps"]))
    unit = camera_shutter_key(current)
    denom = shutter_denominator(unit, current.get(unit), current.get("fps")) if unit else None
    if denom is not None:
        lines.append("SHUTTER %s 1/%d s at %s fps" % (tag, denom, current.get("fps") or "?"))
    return lines


def suggest_baseline(current):
    """A baseline document pinning the camera's CURRENT ISO + shutter (in its own unit); the optional
    keys stay null (pinning them is a deliberate supervisor choice)."""
    values = {k: None for k in ENFORCEABLE_KEYS}
    values["iso"] = current.get("iso")
    unit = camera_shutter_key(current)
    if unit:
        values[unit] = current.get(unit)
    return {"schema": BASELINE_SCHEMA, "values": values}


# ---------------------------------------------------------------------------------------------
# the production-exposure snapshot (taken before the FIRST test set, restored at the EVENT switch)
# ---------------------------------------------------------------------------------------------
SNAPSHOT_COMMANDS = ("snapshot-path", "snapshot", "restore-status", "restore-plan", "restore-grade",
                     "consume", "snapshot-state", "restore-failed")


def default_snapshot_path(env=None):
    """$CAMERA_PROD_EXPOSURE_SNAPSHOT, else ~/.camera-box/camera-prod-exposure.json on the runner
    (dev1: the E2E and rig-mode both run there, so both sides meet at the same file)."""
    env = os.environ if env is None else env
    override = env.get(SNAPSHOT_ENV)
    if override:
        return override
    home = env.get("HOME") or os.path.expanduser("~")
    return os.path.join(home, SNAPSHOT_DIR, SNAPSHOT_BASENAME)


def _enforced_or_snapshot_error(baseline, current):
    try:
        return enforced_keys(baseline, current)
    except ShutterUnitError as e:
        raise SnapshotError(str(e))


def build_snapshot(current, baseline, box, taken_utc):
    """The snapshot document: the camera's CURRENT value of every key this run enforces (the keys it
    may overwrite), plus the project fps as log context. A key the camera reports no value for is
    left out (there is nothing to put back)."""
    for field, v in (("box", box), ("taken_utc", taken_utc)):
        if not isinstance(v, str) or not SNAPSHOT_FIELD_RE.match(v):
            raise SnapshotError("snapshot %s %r is not a plain token, the restore could not read it back"
                                % (field, v))
    values = {k: current[k] for k in _enforced_or_snapshot_error(baseline, current)
              if current.get(k) is not None}
    if not values:
        raise SnapshotError("the camera reported none of the enforced keys, nothing to snapshot")
    context = {"fps": current["fps"]} if current.get("fps") else {}
    return {"schema": SNAPSHOT_SCHEMA, "box": box, "taken_utc": taken_utc, "values": values,
            "context": context}


def extend_snapshot(doc, current, baseline):
    """A pending snapshot only GAINS a key the baseline enforces now but did not when it was taken
    (the camera still has the owner's value for it -- the test never set that key); a kept value is
    never changed. Returns (new doc, [added keys])."""
    values = dict(doc["values"])
    added = []
    for k in _enforced_or_snapshot_error(baseline, current):
        if k in values or current.get(k) is None:
            continue
        if k in SHUTTER_KEYS and any(s in values for s in SHUTTER_KEYS):
            continue  # the owner's shutter is already held, in the unit the camera reported then
        values[k] = current[k]
        added.append(k)
    out = dict(doc)
    out["values"] = values
    return out, added


def _legacy_fstop(value):
    """A gphoto2 `f/4.0` / `f/4` / `4` label as a number."""
    text = value.strip()
    if text.lower().startswith("f/"):
        text = text[2:]
    return float(text)


def _upgrade_legacy(doc):
    """A schema-1 (gphoto2) snapshot as schema 2: the keys mapped, the values made numbers, the project
    fps from `context.d006` (x100), else the legacy whole-fps `context.d007` (issue 1402)."""
    values = {}
    for old, v in doc.get("values", {}).items():
        if old not in LEGACY_SNAPSHOT_KEYS:
            raise SnapshotError("legacy snapshot has key %r that is never restored; allowed: %s"
                                % (old, sorted(LEGACY_SNAPSHOT_KEYS)))
        if not isinstance(v, str):
            raise SnapshotError("legacy snapshot value for %s is not a string: %r" % (old, v))
        new = LEGACY_SNAPSHOT_KEYS[old]
        try:
            values[new] = _legacy_fstop(v) if new == "apertureStop" else int(v)
        except ValueError:
            raise SnapshotError("legacy snapshot value for %s is not a number: %r" % (old, v))
    ctx = doc.get("context", {})
    if not isinstance(ctx, dict):
        raise SnapshotError("snapshot 'context' must be an object")
    fps = None
    if "d006" in ctx:
        try:
            fps = _fps_text(int(ctx["d006"]) / 100.0)
        except (TypeError, ValueError):
            fps = None
    elif LEGACY_CONTEXT_FPS_KEY in ctx:
        try:
            fps = _fps_text(int(ctx[LEGACY_CONTEXT_FPS_KEY]))
        except (TypeError, ValueError):
            fps = None
    out = dict(doc)
    out.update({"schema": SNAPSHOT_SCHEMA, "values": values, "context": {"fps": fps} if fps else {}})
    return out


def load_snapshot(text):
    """Parse + validate a snapshot document (schema 2, or a schema-1 one upgraded): enforceable keys
    with plausible number values only."""
    try:
        doc = json.loads(text)
    except ValueError as e:
        raise SnapshotError("snapshot is not valid JSON: %s" % e)
    if not isinstance(doc, dict):
        raise SnapshotError("snapshot must be a JSON object")
    for field in ("box", "taken_utc"):
        v = doc.get(field)
        if not isinstance(v, str) or not SNAPSHOT_FIELD_RE.match(v):
            raise SnapshotError("snapshot field %r must be a plain non-empty string, got %r" % (field, v))
    if not isinstance(doc.get("values"), dict) or not doc["values"]:
        raise SnapshotError("snapshot must carry a non-empty 'values' object")
    if doc.get("schema") == 1:
        doc = _upgrade_legacy(doc)
    elif doc.get("schema") != SNAPSHOT_SCHEMA:
        raise SnapshotError("snapshot schema must be 1 or %d, got %r" % (SNAPSHOT_SCHEMA, doc.get("schema")))
    values = doc["values"]
    unknown = sorted(set(values) - set(ENFORCEABLE_KEYS))
    if unknown:
        raise SnapshotError("snapshot has key(s) %s that are never restored; allowed: %s"
                            % (unknown, list(ENFORCEABLE_KEYS)))
    for k in list(values):
        values[k] = _number(k, values[k], error=SnapshotError)
        if values[k] is None:
            raise SnapshotError("snapshot value for %s is null" % k)
    context = doc.get("context", {})
    if not isinstance(context, dict):
        raise SnapshotError("snapshot 'context' must be an object")
    return doc


def _snapshot_keys(snapshot_values, current):
    """The snapshot keys in FIELDS order; a shutter held in a unit the camera does not report now
    raises ShutterUnitError (never a guessed conversion: set it by hand)."""
    unit = camera_shutter_key(current)
    keys = [k for k in ENFORCEABLE_KEYS if k in snapshot_values]
    for k in keys:
        if k in SHUTTER_KEYS and k != unit:
            raise ShutterUnitError("the snapshot holds the shutter as %s, the camera reports %s now -- set "
                                   "the camera's shutter display back, or the shutter by hand" % (k, unit))
    return keys


def restore_plan(snapshot_values, current):
    """[(key, production value)] for every snapshot key the camera does not report at that value."""
    return [(k, snapshot_values[k]) for k in _snapshot_keys(snapshot_values, current)
            if not same(k, current.get(k), snapshot_values[k])]


def grade_restore(snapshot_values, readback):
    """[(key, want, got)] for every snapshot key that did not read back at its production value."""
    return [(k, snapshot_values[k], readback.get(k)) for k in _snapshot_keys(snapshot_values, readback)
            if not same(k, readback.get(k), snapshot_values[k])]


def snapshot_summary(doc):
    """One plain Slovak line for the operator log and the EVENT Discord note, e.g.
    `ISO 800, uzávierka 1/60 s (uhol 360°), cam1 2026-09-26T15:00:00Z`."""
    values = doc["values"]
    fps = doc.get("context", {}).get("fps")
    parts = []
    if "iso" in values:
        parts.append("ISO %s" % values["iso"])
    if "shutterAngle" in values:
        denom = shutter_denominator("shutterAngle", values["shutterAngle"], fps)
        angle = _x100_text(values["shutterAngle"])
        if denom is not None:
            parts.append("uzávierka 1/%d s (uhol %s°)" % (denom, angle))
        else:
            parts.append("uzávierka uhol %s°" % angle)
    if "shutterSpeed" in values:
        parts.append("uzávierka 1/%s s" % values["shutterSpeed"])
    for k in OPTIONAL_KEYS:
        if k in values:
            parts.append("%s %s" % (KEY_LABELS[k], _fmt(k, values[k])))
    return "%s, %s %s" % (", ".join(parts), doc["box"], doc["taken_utc"])


def _values_tokens(values):
    return " ".join("%s=%s" % (k, _fmt(k, values[k])) for k in ENFORCEABLE_KEYS if k in values)


def _consumed_parts(path):
    """(prefix, suffix) of a consumed snapshot's basename: `<stem>.consumed-` + `.json`."""
    base = os.path.basename(path)
    stem = base[:-len(".json")] if base.endswith(".json") else base
    return stem + ".consumed-", ".json"


def consumed_path(path, stamp):
    """Where a restored snapshot is moved: `<stem>.consumed-<stamp>.json`, next to it."""
    prefix, suffix = _consumed_parts(path)
    return os.path.join(os.path.dirname(path), prefix + stamp + suffix)


def unique_consumed_path(path, stamp, exists):
    """consumed_path(path, stamp), or the same name with `-1`, `-2`, ... when `exists` says it is
    taken: two restores in one second never overwrite an earlier consumed snapshot."""
    target = consumed_path(path, stamp)
    n = 0
    while exists(target):
        n += 1
        target = consumed_path(path, "%s-%d" % (stamp, n))
    return target


def restore_failed_path(path):
    """The marker a failed EVENT restore leaves next to the snapshot: `<stem>.restore-failed.json`."""
    base = os.path.basename(path)
    stem = base[:-len(".json")] if base.endswith(".json") else base
    return os.path.join(os.path.dirname(path), stem + ".restore-failed.json")


def _restore_failed_token(path):
    """` restore_failed=<utc>` when a failed EVENT restore is recorded for this snapshot, else ''."""
    marker = restore_failed_path(path)
    if not os.path.exists(marker):
        return ""
    try:
        with open(marker, encoding="utf-8") as f:
            utc = json.load(f).get("utc")
    except (OSError, ValueError, AttributeError):
        utc = None
    if not isinstance(utc, str) or not SNAPSHOT_FIELD_RE.match(utc):
        utc = "unknown"
    return " restore_failed=%s" % utc


def _consumed_stamp(path, name):
    prefix, suffix = _consumed_parts(path)
    if not (name.startswith(prefix) and name.endswith(suffix)):
        return None
    stamp = name[len(prefix):len(name) - len(suffix)]
    return stamp if CONSUMED_STAMP_RE.match(stamp) else None


def _consumed_sort_key(stamp):
    """(time stamp, same-second counter): `…Z` < `…Z-1` < `…Z-2` < `…Z-10`."""
    base, _sep, n = stamp.partition("-")
    return base, int(n) if n else 0


def newest_consumed(path, names):
    """The newest `<stem>.consumed-<stamp>.json` basename among `names`, or None."""
    found = [(_consumed_sort_key(_consumed_stamp(path, n)), n) for n in names
             if _consumed_stamp(path, n) is not None]
    return max(found)[1] if found else None


def snapshot_state(path):
    """ONE line for the development handover check:
      exposure state=none
      exposure state=pending box=B taken=T iso=.. shutterAngle=..
      exposure state=restored restored=STAMP box=B taken=T iso=.. shutterAngle=..
      exposure state=invalid path=P reason=..."""
    if os.path.exists(path):
        try:
            doc = _load_snapshot_file(path)
        except (OSError, SnapshotError) as e:
            return "exposure state=invalid path=%s reason=%s" % (path, re.sub(r"\s+", "-", str(e))[:160])
        return "exposure state=pending box=%s taken=%s %s%s" % (doc["box"], doc["taken_utc"],
                                                               _values_tokens(doc["values"]),
                                                               _restore_failed_token(path))
    folder = os.path.dirname(path) or "."
    try:
        names = os.listdir(folder)
    except OSError:
        names = []
    newest = newest_consumed(path, names)
    if newest is None:
        return "exposure state=none"
    try:
        doc = _load_snapshot_file(os.path.join(folder, newest))
        detail = "box=%s taken=%s %s" % (doc["box"], doc["taken_utc"], _values_tokens(doc["values"]))
    except (OSError, SnapshotError):
        detail = "file=%s" % newest
    return "exposure state=restored restored=%s %s" % (_consumed_stamp(path, newest), detail)


def _utc_now():
    return datetime.datetime.now(datetime.timezone.utc)


def write_snapshot_once(path, doc):
    """Write `doc` to `path` ONLY when it does not exist yet: a temp file in the same folder, then
    an exclusive hard link (fails when the path appeared meanwhile), so a half-written or replaced
    snapshot is impossible. Returns True when written, False when a snapshot was already there."""
    return _write_via_temp(path, doc, exclusive=True)


def _write_via_temp(path, doc, exclusive):
    """Write `doc` to a temp file next to `path`, then publish it: an exclusive hard link (False when
    `path` appeared meanwhile) or an atomic replace. The temp file never survives, even when the
    write itself fails."""
    folder = os.path.dirname(path) or "."
    os.makedirs(folder, exist_ok=True)
    tmp = "%s.tmp-%d" % (path, os.getpid())
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(json.dumps(doc, sort_keys=True) + "\n")
            f.flush()
            os.fsync(f.fileno())
        if exclusive:
            try:
                os.link(tmp, path)
            except FileExistsError:
                return False
        else:
            os.replace(tmp, path)
        return True
    finally:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass


def _load_snapshot_file(path):
    with open(path, encoding="utf-8") as f:
        return load_snapshot(f.read())


def _snapshot_kept(path):
    print("SNAPSHOT kept %s (a production exposure is already waiting for the EVENT restore)" % path)
    return EXIT_OK


def _snapshot_extend(path, baseline, current):
    """A snapshot is already waiting: never overwrite it, never trust an unreadable one, only add a
    key pinned since it was taken."""
    try:
        existing = _load_snapshot_file(path)
    except (OSError, SnapshotError) as e:
        print("the pending production exposure snapshot %s is unreadable (%s): it is no record the "
              "EVENT switch could restore -- fix it or move it aside (consume) before the camera is "
              "changed" % (path, e), file=sys.stderr)
        return EXIT_SNAPSHOT_FAILED
    try:
        doc, added = extend_snapshot(existing, current, baseline)
        if not added:
            return _snapshot_kept(path)
        _write_via_temp(path, doc, exclusive=False)
    except SnapshotError as e:
        print("cannot snapshot the production exposure: %s" % e, file=sys.stderr)
        return EXIT_SNAPSHOT_FAILED
    except OSError as e:
        print("cannot extend the production exposure snapshot %s: %s" % (path, e), file=sys.stderr)
        return EXIT_SNAPSHOT_FAILED
    print("SNAPSHOT extended %s with %s: %s" % (path, ", ".join(added), snapshot_summary(doc)))
    return EXIT_OK


def _snapshot_take(a, path):
    """`snapshot`: store the values the E2E is about to overwrite, only when none is waiting."""
    try:
        baseline = _load_baseline_file(a.baseline)
    except (OSError, BaselineError) as e:
        print("invalid baseline %s: %s" % (a.baseline, e), file=sys.stderr)
        return EXIT_BAD_BASELINE
    current = parse_read(sys.stdin.read())
    if current is None:
        print("unreadable camera REST read", file=sys.stderr)
        return EXIT_UNREADABLE
    if os.path.exists(path):
        return _snapshot_extend(path, baseline, current)
    try:
        doc = build_snapshot(current, baseline, a.box, _utc_now().strftime("%Y-%m-%dT%H:%M:%SZ"))
        written = write_snapshot_once(path, doc)
    except SnapshotError as e:
        print("cannot snapshot the production exposure: %s" % e, file=sys.stderr)
        return EXIT_SNAPSHOT_FAILED
    except OSError as e:
        print("cannot write the production exposure snapshot %s: %s" % (path, e), file=sys.stderr)
        return EXIT_SNAPSHOT_FAILED
    if not written:
        return _snapshot_extend(path, baseline, current)
    # a marker without a snapshot is stale by definition: it belonged to an earlier period
    try:
        os.unlink(restore_failed_path(path))
    except FileNotFoundError:
        pass
    except OSError as e:
        print("WARNING: a stale restore-failed marker could not be removed: %s" % e, file=sys.stderr)
    print("SNAPSHOT saved %s: %s" % (path, snapshot_summary(doc)))
    return EXIT_OK


def _snapshot_restore(a, path):
    """restore-status / restore-plan / restore-grade over a pending snapshot."""
    if not os.path.exists(path):
        print("no production exposure snapshot at %s" % path, file=sys.stderr)
        return EXIT_NO_SNAPSHOT
    try:
        doc = _load_snapshot_file(path)
    except (OSError, SnapshotError) as e:
        print("invalid production exposure snapshot %s: %s" % (path, e), file=sys.stderr)
        return EXIT_BAD_SNAPSHOT
    if a.cmd == "restore-status":
        print(snapshot_summary(doc))
        return EXIT_OK
    current = parse_read(sys.stdin.read())
    if current is None:
        print("unreadable camera REST read (video/iso + video/shutter must answer 200 with values)",
              file=sys.stderr)
        return EXIT_UNREADABLE
    values = doc["values"]
    try:
        if a.cmd == "restore-plan":
            for line in _before_lines("NOW", current):
                print(line)
            sets = restore_plan(values, current)
            for k, v in sets:
                print("RESTORE %s %s -> %s" % (k, _fmt(k, current.get(k)), _fmt(k, v)))
            for ep, body in set_requests(sets):
                print("RESTOREREQ %s %s" % (ep, body))
            return EXIT_OK
        bad = grade_restore(values, current)
    except ShutterUnitError as e:
        print("cannot restore the shutter: %s" % e, file=sys.stderr)
        return EXIT_SHUTTER_UNIT
    for line in _before_lines("AFTER", current):
        print(line)
    bad_keys = {k for k, _want, _got in bad}
    for k in ENFORCEABLE_KEYS:
        if k in values and k not in bad_keys:
            print("RESTORED %s %s" % (k, _fmt(k, values[k])))
    for k, want, got in bad:
        print("MISMATCH %s want=%s got=%s" % (k, _fmt(k, want), _fmt(k, got)))
    return EXIT_MISMATCH if bad else EXIT_OK


def _snapshot_main(a):
    """The snapshot / restore subcommands (argparse namespace in, exit code out)."""
    if a.cmd == "snapshot-path":
        print(default_snapshot_path())
        return EXIT_OK
    path = a.snapshot or default_snapshot_path()
    if a.cmd == "snapshot-state":
        print(snapshot_state(path))
        return EXIT_OK
    if a.cmd == "snapshot":
        return _snapshot_take(a, path)
    if a.cmd == "consume":
        target = unique_consumed_path(path, _utc_now().strftime("%Y%m%dT%H%M%SZ"), os.path.exists)
        try:
            os.rename(path, target)
        except OSError as e:
            print("cannot move the snapshot %s aside: %s" % (path, e), file=sys.stderr)
            return EXIT_SNAPSHOT_FAILED
        try:
            os.unlink(restore_failed_path(path))
        except FileNotFoundError:
            pass
        except OSError as e:
            print("the snapshot is moved aside, but the restore-failed marker could not be removed: %s"
                  % e, file=sys.stderr)
        print(target)
        return EXIT_OK
    if a.cmd == "restore-failed":
        doc = {"utc": _utc_now().strftime("%Y-%m-%dT%H:%M:%SZ"), "reason": a.reason}
        try:
            with open(restore_failed_path(path), "w", encoding="utf-8") as f:
                f.write(json.dumps(doc, sort_keys=True) + "\n")
        except OSError as e:
            print("cannot record the failed restore next to %s: %s" % (path, e), file=sys.stderr)
            return EXIT_SNAPSHOT_FAILED
        return EXIT_OK
    return _snapshot_restore(a, path)


def _load_baseline_file(path):
    with open(path, encoding="utf-8") as f:
        return load_baseline(f.read())


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("status")
    s.add_argument("--baseline", required=True)
    d = sub.add_parser("decide")
    d.add_argument("--present", choices=("0", "1"), required=True)
    d.add_argument("--acked", choices=("0", "1"), required=True)
    d.add_argument("--pinned", choices=("0", "1"), required=True)
    sub.add_parser("read-endpoints")
    pl = sub.add_parser("plan")
    pl.add_argument("--baseline", required=True)
    sub.add_parser("suggest")
    g = sub.add_parser("grade")
    g.add_argument("--baseline", required=True)
    sub.add_parser("snapshot-path")
    sn = sub.add_parser("snapshot")
    sn.add_argument("--baseline", required=True)
    sn.add_argument("--snapshot")
    sn.add_argument("--box", required=True)
    for name in SNAPSHOT_COMMANDS[2:]:
        sp = sub.add_parser(name)
        sp.add_argument("--snapshot")
        if name == "restore-failed":
            sp.add_argument("--reason", required=True)
    a = p.parse_args(argv)

    if a.cmd in SNAPSHOT_COMMANDS:
        return _snapshot_main(a)

    if a.cmd == "decide":
        print(decide(a.present == "1", a.acked == "1", a.pinned == "1"))
        return EXIT_OK
    if a.cmd == "read-endpoints":
        print(" ".join(READ_ENDPOINTS))
        return EXIT_OK
    if a.cmd == "suggest":
        current = parse_read(sys.stdin.read())
        if current is None:
            print("unreadable camera REST read", file=sys.stderr)
            return EXIT_UNREADABLE
        print(json.dumps(suggest_baseline(current), sort_keys=True))
        return EXIT_OK

    try:
        baseline = _load_baseline_file(a.baseline)
    except (OSError, BaselineError) as e:
        print("invalid baseline %s: %s" % (a.baseline, e), file=sys.stderr)
        return EXIT_BAD_BASELINE

    if a.cmd == "status":
        print("pinned" if baseline_pinned(baseline) else "unpinned")
        return EXIT_OK

    current = parse_read(sys.stdin.read())
    if current is None:
        print("unreadable camera REST read (video/iso + video/shutter must answer 200 with values)",
              file=sys.stderr)
        return EXIT_UNREADABLE

    try:
        sets = plan_sets(current, baseline) if a.cmd == "plan" else None
        bad = grade_readback(current, baseline) if a.cmd == "grade" else None
    except ShutterUnitError as e:
        for line in _before_lines("BEFORE" if a.cmd == "plan" else "AFTER", current):
            print(line)
        print("the test camera's shutter unit is not pinned: %s" % e, file=sys.stderr)
        return EXIT_SHUTTER_UNIT

    if a.cmd == "plan":
        for line in _before_lines("BEFORE", current):
            print(line)
        for k, v in sets:
            print("SET %s %s -> %s" % (k, _fmt(k, current.get(k)), _fmt(k, v)))
        for ep, body in set_requests(sets):
            print("SETREQ %s %s" % (ep, body))
        return EXIT_OK

    # grade
    for line in _before_lines("AFTER", current):
        print(line)
    for k, want, got in bad:
        print("MISMATCH %s want=%s got=%s" % (k, _fmt(k, want), _fmt(k, got)))
    return EXIT_MISMATCH if bad else EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
