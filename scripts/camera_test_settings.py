#!/usr/bin/env python3
"""E2E test-camera shutter/ISO enforce -- the PURE decision half (issue 1371).

The ONE test camera (a BMPCC, fed through the HDMI splitter into every cambox) must start every
E2E run from the same exposure. Owner, 25.9.2026: "shutter a iso boli na zlych hodnotach tak tie
si mas uz ty vediet pri teste skontrolovat a nastavit". The E2E already pauses the bkshading relay
on the source cambox + cam2 (issue 808), so at [0/8] exactly one gphoto2 user can talk to the
camera over USB-PTP. The thin bash transport (`scripts/lib/camera-test-settings.sh`) runs gphoto2
over ssh and hands the raw text here; THIS module holds every decision:

  * which gphoto2 config keys are read/enforced -- the SAME names the relay transport uses
    (`bkshading/relay/src/transport.rs` `CORE_CONFIG_KEYS`, `bkshading/proto/src/read.rs`
    `plan_writes`; a Tier-0 test pins that every key here is one of the relay's);
  * the checked-in baseline (`scripts/camera-test-baseline.json`) -- load + validate;
  * parse the combined multi `--get-config` output (one END-terminated block per key, the relay's
    own `split_config_blocks` contract), diff vs the baseline, build the `--set-config` list;
  * grade the read-back after a set;
  * the presence/ack/pinned decision matrix (see `decide`).

Keys:
  REQUIRED  iso (ISO/gain), d002 (shutter ANGLE x100, the value the relay writes) -- the owner's
            shutter + ISO. Both must be pinned for the baseline to count as pinned.
  OPTIONAL  f-number, d004 (WB Kelvin), d005 (tint) -- null = read + logged only, a value = enforced.
            Aperture is NOT pinned by default: the cam1 BMPCC silently drops aperture PTP writes
            (issue 1343), so a pinned f-number would abort every run until that is solved.
  CONTEXT   d007 (project fps) -- read only to log the shutter as 1/N s; NEVER set (fps is the
            issue-809 grab-mode coupling, not a test-exposure setting).

CLI (used by the bash lib; every subcommand is pure: stdin/args in, stdout + exit code out):
  status  --baseline F                     -> "pinned" | "unpinned"         (exit 2 = invalid file)
  decide  --present 0|1 --acked 0|1 --pinned 0|1   -> the action word (see `decide`)
  read-args                                -> the gphoto2 argv for the ONE read session
  plan    --baseline F  < get-config text  -> BEFORE/SHUTTER/SET/SETARGS lines (exit 3 = unreadable)
  suggest < get-config text                -> a baseline JSON with the camera's CURRENT values
  grade   --baseline F  < get-config text  -> AFTER/MISMATCH lines (exit 5 = a set did not read back,
                                              exit 3 = unreadable)

The PRODUCTION-EXPOSURE snapshot (owner, 26.9.2026: "ked vypina sa development tak ze aj vratis iso
a uzavierku naspat"): right before the FIRST `--set-config` of a development period the E2E stores
the values it is about to overwrite (the owner's production ISO + shutter) on the runner; the
rig-mode EVENT switch restores them, reads them back and moves the file aside as consumed.
  snapshot-path                            -> the snapshot path ($CAMERA_PROD_EXPOSURE_SNAPSHOT, else
                                              ~/.camera-box/camera-prod-exposure.json)
  snapshot --baseline F --snapshot P --box B < get-config text
                                           -> SNAPSHOT saved|kept (written only when P is absent;
                                              exit 6 = a value could not be restored / write failed)
  restore-status --snapshot P              -> the one-line Slovak summary (exit 4 = no snapshot,
                                              exit 7 = invalid snapshot)
  restore-plan   --snapshot P < get-config -> NOW/RESTORE/RESTOREARGS lines (exit 3 / 7)
  restore-grade  --snapshot P < get-config -> AFTER/RESTORED/MISMATCH lines (exit 5 = not restored)
  consume        --snapshot P              -> moves P to <stem>.consumed-<UTC stamp>.json, never over an
                                              earlier one, and clears the restore-failed marker (exit 6)
  restore-failed --snapshot P --reason R   -> records a failed EVENT restore next to P
                                              (<stem>.restore-failed.json) for the handover check
  snapshot-state --snapshot P              -> ONE `exposure state=none|pending|restored|invalid ...`
                                              line for the development handover check (always exit 0)
Every `--snapshot` defaults to snapshot-path.
"""
import argparse
import datetime
import json
import os
import re
import sys

REQUIRED_KEYS = ("iso", "d002")
OPTIONAL_KEYS = ("f-number", "d004", "d005")
CONTEXT_KEYS = ("d007",)
ENFORCEABLE_KEYS = REQUIRED_KEYS + OPTIONAL_KEYS
READ_KEYS = ENFORCEABLE_KEYS + CONTEXT_KEYS

KEY_LABELS = {
    "iso": "ISO",
    "d002": "shutter angle x100",
    "f-number": "aperture",
    "d004": "white balance K",
    "d005": "tint",
    "d007": "project fps",
}

BASELINE_SCHEMA = 1
# Every value ends up as a word of a remote shell command (`gphoto2 --set-config key=value`), so
# only plain tokens are accepted -- a baseline value can never inject shell syntax.
SAFE_VALUE_RE = re.compile(r"^[A-Za-z0-9./_+-]+$")

EXIT_OK = 0
EXIT_BAD_BASELINE = 2
EXIT_UNREADABLE = 3
EXIT_MISMATCH = 5
EXIT_NO_SNAPSHOT = 4
EXIT_SNAPSHOT_FAILED = 6
EXIT_BAD_SNAPSHOT = 7

SNAPSHOT_SCHEMA = 1
SNAPSHOT_ENV = "CAMERA_PROD_EXPOSURE_SNAPSHOT"
SNAPSHOT_DIR = ".camera-box"
SNAPSHOT_BASENAME = "camera-prod-exposure.json"
CONSUMED_STAMP_RE = re.compile(r"^\d{8}T\d{6}Z(-\d+)?$")
# box label + UTC time are logged and parsed back as `key=value` words: plain tokens only
SNAPSHOT_FIELD_RE = re.compile(r"^[A-Za-z0-9._:+-]+$")

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


def _normalize(value):
    """A baseline/camera value as the exact string gphoto2 prints/accepts, or None."""
    if value is None:
        return None
    if isinstance(value, bool):
        raise BaselineError("a boolean is not a camera value: %r" % (value,))
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str):
        v = value.strip()
        if not v:
            raise BaselineError("an empty string is not a camera value (use null for 'not pinned')")
        return v
    raise BaselineError("unsupported value type %s: %r" % (type(value).__name__, value))


def load_baseline(text):
    """Parse + validate the baseline JSON text -> {key: str|None} over ENFORCEABLE_KEYS.

    Every enforceable key must be PRESENT (null = not pinned), no unknown key is allowed, and a
    value must be a plain safe token. Raises BaselineError on anything else."""
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
    out = {}
    for k in ENFORCEABLE_KEYS:
        v = _normalize(values[k])
        if v is not None and not SAFE_VALUE_RE.match(v):
            raise BaselineError("baseline value for %s is not a plain token: %r" % (k, v))
        out[k] = v
    return out


def baseline_pinned(baseline):
    """True when every REQUIRED key (shutter + ISO) has a value."""
    return all(baseline.get(k) is not None for k in REQUIRED_KEYS)


def pinned_keys(baseline):
    """The keys this run enforces: every key with a value, in ENFORCEABLE_KEYS order."""
    return [k for k in ENFORCEABLE_KEYS if baseline.get(k) is not None]


def parse_current(block):
    """The `Current:` value of one gphoto2 config block (mirrors bkshading-proto `parse_current`):
    the first `Current:` line, trimmed; None when absent or empty. libgphoto2's `(null)` (an
    f-number below the first enumerated stop, issue 1306) is also None -- never a value to pin."""
    for raw in block.splitlines():
        line = raw.strip()
        if line.startswith("Current:"):
            value = line[len("Current:"):].strip()
            if not value or value == "(null)":
                return None
            return value
    return None


def split_config_blocks(combined, n):
    """Split a multi `--get-config` stdout into per-key blocks at `END` lines (mirrors the relay's
    `split_config_blocks`, issue 1229). None unless exactly `n` END-terminated blocks were printed
    -- a key that errored mid-batch must never shift a value onto the wrong key."""
    blocks = []
    cur = []
    for raw in combined.splitlines():
        if raw.strip() == "END":
            blocks.append("\n".join(cur))
            cur = []
        else:
            cur.append(raw)
    return blocks if len(blocks) == n else None


def read_args():
    """The gphoto2 argv for ONE read session over READ_KEYS (the relay's coalesced-read shape)."""
    args = []
    for k in READ_KEYS:
        args += ["--get-config", k]
    return args


def parse_read(combined):
    """{key: current value | None} over READ_KEYS, or None when the output is unreadable (wrong
    block count, or a REQUIRED key with no `Current:` value)."""
    blocks = split_config_blocks(combined, len(READ_KEYS))
    if blocks is None:
        return None
    current = {k: parse_current(b) for k, b in zip(READ_KEYS, blocks)}
    if any(current[k] is None for k in REQUIRED_KEYS):
        return None
    return current


def plan_sets(current, baseline):
    """[(key, value)] for every pinned key whose current value differs from the baseline."""
    return [(k, baseline[k]) for k in pinned_keys(baseline) if current.get(k) != baseline[k]]


def grade_readback(readback, baseline):
    """[(key, want, got)] for every pinned key the camera does not report at the baseline value."""
    return [(k, baseline[k], readback.get(k)) for k in pinned_keys(baseline) if readback.get(k) != baseline[k]]


def shutter_denominator(d002, d007):
    """The shutter as a 1/N denominator (the relay's `convert_angle_or_denom`: round-half-up of
    360 * fps / angle), or None when either value is missing/non-numeric. Log context only."""
    try:
        angle100 = int(d002)
        fps = int(d007)
    except (TypeError, ValueError):
        return None
    if angle100 <= 0 or fps <= 0:
        return None
    return max(1, int(360.0 * fps * 100 / angle100 + 0.5))


def decide(present, acked, pinned):
    """The single decision matrix of the step.

    camera on USB:
      acked            -> abort-stale-ack   (the cambox-offline-ack contract: an ack for a thing
                                             that IS there must be removed, loudly)
      baseline null    -> abort-unpinned    (refuse loudly; the lib prints the values to pin)
      baseline pinned  -> enforce           (read, set what differs, read back, abort on mismatch)
    camera NOT on USB:
      acked            -> unverified-acked  (loud, report-only: operator says it is offline)
      baseline null    -> unverified-unpinned (loud, report-only: nothing to enforce yet -- the
                                             state until the supervisor pins the baseline, issue 1350)
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
    for k in READ_KEYS:
        v = current.get(k)
        lines.append("%s %s %s  (%s)" % (tag, k, v if v is not None else "-", KEY_LABELS[k]))
    denom = shutter_denominator(current.get("d002"), current.get("d007"))
    if denom is not None:
        lines.append("SHUTTER %s 1/%d s at %s fps" % (tag, denom, current.get("d007")))
    return lines


def suggest_baseline(current):
    """A baseline document pinning the camera's CURRENT shutter + ISO (optional keys left null --
    pinning them is a deliberate supervisor choice, see the module doc)."""
    values = {k: None for k in ENFORCEABLE_KEYS}
    for k in REQUIRED_KEYS:
        values[k] = current.get(k)
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


def build_snapshot(current, baseline, box, taken_utc):
    """The snapshot document: the camera's CURRENT value of every key this run enforces (the keys it
    may overwrite), plus the project fps as log context. A key the camera reports no value for is
    left out (there is nothing to put back). A value that is not a plain token could not be written
    back by the restore, so it raises SnapshotError: the caller refuses the set rather than
    overwrite a production value it cannot restore."""
    for field, v in (("box", box), ("taken_utc", taken_utc)):
        if not isinstance(v, str) or not SNAPSHOT_FIELD_RE.match(v):
            raise SnapshotError("snapshot %s %r is not a plain token, the restore could not read it back"
                                % (field, v))
    values = {}
    for k in pinned_keys(baseline):
        v = current.get(k)
        if v is None:
            continue
        if not SAFE_VALUE_RE.match(v):
            raise SnapshotError("the camera's current %s %r is not a plain token, so it could not be "
                                "restored" % (k, v))
        values[k] = v
    if not values:
        raise SnapshotError("the camera reported none of the enforced keys, nothing to snapshot")
    context = {k: current[k] for k in CONTEXT_KEYS
               if current.get(k) is not None and SAFE_VALUE_RE.match(current[k])}
    return {"schema": SNAPSHOT_SCHEMA, "box": box, "taken_utc": taken_utc, "values": values,
            "context": context}


def load_snapshot(text):
    """Parse + validate a snapshot document. Every value becomes a word of a remote
    `gphoto2 --set-config`, so only enforceable keys with plain-token string values pass."""
    try:
        doc = json.loads(text)
    except ValueError as e:
        raise SnapshotError("snapshot is not valid JSON: %s" % e)
    if not isinstance(doc, dict):
        raise SnapshotError("snapshot must be a JSON object")
    if doc.get("schema") != SNAPSHOT_SCHEMA:
        raise SnapshotError("snapshot schema must be %d, got %r" % (SNAPSHOT_SCHEMA, doc.get("schema")))
    for field in ("box", "taken_utc"):
        v = doc.get(field)
        if not isinstance(v, str) or not SNAPSHOT_FIELD_RE.match(v):
            raise SnapshotError("snapshot field %r must be a plain non-empty string, got %r" % (field, v))
    values = doc.get("values")
    if not isinstance(values, dict) or not values:
        raise SnapshotError("snapshot must carry a non-empty 'values' object")
    unknown = sorted(set(values) - set(ENFORCEABLE_KEYS))
    if unknown:
        raise SnapshotError("snapshot has key(s) %s that are never restored; allowed: %s"
                            % (unknown, list(ENFORCEABLE_KEYS)))
    for k, v in values.items():
        if not isinstance(v, str) or not SAFE_VALUE_RE.match(v):
            raise SnapshotError("snapshot value for %s is not a plain token string: %r" % (k, v))
    context = doc.get("context", {})
    if not isinstance(context, dict):
        raise SnapshotError("snapshot 'context' must be an object")
    return doc


def restore_plan(snapshot_values, current):
    """[(key, production value)] for every snapshot key the camera does not report at that value,
    in ENFORCEABLE_KEYS order."""
    return [(k, snapshot_values[k]) for k in ENFORCEABLE_KEYS
            if k in snapshot_values and current.get(k) != snapshot_values[k]]


def grade_restore(snapshot_values, readback):
    """[(key, want, got)] for every snapshot key that did not read back at its production value."""
    return [(k, snapshot_values[k], readback.get(k)) for k in ENFORCEABLE_KEYS
            if k in snapshot_values and readback.get(k) != snapshot_values[k]]


def _angle_text(d002):
    try:
        return ("%.2f" % (int(d002) / 100.0)).rstrip("0").rstrip(".")
    except (TypeError, ValueError):
        return str(d002)


def snapshot_summary(doc):
    """One plain Slovak line for the operator log and the EVENT Discord note, e.g.
    `ISO 800, uzávierka 1/60 s (uhol 360°), cam1 2026-09-26T15:00:00Z`."""
    values = doc["values"]
    parts = []
    if "iso" in values:
        parts.append("ISO %s" % values["iso"])
    if "d002" in values:
        denom = shutter_denominator(values["d002"], doc.get("context", {}).get("d007"))
        angle = _angle_text(values["d002"])
        if denom is not None:
            parts.append("uzávierka 1/%d s (uhol %s°)" % (denom, angle))
        else:
            parts.append("uzávierka uhol %s°" % angle)
    for k in OPTIONAL_KEYS:
        if k in values:
            parts.append("%s %s" % (KEY_LABELS[k], values[k]))
    return "%s, %s %s" % (", ".join(parts), doc["box"], doc["taken_utc"])


def _values_tokens(values):
    return " ".join("%s=%s" % (k, values[k]) for k in ENFORCEABLE_KEYS if k in values)


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


def newest_consumed(path, names):
    """The newest `<stem>.consumed-<stamp>.json` basename among `names`, or None."""
    found = [n for n in names if _consumed_stamp(path, n) is not None]
    return max(found) if found else None


def snapshot_state(path):
    """ONE line for the development handover check:
      exposure state=none
      exposure state=pending box=B taken=T iso=.. d002=..
      exposure state=restored restored=STAMP box=B taken=T iso=.. d002=..
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
    folder = os.path.dirname(path) or "."
    os.makedirs(folder, exist_ok=True)
    tmp = "%s.tmp-%d" % (path, os.getpid())
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(json.dumps(doc, sort_keys=True) + "\n")
        f.flush()
        os.fsync(f.fileno())
    try:
        os.link(tmp, path)
    except FileExistsError:
        return False
    finally:
        os.unlink(tmp)
    return True


def _load_snapshot_file(path):
    with open(path, encoding="utf-8") as f:
        return load_snapshot(f.read())


def _snapshot_kept(path):
    print("SNAPSHOT kept %s (a production exposure is already waiting for the EVENT restore)" % path)
    return EXIT_OK


def _snapshot_take(a, path):
    """`snapshot`: store the values the E2E is about to overwrite, only when none is waiting."""
    if os.path.exists(path):
        return _snapshot_kept(path)
    try:
        baseline = _load_baseline_file(a.baseline)
    except (OSError, BaselineError) as e:
        print("invalid baseline %s: %s" % (a.baseline, e), file=sys.stderr)
        return EXIT_BAD_BASELINE
    current = parse_read(sys.stdin.read())
    if current is None:
        print("unreadable gphoto2 output", file=sys.stderr)
        return EXIT_UNREADABLE
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
        return _snapshot_kept(path)
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
        print("unreadable gphoto2 output (expected %d END blocks with iso + d002 values)" % len(READ_KEYS),
              file=sys.stderr)
        return EXIT_UNREADABLE
    values = doc["values"]
    if a.cmd == "restore-plan":
        for line in _before_lines("NOW", current):
            print(line)
        sets = restore_plan(values, current)
        for k, v in sets:
            print("RESTORE %s %s -> %s" % (k, current.get(k) if current.get(k) is not None else "-", v))
        if sets:
            print("RESTOREARGS " + " ".join("--set-config %s=%s" % (k, v) for k, v in sets))
        return EXIT_OK
    for line in _before_lines("AFTER", current):
        print(line)
    bad = grade_restore(values, current)
    bad_keys = {k for k, _want, _got in bad}
    for k in ENFORCEABLE_KEYS:
        if k in values and k not in bad_keys:
            print("RESTORED %s %s" % (k, values[k]))
    for k, want, got in bad:
        print("MISMATCH %s want=%s got=%s" % (k, want, got if got is not None else "-"))
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
    sub.add_parser("read-args")
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
    if a.cmd == "read-args":
        print(" ".join(read_args()))
        return EXIT_OK
    if a.cmd == "suggest":
        current = parse_read(sys.stdin.read())
        if current is None:
            print("unreadable gphoto2 output", file=sys.stderr)
            return EXIT_UNREADABLE
        doc = suggest_baseline(current)
        # A camera label the baseline would refuse (a space, a symbol) must be said here, not
        # discovered when the supervisor pins it.
        for k in REQUIRED_KEYS:
            v = doc["values"][k]
            if not SAFE_VALUE_RE.match(v):
                print("UNPINNABLE %s %r: not a plain token, the baseline would refuse it" % (k, v), file=sys.stderr)
        print(json.dumps(doc, sort_keys=True))
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
        print("unreadable gphoto2 output (expected %d END blocks with iso + d002 values)" % len(READ_KEYS),
              file=sys.stderr)
        return EXIT_UNREADABLE

    if a.cmd == "plan":
        for line in _before_lines("BEFORE", current):
            print(line)
        sets = plan_sets(current, baseline)
        for k, v in sets:
            print("SET %s %s -> %s" % (k, current.get(k) if current.get(k) is not None else "-", v))
        if sets:
            print("SETARGS " + " ".join("--set-config %s=%s" % (k, v) for k, v in sets))
        return EXIT_OK

    # grade
    for line in _before_lines("AFTER", current):
        print(line)
    bad = grade_readback(current, baseline)
    for k, want, got in bad:
        print("MISMATCH %s want=%s got=%s" % (k, want, got if got is not None else "-"))
    return EXIT_MISMATCH if bad else EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
