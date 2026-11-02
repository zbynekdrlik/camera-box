"""issue 1387 slice 3 (design 6558387590): the dev1 frozen-input watchdog follows the resolver.

scripts/frozen-input-alert-watchdog.sh watches the genlock-fifo `received=` counter of the inputs that
carry the picture. Its stream instance (issue 1052) watched the literal `NDI 2ME PGM`, its strih
instance (issue 1069) the camera inputs its log enumeration finds. On the JPEG XS venue the on-air
inputs are `JXS PGM src` on stream and `JXS camN src` on strih; the JPEG XS source writes the same
`genlock-fifo audit '<input>'` lines, so the tap is unchanged -- only WHICH inputs it watches.

The names come from the ONE resolver (scripts/lib/program-inputs.sh program_inputs_resolve +
camera-set.sh camera_strih_input), never a literal:
  * stream instance: on the JPEG XS path the default watched source is the resolved stream program
    input; an explicit FROZEN_INPUT_SOURCES still wins;
  * strih instance (enumerate mode): every JPEG XS camera input the facts declare is ALWAYS watched,
    beside whatever the log enumeration finds -- a declared input whose audit line is missing then
    reaches the tap-broken WARN instead of silently never being watched;
  * an NDI venue resolves nothing: both instances behave exactly as before; a resolve the facts refuse
    is a WARNING and the NDI defaults stay (a watchdog never stops watching).
The dedup key, the confirm counter and the SKIP gate are unchanged.

Tier-0: the real watchdog with stubbed I/O (the *_CMD seams, a stub notify), the committed fact files.
"""
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
WATCHDOG = _ROOT / "scripts" / "frozen-input-alert-watchdog.sh"
HEALTH = _ROOT / "scripts" / "lib" / "frozen-input-health.sh"

_SCRUB = ("E2E_PROGRAM_PATH", "CAMERA_STRIH_JXS_SET", "STRIH_PROG_SOURCE", "STREAM_PROG_SOURCE", "OBS_FLEET",
          "STRIH_BOXES_DIR", "CAMERA_ACTIVE_SET", "FROZEN_INPUT_SOURCES", "FROZEN_INPUT_ENUMERATE",
          "FROZEN_INPUT_RECEIVER", "FROZEN_INPUT_SENDER", "FROZEN_INPUT_STRIH_HOST", "FROZEN_INPUT_STREAM_HOST",
          "FROZEN_INPUT_PROBE_CMD", "FROZEN_INPUT_ENUMERATE_CMD", "FROZEN_INPUT_ALERT_STATE_FILE")

# a probe stub: logs the source it is asked for, prints a `genlock-fifo audit` line; every source named
# in $FROZEN_SOURCES keeps a constant counter (frozen), every other one grows each call (advancing)
PROBE = r"""#!/usr/bin/env bash
src="$2"
printf '%s\n' "$src" >> "$STUB_STATE/probed.log"
key=$(printf '%s' "$src" | tr -c 'A-Za-z0-9' '_')
cf="$STUB_STATE/cnt_$key"
n=$(cat "$cf" 2>/dev/null || echo 0); n=$((n+1)); printf '%s' "$n" > "$cf"
case ";${FROZEN_SOURCES:-};" in *";$src;"*) rc=5000 ;; *) rc=$((5000 + n)) ;; esac
printf "12:00:00.000: genlock-fifo audit '%s': received=%s consumed=1 locked=1\n" "$src" "$rc"
"""

NOTIFY = """#!/usr/bin/env python3
import os, sys
with open(os.environ["STUB_NOTIFY_LOG"], "a") as f:
    f.write("---NOTIFY---\\n" + "\\n".join(sys.argv[1:]) + "\\n")
"""


def _exe(path, body):
    path.write_text(body)
    path.chmod(0o755)
    return path


class Rig:
    def __init__(self, tmp_path, enumerate_log=""):
        self.dir = tmp_path
        self.state = tmp_path / "state"
        self.state.mkdir()
        self.probe = _exe(tmp_path / "probe.sh", PROBE)
        self.enum = _exe(tmp_path / "enumerate.sh", f"#!/usr/bin/env bash\ncat <<'LOG'\n{enumerate_log}LOG\n")
        self.notify = _exe(tmp_path / "notify.py", NOTIFY)
        self.notify_log = tmp_path / "notify.log"
        self.notify_log.write_text("")

    def run(self, env=None):
        e = {k: v for k, v in os.environ.items() if k not in _SCRUB}
        e.update({"FROZEN_INPUT_PROBE_CMD": str(self.probe), "FROZEN_INPUT_ENUMERATE_CMD": str(self.enum),
                  "AIRULESET_NOTIFY": str(self.notify), "STUB_NOTIFY_LOG": str(self.notify_log),
                  "STUB_STATE": str(self.state), "FROZEN_INPUT_ALERT_STATE_DIR": str(self.state),
                  "FROZEN_INPUT_ALERT_STATE_FILE": str(self.state / "wd.state"),
                  "FROZEN_INPUT_NETREACH_STATE_FILE": str(self.state / "netreach.state"),
                  "FROZEN_INPUT_ALERT_CONFIRM_THRESHOLD": "1"})
        e.update(env or {})
        r = subprocess.run(["bash", str(WATCHDOG)], capture_output=True, text=True, env=e, timeout=120,
                           cwd=str(_ROOT))
        assert r.returncode == 0, r.stdout + r.stderr
        return r

    def probed(self):
        p = self.state / "probed.log"
        return p.read_text().splitlines() if p.exists() else []


STREAM_SNV = {"FROZEN_INPUT_RECEIVER": "stream|10.77.9.204", "FROZEN_INPUT_SENDER": "strih"}
STRIH_SNV = {"FROZEN_INPUT_RECEIVER": "strih|10.77.9.202", "FROZEN_INPUT_SENDER": "strih",
             "FROZEN_INPUT_ENUMERATE": "1", "FROZEN_INPUT_ALERT_TAG": "#1069"}
NDI_VENUE = {"FROZEN_INPUT_STRIH_HOST": "192.0.2.10", "FROZEN_INPUT_STREAM_HOST": "192.0.2.11"}
STRIH_LOG = ("12:00:00.000: genlock-fifo audit 'NDI cam3': received=10 consumed=1 locked=1\n"
             "12:00:00.010: genlock-fifo audit 'NDI 2ME PGM (mv)': received=10 consumed=1 locked=1\n")


# --- the pure merge ------------------------------------------------------------------------------


def _health(body, *args):
    r = subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{HEALTH}"\n{body}', "harness", *args],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_watch_list_puts_the_declared_inputs_first_and_never_twice():
    out = _health('frozen_input_watch_list "$1" "$2"', "JXS cam2 src;JXS cam5 src", "NDI cam3;JXS cam5 src;NDI cam5")
    assert out == "JXS cam2 src;JXS cam5 src;NDI cam3;NDI cam5"


def test_the_watch_list_of_nothing_is_empty_and_blanks_are_dropped():
    assert _health('frozen_input_watch_list "" ""') == ""
    assert _health('frozen_input_watch_list "$1" "$2"', ";;", "NDI cam3; ;") == "NDI cam3"
    assert _health('frozen_input_watch_list "$1"', "JXS PGM src") == "JXS PGM src"


# --- the stream instance -------------------------------------------------------------------------


def test_the_stream_instance_watches_the_resolved_jpeg_xs_program_on_the_snv_venue(tmp_path):
    rig = Rig(tmp_path)
    r = rig.run(STREAM_SNV)
    assert rig.probed() == ["JXS PGM src"], r.stderr
    assert "issue 1387" in r.stderr and "JXS PGM src" in r.stderr


def test_the_stream_instance_pages_a_frozen_jpeg_xs_program_with_the_unchanged_dedup_key(tmp_path):
    rig = Rig(tmp_path)
    env = dict(STREAM_SNV, FROZEN_SOURCES="JXS PGM src")
    rig.run(env)
    rig.run(env)
    log = rig.notify_log.read_text()
    assert "JXS PGM src: received=5000 not advancing" in log
    assert "frozen-input-stream-JXS PGM src" in log


def test_the_stream_instance_keeps_ndi_2me_pgm_on_an_ndi_venue(tmp_path):
    rig = Rig(tmp_path)
    rig.run(dict(STREAM_SNV, **NDI_VENUE))
    assert rig.probed() == ["NDI 2ME PGM"]


def test_an_explicit_source_list_still_wins_on_the_jpeg_xs_venue(tmp_path):
    rig = Rig(tmp_path)
    rig.run(dict(STREAM_SNV, FROZEN_INPUT_SOURCES="NDI 2ME PGM;JXS PGM src"))
    assert rig.probed() == ["NDI 2ME PGM", "JXS PGM src"]


def test_a_resolve_the_facts_refuse_warns_and_keeps_the_ndi_default(tmp_path):
    rig = Rig(tmp_path)
    r = rig.run(dict(STREAM_SNV, E2E_PROGRAM_PATH="ndi"))
    assert rig.probed() == ["NDI 2ME PGM"]
    assert "WARNING" in r.stderr and "REFUSED" in r.stderr


# --- the strih instance (enumerate mode) ---------------------------------------------------------


def test_the_strih_instance_always_watches_every_declared_jpeg_xs_camera_beside_the_enumeration(tmp_path):
    rig = Rig(tmp_path, STRIH_LOG)
    rig.run(STRIH_SNV)
    assert rig.probed() == ["JXS cam2 src", "JXS cam3 src", "JXS cam5 src", "NDI cam3"]


def test_the_strih_instance_pages_a_frozen_jpeg_xs_camera_its_log_never_enumerated(tmp_path):
    rig = Rig(tmp_path, STRIH_LOG)
    env = dict(STRIH_SNV, FROZEN_SOURCES="JXS cam5 src")
    rig.run(env)
    rig.run(env)
    log = rig.notify_log.read_text()
    assert "#1069" in log and "JXS cam5 src: received=5000 not advancing" in log
    for other in ("JXS cam2 src: received", "JXS cam3 src: received", "NDI cam3: received"):
        assert other not in log


def test_the_strih_instance_on_an_ndi_venue_watches_only_what_it_enumerates(tmp_path):
    rig = Rig(tmp_path, STRIH_LOG)
    rig.run(dict(STRIH_SNV, **NDI_VENUE))
    assert rig.probed() == ["NDI cam3"]


def test_a_box_issue_1001_holds_down_reads_nothing_and_enumerates_nothing(tmp_path):
    rig = Rig(tmp_path, STRIH_LOG)
    (rig.state / "netreach.state").write_text("alerted_strih=1\n")
    r = rig.run(STRIH_SNV)
    assert rig.probed() == [], r.stderr
