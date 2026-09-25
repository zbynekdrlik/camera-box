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
"""
import argparse
import json
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

# decide() actions
ENFORCE = "enforce"
ABORT_UNPINNED = "abort-unpinned"
ABORT_STALE_ACK = "abort-stale-ack"
ABORT_ABSENT = "abort-absent"
UNVERIFIED_ACKED = "unverified-acked"
UNVERIFIED_UNPINNED = "unverified-unpinned"


class BaselineError(ValueError):
    """The baseline file is malformed (never silently treated as 'unpinned')."""


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
    a = p.parse_args(argv)

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
