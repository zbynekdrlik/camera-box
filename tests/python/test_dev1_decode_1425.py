"""issue 1425 (owner ROZHODNUTÉ issuecomment-7187562277, 26.11.2026): the full-path E2E's recording
decode + recording-verdict run on dev1, NEVER on a rig box.

scripts/recording-verdict-on-strih-snv.sh (the strih [8/8a] decode, which the stream box execs for
[8/8b]) copies the recording PACED + sha256-verified to dev1 (scripts/lib/rec-pull-paced.sh), runs the
SAME recording-verdict arguments locally with the dev1 binary, writes the partial (+ its pixel dir)
where the callers read it, and deletes the copy on every exit. The missing-slot pixel proofs export
from a dev1 copy too. RUN against the fake box of tests/python/stream_box_fakes_1462.py: the box's
remote bash is really run on a stub PATH, so "nothing decodes on the box" is checked by what ran.
Tier-0: no rig, no network, no cargo.
"""
import hashlib
import json
import os
import pathlib
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stream_box_fakes_1462 import calls, ffmpeg_calls, link_box_tools, make_rig  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
DECODE = SCRIPTS / "recording-verdict-on-strih-snv.sh"
STREAM_WRAPPER = SCRIPTS / "recording-verdict-on-stream-snv.sh"
LIB = SCRIPTS / "lib" / "rec-pull-paced.sh"
MSP = SCRIPTS / "lib" / "missing-slot-pixels.sh"

# The dev1 recording-verdict: logs its argv + whether the --strih/--stream recording it was handed is
# readable (and its sha256), writes the --out partial (+ a pixel dir with FAKE_VERDICT_PIXELS), exits
# FAKE_VERDICT_RC.
_VERDICT = r'''
import hashlib, json, os, sys
a = sys.argv[1:]
rec = None
for k in ("--strih", "--stream"):
    if k in a:
        rec = a[a.index(k) + 1]
entry = {"argv": a, "rec": rec}
if rec and os.path.isfile(rec):
    entry["rec_sha"] = hashlib.sha256(open(rec, "rb").read()).hexdigest()
for k in ("--av-marker-log", "--switch-schedule"):
    if k in a:
        p = a[a.index(k) + 1]
        entry[k] = open(p).read() if os.path.isfile(p) else None
with open(os.environ["FAKE_VERDICT_LOG"], "a") as f:
    f.write(json.dumps(entry) + "\n")
rc = int(os.environ.get("FAKE_VERDICT_RC", "0"))
if rc:
    sys.exit(rc)
out = a[a.index("--out") + 1]
with open(out, "w") as f:
    json.dump({"argv": a}, f)
if os.environ.get("FAKE_VERDICT_PIXELS"):
    d = out[:-len(".json")] + "-pixels"
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "frame-12.png"), "wb") as f:
        f.write(b"\x89PNG-fake")
'''

BOX_TOOLS = ("sha256sum", "stat", "cut", "cat", "head", "tr", "grep", "nice", "taskset", "env", "basename", "mkdir")

# A box sha256sum that lies (a stale / corrupted copy must never be decoded).
_LYING_SHA = '#!/bin/sh\necho "0000000000000000000000000000000000000000000000000000000000000000  $2"\n'


def _rig(tmp_path, lying_sha=False):
    rig = make_rig(tmp_path)
    if lying_sha:
        p = rig["box"] / "sha256sum"
        p.write_text(_LYING_SHA)
        p.chmod(0o755)
    link_box_tools(rig, *BOX_TOOLS)
    verdict = tmp_path / "dev1-recording-verdict"
    verdict.write_text("#!%s\n%s" % (sys.executable, _VERDICT))
    verdict.chmod(0o755)
    rig["verdict"] = verdict
    rig["verdict_log"] = tmp_path / "verdict.log"
    rig["env"]["FAKE_VERDICT_LOG"] = str(rig["verdict_log"])
    rig["local_out"] = tmp_path / "dev1-out"
    rig["remote_out"] = rig["home"] / "verdict-out"
    rig["remote_out"].mkdir()
    rec = rig["home"] / "2026-11-26 09-24-08.mkv"
    rec.write_bytes(os.urandom(300_000))
    rig["rec"] = rec
    return rig


def _decode(rig, *extra_pass, env=None, script=DECODE, rec_flag="--strih-rec", kind="strih"):
    e = dict(rig["env"])
    e.update({"STRIH_LX_BOX": "10.77.9.202", "STREAM_LX_BOX": "10.77.9.204"})
    e.update(env or {})
    rec = str(rig["rec"])
    args = ["bash", str(script), "--verdict-bin", str(rig["verdict"]), "--remote-bin", str(rig["home"] / "rv"),
            "--out-dir", str(rig["remote_out"]), "--local-out-dir", str(rig["local_out"]), rec_flag, rec,
            "--", "--extract-partial", kind, "--" + kind, rec, "--capture-fps", "30", *extra_pass,
            "--out", str(rig["remote_out"] / ("%s-partial-7.json" % kind))]
    return subprocess.run(args, capture_output=True, text=True, env=e, cwd=str(REPO), timeout=120)


def _verdicts(rig):
    p = rig["verdict_log"]
    return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []


def _box_texts(rig):
    return [c["remote"] for c in calls(rig) if c["tool"] == "ssh"]


def _copies_left(rig):
    return sorted(str(p) for p in rig["local_out"].rglob("*.mkv")) if rig["local_out"].exists() else []


def test_the_strih_recording_is_decoded_on_dev1_from_a_verified_copy(tmp_path):
    rig = _rig(tmp_path)
    p = _decode(rig, env={"FAKE_VERDICT_PIXELS": "1"})
    assert p.returncode == 0, p.stdout + p.stderr
    (v,) = _verdicts(rig)
    # the dev1 verdict got a byte-identical COPY, never the box path
    assert v["rec"] != str(rig["rec"]) and v["rec_sha"] == hashlib.sha256(rig["rec"].read_bytes()).hexdigest()
    assert v["argv"][v["argv"].index("--extract-partial") + 1] == "strih"
    assert v["argv"][v["argv"].index("--out") + 1] == str(rig["local_out"] / "strih-partial-7.json")
    # the partial + its pixel dir sit where the callers read them
    assert (rig["local_out"] / "strih-partial-7.json").is_file()
    assert (rig["local_out"] / "strih-partial-7-pixels" / "frame-12.png").read_bytes() == b"\x89PNG-fake"
    # nothing decoded on the box: no remote text names the verdict, ffmpeg or a binary upload
    for t in _box_texts(rig):
        assert "recording-verdict" not in t and "--extract-partial" not in t and "ffmpeg" not in t, t
    assert not [c for c in calls(rig) if c["tool"] == "scp" and ":" in c["dst"] and "@" in c["dst"]], "no upload"
    assert ffmpeg_calls(rig) == []
    # the copy is paced, logged, and gone afterwards
    (cp,) = [c for c in calls(rig) if c["tool"] == "scp" and c["src"].endswith(".mkv")]
    assert cp["argv"][cp["argv"].index("-l") + 1] == "200000", cp
    assert "300000 B in" in p.stdout and "sha256" in p.stdout and "match" in p.stdout, p.stdout
    assert "decode time on dev1" in p.stdout, p.stdout
    assert _copies_left(rig) == []


def test_the_pacing_rate_is_configurable_and_a_bad_value_keeps_200(tmp_path):
    for value, want in (("50", "50000"), ("0", "200000"), ("1e3", "200000"), ("", "200000")):
        sub = tmp_path / ("r" + (value or "empty"))
        sub.mkdir()
        rig = _rig(sub)
        p = _decode(rig, env={"REC_PULL_RATE_MBIT": value})
        assert p.returncode == 0, p.stdout + p.stderr
        (cp,) = [c for c in calls(rig) if c["tool"] == "scp" and c["src"].endswith(".mkv")]
        assert cp["argv"][cp["argv"].index("-l") + 1] == want, (value, cp)


def test_a_copy_that_does_not_match_the_box_is_never_decoded(tmp_path):
    rig = _rig(tmp_path, lying_sha=True)
    p = _decode(rig)
    assert p.returncode != 0, p.stdout + p.stderr
    assert "does not match the box" in p.stderr, p.stderr
    assert _verdicts(rig) == [] and _copies_left(rig) == []
    assert not (rig["local_out"] / "strih-partial-7.json").exists()


def test_a_failed_decode_fails_the_leg_and_still_removes_the_copy(tmp_path):
    rig = _rig(tmp_path)
    p = _decode(rig, env={"FAKE_VERDICT_RC": "3"})
    assert p.returncode == 3, p.stdout + p.stderr
    assert len(_verdicts(rig)) == 1 and _copies_left(rig) == []
    assert not (rig["local_out"] / "strih-partial-7.json").exists()


def test_an_unreachable_box_fails_before_any_decode(tmp_path):
    rig = _rig(tmp_path)
    p = _decode(rig, env={"FAKE_SSH_DOWN": "1"})
    assert p.returncode != 0 and _verdicts(rig) == [], p.stdout + p.stderr
    assert _copies_left(rig) == []


def test_the_stream_box_decode_reads_the_pushed_marker_log_from_dev1(tmp_path):
    rig = _rig(tmp_path)
    marker = rig["remote_out"] / "av-markers-7.csv"
    marker.write_text("t,idx\n1,2\n")
    p = _decode(rig, "--av-marker-log", str(marker), script=STREAM_WRAPPER, rec_flag="--stream-rec", kind="stream")
    assert p.returncode == 0, p.stdout + p.stderr
    (v,) = _verdicts(rig)
    assert v["rec_sha"] == hashlib.sha256(rig["rec"].read_bytes()).hexdigest()
    assert v["--av-marker-log"] == "t,idx\n1,2\n"
    assert v["argv"][v["argv"].index("--av-marker-log") + 1] != str(marker), "a dev1 path, not the box one"
    assert (rig["local_out"] / "stream-partial-7.json").is_file()
    assert "stream-snv" in p.stdout and _copies_left(rig) == []
    for t in _box_texts(rig):
        assert "--extract-partial" not in t and "ffmpeg" not in t, t


def test_the_verdict_bin_is_required_on_dev1(tmp_path):
    rig = _rig(tmp_path)
    rig["verdict"] = tmp_path / "missing-binary"
    p = _decode(rig)
    assert p.returncode == 2 and "--verdict-bin" in p.stderr, p.stdout + p.stderr
    assert calls(rig) == []


def test_the_box_stat_text_quotes_a_spaced_path_and_runs_behind_the_prefix():
    r = subprocess.run(["bash", "-c", '. "$1"; rec_pull_box_stat_cmd "/srv/_REC/a b.mkv" \'LP="nice -n 19";\'',
                        "h", str(LIB)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0
    assert r.stdout.startswith("F=/srv/_REC/a\\ b.mkv; ") and '$LP sha256sum -- "$F"' in r.stdout, r.stdout


# --- the missing-slot pixel proofs: exported from a dev1 copy, never on the box ---------------------

def _msp(tmp_path, rig, box_os="linux"):
    rec = rig["home"] / "strih rec.mkv"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=30", "-t", "1",
                    "-pix_fmt", "yuv420p", str(rec)], check=True, timeout=60)
    out = tmp_path / "msp-out"
    out.mkdir()
    report = out / "verdict-77.json"
    report.write_text('{"overall_pass": true, "full_chain": {"loss": {"cam3": {"classified": '
                      '[{"id": 1, "kind": "burn_unreadable", "frame_index": 12, "png": null}]}}}}')
    text = ('set -euo pipefail\n. "%s"\n'
            "missing_slot_pixels_run '%s' '%s' 77 10.77.9.202 %s '%s' 10.77.9.204 '' 'C:\\o' || true\n"
            "echo MSP-DONE\n") % (MSP, report, out, box_os, rec)
    drv = tmp_path / "msp-driver.sh"
    drv.write_text(text)
    e = dict(rig["env"], STRIH_LX_REMOTE_OUT_DIR=str(rig["remote_out"]))
    p = subprocess.run(["bash", str(drv)], capture_output=True, text=True, env=e, cwd=str(REPO), timeout=180)
    return p, out, report


def test_the_pixel_proofs_are_exported_on_dev1_from_a_copy(tmp_path):
    rig = _rig(tmp_path)
    p, out, report = _msp(tmp_path, rig)
    assert p.returncode == 0 and "MSP-DONE" in p.stdout, p.stdout + p.stderr
    for k in (11, 12, 13):
        assert (out / "cam3-missing" / ("frame-%d.png" % k)).is_file(), p.stdout + p.stderr
    assert '"strih": "ok"' in report.read_text()
    # the box ran no ffmpeg (its stub logs every call) and no remote text names it
    assert ffmpeg_calls(rig) == []
    for t in _box_texts(rig):
        assert "ffmpeg" not in t and "ffprobe" not in t, t
    # the recording copy is gone, only the PNGs stay
    assert not list(out.rglob("*.mkv")), list(out.rglob("*"))
