"""#1324 -- the marker-DECODABILITY preflight (scripts/lib/marker-decodability-preflight.sh) + the
recording-e2e.sh [4b3/8] wiring. The honest sibling of the #1323 level ceiling: the floor proves
NOT-silent and the ceiling proves NOT-flooded, but neither can tell a DECODABLE QPSK marker from a
chain at a plausible level whose marker is not decodable (drowned / off-axis mic / wrong Dante
channel / format mismatch). Two Tier-0 layers (no rig, no ssh, no cargo):

  1. the pure decision lib -- default thresholds, the ffmpeg extract / delete PowerShell builders,
     the JSON field parse, the class-named abort messages;
  2. recording-e2e.sh actually WIRES a [4b3/8] step that sources the lib, runs the probe from
     $PROBE_BIN_DIR, aborts naming the class on any non-OK verdict, and skips ONLY when the
     probe-tools artifact is absent (a loud UNVERIFIED, never a silent pass) -- a static read of
     the shell script, the SAME model as tests/harness_audio_presence_preflight.rs.
"""
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "marker-decodability-preflight.sh"
_E2E = _ROOT / "scripts" / "recording-e2e.sh"

_JSON_UNDEC = (
    '{"preamble_screens":151444,"candidates":460,"cluster_samples":2,'
    '"crc_ok":300,"crc_fail":160,"peak_dbfs":-47.8,"verdict":"UNDECODED"}'
)
_JSON_OK = (
    '{"preamble_screens":188,"candidates":190,"cluster_samples":7,'
    '"crc_ok":107,"crc_fail":83,"peak_dbfs":-19.2,"verdict":"OK"}'
)


def run(snippet):
    """Source the lib and run `snippet`; return (exit_ok, stdout_trimmed)."""
    script = f'. "{_LIB}"\n{snippet}'
    p = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
    return p.returncode == 0, p.stdout.strip()


# --- single-sourced defaults ------------------------------------------------
def test_default_min_clusters_is_four():
    ok, v = run("marker_decodability_default_min_clusters")
    assert ok and v == "4"


def test_default_probe_secs_is_twentyfive():
    ok, v = run("marker_decodability_default_probe_secs")
    assert ok and v == "25"


# --- JSON field parse -------------------------------------------------------
def test_parse_num_reads_integer_and_signed_decimal_fields():
    assert run(f"marker_decodability_parse_num '{_JSON_UNDEC}' cluster_samples")[1] == "2"
    assert run(f"marker_decodability_parse_num '{_JSON_UNDEC}' preamble_screens")[1] == "151444"
    assert run(f"marker_decodability_parse_num '{_JSON_UNDEC}' peak_dbfs")[1] == "-47.8"


def test_parse_num_fails_nonzero_on_absent_key():
    ok, v = run(f"marker_decodability_parse_num '{_JSON_UNDEC}' nope && echo GOT")
    assert not ok
    assert v == ""


def test_parse_verdict_reads_the_word():
    assert run(f"marker_decodability_parse_verdict '{_JSON_UNDEC}'")[1] == "UNDECODED"
    assert run(f"marker_decodability_parse_verdict '{_JSON_OK}'")[1] == "OK"


def test_is_ok_only_the_literal_ok_word():
    assert run("marker_decodability_is_ok OK")[1] == "true"
    for w in ("UNDECODED", "SILENT", "POLLUTED", "", "ok"):
        arg = w if w else '""'  # pass an explicit empty-string arg, no backslash in an f-string
        assert run(f"marker_decodability_is_ok {arg}")[1] == "false"


# --- remote command builders ------------------------------------------------
def test_extract_wav_ps_keeps_every_channel_of_the_track():
    """Issue 1367: the stereo mbc track carries the marker on L and R with R 10.17 ms late; a mono
    downmix comb-filters the two copies into an undecodable signal (cluster 2, POLLUTED) while each
    channel alone decodes. The extract must keep every channel so the probe can pick the best."""
    _, c = run("marker_decodability_extract_wav_ps 'D:\\_REC\\p.mp4' 'C:\\camera-box\\mbc.wav' 0")
    assert "ffmpeg" in c
    assert "-map 0:a:0" in c
    assert "-ar 48000" in c and "pcm_f32le" in c
    assert "-ac" not in c, "no channel downmix before the QPSK demod"
    assert "pan=" not in c and "amix" not in c, "no filter-based downmix either"
    assert 'D:\\_REC\\p.mp4' in c and 'C:\\camera-box\\mbc.wav' in c


# the probe's per-channel JSON line (issue 1367): every pre-1367 key first, then the pick
_JSON_STEREO = (
    '{"preamble_screens":4,"candidates":4,"cluster_samples":4,"crc_ok":4,"crc_fail":0,'
    '"peak_dbfs":-17.8,"verdict":"OK","channels":2,"chosen_channel":1,"per_channel":['
    '{"channel":0,"ch_preamble_screens":649,"ch_cluster_samples":3,"ch_crc_ok":3,'
    '"ch_crc_fail":646,"ch_peak_dbfs":-17.8},'
    '{"channel":1,"ch_preamble_screens":4,"ch_cluster_samples":4,"ch_crc_ok":4,'
    '"ch_crc_fail":0,"ch_peak_dbfs":-17.9}]}'
)


def test_parse_reads_the_chosen_channel_fields_of_the_per_channel_line():
    assert run(f"marker_decodability_parse_num '{_JSON_STEREO}' cluster_samples")[1] == "4"
    assert run(f"marker_decodability_parse_num '{_JSON_STEREO}' preamble_screens")[1] == "4"
    assert run(f"marker_decodability_parse_num '{_JSON_STEREO}' peak_dbfs")[1] == "-17.8"
    assert run(f"marker_decodability_parse_num '{_JSON_STEREO}' chosen_channel")[1] == "1"
    assert run(f"marker_decodability_parse_verdict '{_JSON_STEREO}'")[1] == "OK"


def test_parse_never_reads_a_per_channel_value_whatever_the_key_order():
    """The gate's decision point must not depend on the producer's key order: even a line whose
    per-channel array comes FIRST with unprefixed keys yields the top-level (chosen) values."""
    reordered = (
        '{"per_channel":[{"channel":0,"preamble_screens":649,"cluster_samples":3,'
        '"peak_dbfs":-5.0,"verdict":"POLLUTED"}],"preamble_screens":4,"candidates":4,'
        '"cluster_samples":4,"crc_ok":4,"crc_fail":0,"peak_dbfs":-17.8,"verdict":"OK"}'
    )
    assert run(f"marker_decodability_parse_num '{reordered}' cluster_samples")[1] == "4"
    assert run(f"marker_decodability_parse_num '{reordered}' preamble_screens")[1] == "4"
    assert run(f"marker_decodability_parse_num '{reordered}' peak_dbfs")[1] == "-17.8"
    assert run(f"marker_decodability_parse_verdict '{reordered}'")[1] == "OK"


def test_delete_ps_removes_the_wav():
    _, c = run("marker_decodability_delete_ps 'C:\\camera-box\\mbc.wav'")
    assert "Remove-Item" in c and "C:\\camera-box\\mbc.wav" in c


# --- class-named abort messages ---------------------------------------------
def test_fail_message_names_the_class_and_the_numbers():
    _, m = run(f"marker_decodability_fail_message UNDECODED 2 4 151444 -47.8")
    assert "UNDECODED" in m
    assert "cluster 2 < 4" in m
    assert "preamble_screens 151444" in m
    assert "-47.8" in m
    assert "#1324" in m


def test_fail_message_silent_and_polluted_name_their_own_cause():
    _, sm = run("marker_decodability_fail_message SILENT 0 4 3 -91.0")
    assert "SILENT" in sm and "#748" in sm
    _, pm = run("marker_decodability_fail_message POLLUTED 1 4 90000 -5.0")
    assert "POLLUTED" in pm and "#1323" in pm


def test_unverified_and_unreadable_messages_are_distinct():
    _, u = run("marker_decodability_unverified_message")
    assert "UNVERIFIED" in u and "probe-tools" in u
    _, r = run("marker_decodability_probe_unreadable_message 'raw blob'")
    assert "could not parse a verdict" in r and "raw blob" in r


# --- recording-e2e.sh WIRING (static read) ----------------------------------
def _e2e():
    return _E2E.read_text()


def test_e2e_sources_the_lib():
    assert "lib/marker-decodability-preflight.sh" in _e2e()


def test_e2e_has_a_4b3_step_between_the_audio_presence_step_and_startrecord():
    s = _e2e()
    # anchor on the actual echoed STEP banners, not the earlier source-comment references.
    i_ap_step = s.find('echo "[4b2/8]')
    i_md_step = s.find('echo "[4b3/8]')
    i_sr = s.find("[5/8] StartRecord")
    assert i_ap_step != -1 and i_md_step != -1 and i_sr != -1
    # the decodability step runs AFTER the audio-presence step and BEFORE StartRecord
    assert i_ap_step < i_md_step < i_sr


def test_e2e_step_runs_the_probe_and_aborts_on_non_ok():
    # slice the [4b3/8] region up to [4c/8]
    s = _e2e()
    start = s.find('echo "[4b3/8]')
    end = s.find('echo "[4c/8]', start)
    region = s[start:end]
    assert "--qpsk-probe" in region, "the step must run recording-verdict --qpsk-probe"
    assert "marker_decodability_is_ok" in region, "the step must gate on the verdict"
    assert "marker_decodability_fail_message" in region, "abort must name the class"
    assert "exit 1" in region, "a non-OK verdict must abort the run"
    # skip only when the probe binary is absent -- a loud UNVERIFIED, never a silent pass
    assert "marker_decodability_unverified_message" in region


def test_e2e_single_sources_the_db_bars_from_the_audio_presence_lib():
    s = _e2e()
    start = s.find('echo "[4b3/8]')
    end = s.find('echo "[4c/8]', start)
    region = s[start:end]
    assert "audio_preflight_default_threshold_db" in region
    assert "audio_preflight_default_ceiling_db" in region


def test_harness_passes_the_negative_db_bars_in_equals_form():
    """PR 1326 E2E 35134835641 aborted [4b3/8] with clap's `error: unexpected argument '-6' found`:
    a separate `-60` token after `--qpsk-silent-db` is read as the short flag `-6`. The harness must
    pass BOTH negative bars in the `--flag=VALUE` form, which clap always takes literally."""
    s = _E2E.read_text(encoding="utf-8")
    step = s.index("[4b3/8] #1324 marker-decodability preflight")
    block = s[step : s.index("[4c/8]", step)]
    assert '"--qpsk-silent-db=$AUDIO_DECODABILITY_SILENT_DB"' in block
    assert '"--qpsk-loud-db=$AUDIO_DECODABILITY_LOUD_DB"' in block
    assert '--qpsk-silent-db "' not in block and '--qpsk-loud-db "' not in block
