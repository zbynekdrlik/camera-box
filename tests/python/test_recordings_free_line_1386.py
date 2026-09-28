"""issue 1386 item 4 -- recording-e2e.sh reads its recordings-volume free-space line through the ONE
reader bundle_state_gather.recordings_free_line (via scripts/lib/recordings-free-line.sh), not an
inline python copy.

Pins: the helper prints exactly the reader's line for every body, under the E2E harness's own
`set -euo pipefail`; recording-e2e.sh sources the lib and its check_recordings_free_space calls the
helper with no inline `python3 -c` left; and the check, extracted verbatim from recording-e2e.sh and
run against a PATH curl stub, still prints the OK line / the loud WARNING #652 / the skip NOTE.
Tier-0: python + bash subprocesses, no cargo.
"""
import json
import os
import pathlib
import stat
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_LIB = _SCRIPTS / "lib" / "recordings-free-line.sh"
_E2E = _SCRIPTS / "recording-e2e.sh"
sys.path.insert(0, str(_SCRIPTS))
import bundle_state_gather as bsg  # noqa: E402

BODIES = [
    json.dumps({"total_bytes": 1, "file_count": 1, "oldest_mtime": 1.7e9, "free_bytes": 619 * 10**9}),
    json.dumps({"free_bytes": 50 * 10**9}),
    json.dumps({"free_bytes": 50 * 10**9 - 1}),
    json.dumps({"free_bytes": 0}),
    json.dumps({"free_bytes": None}),
    json.dumps({"total_bytes": 0}),
    json.dumps({"free_bytes": True}),
    json.dumps({"free_bytes": "abc"}),
    "[1]", "not json", "",
]


def _bash(tmp_path, body, path_prefix=None):
    sf = tmp_path / "case.sh"
    sf.write_text("set -euo pipefail\n" + body)
    env = dict(os.environ)
    if path_prefix:
        env["PATH"] = f"{path_prefix}:{env['PATH']}"
    return subprocess.run(["bash", str(sf)], capture_output=True, text=True, env=env, timeout=60)


@pytest.mark.parametrize("body", BODIES)
def test_helper_prints_exactly_the_one_readers_line(tmp_path, body):
    (tmp_path / "body").write_text(body)
    r = _bash(tmp_path, f'. "{_LIB}"\nout="$(recordings_free_line_from_stats "$(cat "{tmp_path}/body")" 50 "{_SCRIPTS}")"\n'
              'printf "%s\\n" "$out"\n')
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == bsg.recordings_free_line(body, 50)


def test_helper_fails_only_when_python_cannot_run_the_reader(tmp_path):
    r = _bash(tmp_path, f'. "{_LIB}"\nrecordings_free_line_from_stats "{{}}" abc "{_SCRIPTS}" && echo RC0 || echo RC1\n')
    assert r.stdout.strip() == "RC1", "a non-numeric threshold must fail, so the caller keeps its skip NOTE"


def _check_fn():
    s = _E2E.read_text()
    a = s.index("check_recordings_free_space() {")
    return s[a:s.index("\n}\n", a) + 3]


def test_recording_e2e_sources_the_lib_and_keeps_no_inline_reader():
    s = _E2E.read_text()
    assert s.count('. "$HERE/lib/recordings-free-line.sh"') == 1
    assert s.index('. "$HERE/lib/recordings-free-line.sh"') < s.index("check_recordings_free_space() {")
    fn = _check_fn()
    assert 'recordings_free_line_from_stats "$stats" "$RECORDINGS_FREE_MIN_GB" "$HERE"' in fn
    assert "python3 -c" not in fn, "the inline reader copy is back -- use the one reader via the lib"
    assert "recordings_free_verdict" not in fn


def _run_check(tmp_path, body, curl_ok=True):
    (tmp_path / "body").write_text(body)
    curl = tmp_path / "curl"
    curl.write_text("#!/bin/sh\n" + (f'cat "{tmp_path}/body"\n' if curl_ok else "exit 7\n"))
    curl.chmod(curl.stat().st_mode | stat.S_IXUSR)
    return _bash(tmp_path, f'HERE="{_SCRIPTS}"\nWIN_BUNDLE_STATE_PORT=8899\nRECORDINGS_FREE_MIN_GB=50\n'
                 f'. "{_LIB}"\n' + _check_fn() + 'check_recordings_free_space strih 10.0.0.1\necho "RC=$?"\n',
                 path_prefix=str(tmp_path))


def test_check_prints_the_ok_line(tmp_path):
    r = _run_check(tmp_path, json.dumps({"free_bytes": 619 * 10**9}))
    assert "strih recordings volume: 619.0 GB free (warn at <= 50 GB free)" in r.stdout
    assert "RC=0" in r.stdout and "WARNING" not in r.stderr


def test_check_warns_loudly_below_the_threshold(tmp_path):
    r = _run_check(tmp_path, json.dumps({"free_bytes": 40 * 10**9}))
    assert "strih recordings volume: 40.0 GB free" in r.stdout
    assert "WARNING #652 #1276: strih's OBS recordings volume has only ~40.0 GB" in r.stderr
    assert "RC=0" in r.stdout


@pytest.mark.parametrize("body", [json.dumps({"free_bytes": None}), json.dumps({"free_bytes": True}), "[1]", ""])
def test_check_never_warns_on_an_unreadable_free_space(tmp_path, body):
    r = _run_check(tmp_path, body)
    assert "WARNING" not in r.stderr
    assert "unreadable/unparseable" in r.stderr and "RC=0" in r.stdout


def test_check_skips_when_the_server_is_unreachable(tmp_path):
    r = _run_check(tmp_path, "", curl_ok=False)
    assert "could not fetch strih recordings-dir stats" in r.stderr and "RC=0" in r.stdout


# -- the 8 h soak reads the same helper (issue 1386 slice C) ---------------------------------------
_SOAK = _SCRIPTS / "av-soak.sh"
_SOAK_LIB = _SCRIPTS / "lib" / "av-soak.sh"


def _soak_fn():
    s = _SOAK_LIB.read_text()
    a = s.index("av_soak_free_space_verdict() {")
    return s[a:s.index("\n}\n", a) + 3]


def test_the_soak_sources_the_helper_and_keeps_no_inline_reader():
    s = _SOAK.read_text()
    src = '. "$HERE/lib/recordings-free-line.sh"'
    assert s.count(src) == 1
    assert s.index(src) < s.index('. "$HERE/lib/av-soak.sh"')
    fn = _soak_fn()
    assert 'recordings_free_line_from_stats "$stats" "$min_gb" "$here"' in fn
    assert "python3" not in fn, "the inline reader copy is back in the soak -- use the helper"


def _run_soak(tmp_path, body, min_gb="50", curl_ok=True):
    (tmp_path / "body").write_text(body)
    curl = tmp_path / "curl"
    curl.write_text("#!/bin/sh\n" + (f'cat "{tmp_path}/body"\n' if curl_ok else "exit 7\n"))
    curl.chmod(curl.stat().st_mode | stat.S_IXUSR)
    return _bash(tmp_path, f'. "{_LIB}"\n. "{_SOAK_LIB}"\n'
                 f'av_soak_free_space_verdict 10.0.0.1 8899 {min_gb} "{_SCRIPTS}"\necho "RC=$?"\n',
                 path_prefix=str(tmp_path))


@pytest.mark.parametrize("body", BODIES)
def test_the_soak_verdict_is_the_readers_line_for_every_body(tmp_path, body):
    r = _run_soak(tmp_path, body)
    assert r.returncode == 0, r.stderr
    lines = r.stdout.strip().split("\n")
    assert lines == [bsg.recordings_free_line(body, 50), "RC=0"]


def test_the_soak_verdict_is_unknown_when_the_box_or_the_reader_fails(tmp_path):
    assert _run_soak(tmp_path, "", curl_ok=False).stdout.split() == ["UNKNOWN", "-1", "RC=0"]
    body = json.dumps({"free_bytes": 40 * 10**9})
    assert _run_soak(tmp_path, body, min_gb="abc").stdout.split() == ["UNKNOWN", "-1", "RC=0"]
