"""issue 1462 -- the stream box is a Linux OBS box since 22.10.2026 (stream-snv, fleet class
linux-genlock). The ONE place that knows how to reach it is scripts/lib/stream-box.sh:

  * stream_box_platform HOST     -> linux | windows, from the obs-fleet class (never a literal IP);
  * stream_box_exec ...          -> the bash text over plain ssh on Linux, the UNCHANGED PowerShell
                                    text through win_ssh_run on Windows;
  * stream_box_download ...      -> scp from the box (Linux path / Windows path);
  * stream_mbc_vban_expect HOST  -> `<stream> <port> <input>` from the Linux box's facts (the mbc
                                    VBAN input the TEST check grades), nothing on Windows.

plus the bash command builders next to the PowerShell ones (audio-presence + marker-decodability
libs). Every remote text here is RUN on a fake box (tests/python/stream_box_fakes_1462.py), so the
tests see what it does to a path with a space or a quote in it, not only how it reads.
Tier-0: no rig, no network, no cargo.
"""
import os
import pathlib
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stream_box_fakes_1462 import calls, ffmpeg_calls, make_rig  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib"
STREAM_BOX = LIB / "stream-box.sh"
WIN_SSH = LIB / "win-ssh-exec.sh"
AUDIO = LIB / "audio-presence-preflight.sh"
MARKER = LIB / "marker-decodability-preflight.sh"
WIN_STREAM_FLEET = "stream-snv|10.77.9.204|windows-genlock|always"


def _run(tmp_path, body, *args, env=None, rig=None):
    """BODY under the callers' strict mode with the libs sourced (win-ssh-exec first, like the
    callers); values reach it as $1.. only."""
    rig = rig or make_rig(tmp_path)
    e = dict(rig["env"])
    e.update(env or {})
    driver = tmp_path / "driver.sh"
    driver.write_text('set -euo pipefail\n. "%s"\n. "%s"\n. "%s"\n. "%s"\n%s\n' % (WIN_SSH, STREAM_BOX, AUDIO, MARKER, body))
    p = subprocess.run(["bash", str(driver), *[str(a) for a in args]], capture_output=True, text=True,
                       env=e, cwd=str(REPO), timeout=60)
    return p, rig


# --- the platform ---------------------------------------------------------------------------------

def test_the_default_fleet_reads_the_stream_box_as_linux(tmp_path):
    p, _ = _run(tmp_path, 'for h in 10.77.9.204 stream-snv stream STREAM-SNV; do printf "%s=%s\\n" "$h" "$(stream_box_platform "$h")"; done')
    assert p.returncode == 0, p.stderr
    assert p.stdout.split() == ["10.77.9.204=linux", "stream-snv=linux", "stream=linux", "STREAM-SNV=linux"]


def test_a_windows_class_row_an_unknown_and_an_empty_host_read_windows(tmp_path):
    p, _ = _run(tmp_path, 'printf "%s %s %s\\n" "$(stream_box_platform 10.77.9.204)" "$(stream_box_platform 192.0.2.9)" "$(stream_box_platform "")"',
                env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 0, p.stderr
    assert p.stdout.split() == ["windows", "windows", "windows"]


# --- the mbc VBAN expectation from the box facts --------------------------------------------------

def test_the_linux_stream_box_expects_its_fact_stream_and_port(tmp_path):
    p, _ = _run(tmp_path, 'out="$(stream_mbc_vban_expect 10.77.9.204)"; printf "[%s] loaded=[%s]\\n" "$out" "${STRIH_BOX_LOADED:-}"')
    assert p.returncode == 0, p.stderr
    # the facts are read in a subshell: the caller's shell never gets a loaded box
    assert p.stdout.strip() == "[stream 6980 mbc] loaded=[]"


def test_a_windows_stream_box_expects_nothing(tmp_path):
    p, _ = _run(tmp_path, 'out="$(stream_mbc_vban_expect 10.77.9.204)"; printf "[%s]\\n" "$out"',
                env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 0, p.stderr
    assert p.stdout.strip() == "[]"


def test_the_expectation_follows_the_fact_file(tmp_path):
    boxes = tmp_path / "boxes"
    shutil.copytree(REPO / "scripts" / "strih-boxes", boxes)
    f = boxes / "stream-snv.env"
    t = f.read_text()
    assert t.count("STRIH_PROGRAM_VBAN_STREAMS=stream\n") == 1 and t.count("STRIH_PROGRAM_VBAN_PORT=6980\n") == 1
    f.write_text(t.replace("STRIH_PROGRAM_VBAN_STREAMS=stream\n", "STRIH_PROGRAM_VBAN_STREAMS=prog.mix\n")
                  .replace("STRIH_PROGRAM_VBAN_PORT=6980\n", "STRIH_PROGRAM_VBAN_PORT=6981\n"))
    p, _ = _run(tmp_path, 'stream_mbc_vban_expect 10.77.9.204', env={"STRIH_BOXES_DIR": str(boxes)})
    assert p.returncode == 0, p.stderr
    assert p.stdout.strip() == "prog.mix 6981 mbc"


def test_the_input_name_comes_last_whole_with_its_spaces(tmp_path):
    p, _ = _run(tmp_path, 'read -r s port i <<<"$(stream_mbc_vban_expect 10.77.8.204)"; printf "[%s][%s][%s]\\n" "$s" "$port" "$i"',
                env={"OBS_FLEET": "stream-pp|10.77.8.204|linux-genlock|always"})
    assert p.returncode == 0, p.stderr
    assert p.stdout.strip() == "[stream][6980][Zvuk z mixu]"


def test_a_linux_box_with_no_fact_file_refuses_by_name(tmp_path):
    p, _ = _run(tmp_path, 'if out="$(stream_mbc_vban_expect 192.0.2.10)"; then echo "OK[$out]"; else echo "RC=$?"; fi',
                env={"OBS_FLEET": "stream-zz|192.0.2.10|linux-genlock|always"})
    assert p.returncode == 0, p.stderr
    assert p.stdout.strip() == "RC=1"
    assert "stream-zz" in p.stderr and "1462" in p.stderr


def test_a_linux_box_whose_facts_declare_no_program_vban_refuses_by_name(tmp_path):
    # strih-snv is linux-genlock too; issue 1439 phase 2 gave it a program VBAN stream, so the case runs on
    # a copy made hub-fed again (all four program facts `none`, the strih role's other legal shape)
    boxes = tmp_path / "boxes"
    shutil.copytree(REPO / "scripts" / "strih-boxes", boxes)
    f = boxes / "strih-snv.env"
    t = f.read_text()
    for old, new in (("STRIH_PROGRAM_VBAN_STREAMS=strih-obs\n", "STRIH_PROGRAM_VBAN_STREAMS=none\n"),
                     ("STRIH_PROGRAM_VBAN_PORT=6981\n", "STRIH_PROGRAM_VBAN_PORT=none\n"),
                     ("STRIH_PROGRAM_VBAN_RESERVE_MS=30\n", "STRIH_PROGRAM_VBAN_RESERVE_MS=none\n"),
                     ("STRIH_PROGRAM_AUDIO_INPUT=ASIO zvuk\n", "STRIH_PROGRAM_AUDIO_INPUT=none\n")):
        assert t.count(old) == 1, old
        t = t.replace(old, new)
    f.write_text(t)
    p, _ = _run(tmp_path, 'if out="$(stream_mbc_vban_expect 10.77.9.202)"; then echo "OK[$out]"; else echo "RC=$?"; fi',
                env={"STRIH_BOXES_DIR": str(boxes)})
    assert p.returncode == 0, p.stderr
    assert p.stdout.strip() == "RC=1"
    assert "STRIH_PROGRAM_VBAN" in p.stderr and "strih-snv" in p.stderr
    assert "declare no program VBAN stream" in p.stderr and "did not load" not in p.stderr


def test_facts_that_do_not_load_are_named_apart_from_facts_that_declare_none(tmp_path):
    boxes = tmp_path / "boxes"
    shutil.copytree(REPO / "scripts" / "strih-boxes", boxes)
    f = boxes / "stream-snv.env"
    t = f.read_text()
    assert t.count("STRIH_PROGRAM_VBAN_PORT=6980\n") == 1
    f.write_text(t.replace("STRIH_PROGRAM_VBAN_PORT=6980\n", "STRIH_PROGRAM_VBAN_PORT=oops\n"))
    p, _ = _run(tmp_path, 'if out="$(stream_mbc_vban_expect 10.77.9.204)"; then echo "OK[$out]"; else echo "RC=$?"; fi',
                env={"STRIH_BOXES_DIR": str(boxes)})
    assert p.returncode == 0, p.stderr
    assert p.stdout.strip() == "RC=1"
    assert "did not load" in p.stderr and "declare no program VBAN stream" not in p.stderr
    assert "STRIH_PROGRAM_VBAN_PORT 'oops'" in p.stderr, "the loader's own line names the bad fact"


# --- the bash builders, run as text on the fake box -----------------------------------------------

NASTY = "/home/newlevel/it's a $HOME `id` test.mkv"


def test_the_volumedetect_text_reads_the_exact_path_and_ffmpeg_null_sink(tmp_path):
    p, rig = _run(tmp_path, 'stream_box_exec newlevel pw 10.77.9.204 "$(audio_preflight_volumedetect_ps "$1")" "$(audio_preflight_volumedetect_sh "$1")"',
                  NASTY, env={"FAKE_MAX_DB": "-12.3"})
    assert p.returncode == 0, p.stderr
    argv = ffmpeg_calls(rig)
    assert len(argv) == 1
    a = argv[0]
    assert a[a.index("-i") + 1] == NASTY
    assert a[-4:] == ["volumedetect", "-f", "null", "-"]
    assert "max_volume: -12.3 dB" in p.stdout


def test_the_delete_text_removes_exactly_that_file(tmp_path):
    victim = tmp_path / "rec dir" / "2026-10-23 01-23-45 it's.mkv"
    sibling = tmp_path / "rec dir" / "2026-10-23 01-23-45 it's.mkv.keep"
    victim.parent.mkdir()
    victim.write_text("x")
    sibling.write_text("y")
    p, _ = _run(tmp_path, 'stream_box_exec newlevel pw 10.77.9.204 "$(audio_preflight_delete_ps "$1")" "$(audio_preflight_delete_sh "$1")"', victim)
    assert p.returncode == 0, p.stderr
    assert not victim.exists() and sibling.exists()


def test_the_wav_extract_keeps_every_channel_and_writes_the_wav(tmp_path):
    wav = tmp_path / "box wav" / "mbc it's.wav"
    wav.parent.mkdir()
    p, rig = _run(tmp_path, 'stream_box_exec newlevel pw 10.77.9.204 "$(marker_decodability_extract_wav_ps "$1" "C:\\x.wav" 0)" "$(marker_decodability_extract_wav_sh "$1" "$2" 0)"',
                  NASTY, wav)
    assert p.returncode == 0, p.stderr
    a = ffmpeg_calls(rig)[0]
    assert a == ["-hide_banner", "-nostats", "-y", "-i", NASTY, "-map", "0:a:0", "-ar", "48000", "-c:a", "pcm_f32le", str(wav)]
    assert "-ac" not in a  # issue 1367: never a downmix
    assert wav.read_bytes() == b"RIFF-fake-wav"
    p2, _ = _run(tmp_path, 'stream_box_exec newlevel pw 10.77.9.204 "$(marker_decodability_delete_ps "C:\\x.wav")" "$(marker_decodability_delete_sh "$1")"', wav, rig=rig)
    assert p2.returncode == 0, p2.stderr
    assert not wav.exists()


def test_a_failed_wav_extract_leaves_no_older_wav_behind(tmp_path):
    # the extract removes the WAV first: when ffmpeg then fails, the scp that follows finds nothing,
    # never the WAV an aborted earlier run left there
    wav = tmp_path / "box wav" / "mbc.wav"
    wav.parent.mkdir()
    wav.write_bytes(b"RIFF-stale")
    p, rig = _run(tmp_path, 'stream_box_exec newlevel pw 10.77.9.204 "" "$(marker_decodability_extract_wav_sh "$1" "$2" 0)" || echo "RC=$?"',
                  NASTY, wav, env={"FAKE_FFMPEG_RC": "1"})
    assert p.returncode == 0, p.stderr
    assert "RC=1" in p.stdout, "the extract's rc is ffmpeg's"
    assert not wav.exists()


def test_the_windows_powershell_texts_are_unchanged(tmp_path):
    p, _ = _run(tmp_path, 'printf "%s\\n" "$(audio_preflight_volumedetect_ps "$1")" "$(audio_preflight_delete_ps "$1")" "$(marker_decodability_extract_wav_ps "$1" "C:\\w.wav" 0)" "$(marker_decodability_delete_ps "C:\\w.wav")"',
                "C:\\Users\\newlevel\\Videos\\2026-10-23 01-23-45.mkv")
    assert p.returncode == 0, p.stderr
    rec = "C:\\Users\\newlevel\\Videos\\2026-10-23 01-23-45.mkv"
    assert p.stdout.splitlines() == [
        'ffmpeg -hide_banner -nostats -i "%s" -af volumedetect -f null NUL 2>&1' % rec,
        'Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath "%s"' % rec,
        'ffmpeg -hide_banner -nostats -y -i "%s" -map 0:a:0 -ar 48000 -c:a pcm_f32le "C:\\w.wav" 2>&1' % rec,
        'Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath "C:\\w.wav"',
    ]


# --- the transport --------------------------------------------------------------------------------

def test_the_linux_transport_is_plain_ssh_with_no_stale_known_hosts_and_one_remote_argument(tmp_path):
    p, rig = _run(tmp_path, 'stream_box_exec newlevel s3cret 10.77.9.204 "Write-Output win" "echo lx-ran; exit 3" || echo "RC=$?"')
    assert p.returncode == 0, p.stderr
    assert "lx-ran" in p.stdout and "RC=3" in p.stdout  # the remote rc comes back
    c = calls(rig)
    assert len(c) == 1 and c[0]["tool"] == "ssh"
    assert c[0]["target"] == "newlevel@10.77.9.204" and c[0]["pw"] == "s3cret"
    assert c[0]["remote"] == "echo lx-ran; exit 3"
    argv = " ".join(c[0]["argv"])
    for opt in ("StrictHostKeyChecking=no", "UserKnownHostsFile=/dev/null", "ConnectTimeout="):
        assert opt in argv, argv
    assert "-n" in c[0]["argv"], "stdin is /dev/null: the remote command never reads the caller's terminal"
    assert "ps" not in c[0], "a Linux box never gets PowerShell"


def test_an_empty_bash_text_for_a_linux_box_is_refused_before_any_ssh(tmp_path):
    # `ssh host ""` would open an interactive remote shell on the caller's terminal
    p, rig = _run(tmp_path, 'stream_box_exec newlevel pw 10.77.9.204 "Write-Output win" "" || echo "RC=$?"')
    assert p.returncode == 0, p.stderr
    assert "RC=2" in p.stdout and "no bash command text" in p.stderr
    assert calls(rig) == []


def test_the_windows_transport_sends_the_unchanged_powershell_through_win_ssh_run(tmp_path):
    ps = 'ffmpeg -hide_banner -nostats -i "C:\\a b.mkv" -af volumedetect -f null NUL 2>&1'
    p, rig = _run(tmp_path, 'stream_box_exec newlevel pw 10.77.9.204 "$1" "echo NEVER"', ps,
                  env={"OBS_FLEET": WIN_STREAM_FLEET, "FAKE_WIN_REPLY": "max_volume: -20.0 dB\n"})
    assert p.returncode == 0, p.stderr
    c = calls(rig)
    assert len(c) == 1 and c[0]["ps"] == ps
    assert "max_volume: -20.0 dB" in p.stdout and "NEVER" not in p.stdout


def test_a_windows_box_without_win_ssh_run_refuses_by_name(tmp_path):
    rig = make_rig(tmp_path)
    e = dict(rig["env"], OBS_FLEET=WIN_STREAM_FLEET)
    driver = tmp_path / "solo.sh"
    driver.write_text('set -euo pipefail\n. "%s"\nstream_box_exec u p 10.77.9.204 "Write-Output x" "echo x" || echo "RC=$?"\n' % STREAM_BOX)
    p = subprocess.run(["bash", str(driver)], capture_output=True, text=True, env=e, timeout=60)
    assert p.returncode == 0, p.stderr
    assert "RC=127" in p.stdout and "win-ssh-exec.sh" in p.stderr
    assert calls(rig) == []


def test_the_linux_download_is_scp_with_no_stale_known_hosts(tmp_path):
    src = tmp_path / "box" / "mbc.wav"
    src.parent.mkdir()
    src.write_bytes(b"wav-bytes")
    dst = tmp_path / "local.wav"
    p, rig = _run(tmp_path, 'stream_box_download newlevel pw 10.77.9.204 "C:\\camera-box\\x.wav" "$1" "$2"', src, dst)
    assert p.returncode == 0, p.stderr
    assert dst.read_bytes() == b"wav-bytes"
    c = calls(rig)
    assert c[0]["tool"] == "scp" and c[0]["src"] == "newlevel@10.77.9.204:%s" % src and c[0]["dst"] == str(dst)
    assert "UserKnownHostsFile=/dev/null" in " ".join(c[0]["argv"])


def test_the_windows_download_keeps_win_ssh_download(tmp_path):
    dst = tmp_path / "local.wav"
    p, rig = _run(tmp_path, 'stream_box_download newlevel pw 10.77.9.204 "C:\\camera-box\\x.wav" /nowhere.wav "$1" || echo "RC=$?"', dst,
                  env={"OBS_FLEET": WIN_STREAM_FLEET})
    assert p.returncode == 0, p.stderr
    c = calls(rig)
    # win_ssh_download turns the backslashes into forward slashes for scp (issue 703)
    assert c[0]["tool"] == "scp" and c[0]["src"] == "newlevel@10.77.9.204:C:/camera-box/x.wav"


def test_the_lib_is_source_only_and_strict_mode_free():
    text = STREAM_BOX.read_text()
    code = [ln for ln in text.splitlines() if ln.strip() and not ln.lstrip().startswith("#")]
    assert not any(ln.startswith("set -") for ln in code), "a sourced lib must not change the caller's shell options"


# --- the shared quoting helper (scripts/lib/shell-quote.sh) ------------------------------------------

QUOTE = LIB / "shell-quote.sh"


def test_shell_single_quote_is_one_word_the_shell_takes_byte_for_byte(tmp_path):
    values = [NASTY, "", "'", "''", "a'b'c", "trailing newline\n", "tab\there", "$(touch INJECTED)", "*"]
    for i, v in enumerate(values):
        driver = tmp_path / ("q%d.sh" % i)
        driver.write_text('set -euo pipefail\n. "%s"\neval "printf \'%%s\' $(shell_single_quote "$1")"\n' % QUOTE)
        p = subprocess.run(["bash", str(driver), v], capture_output=True, text=True, cwd=str(tmp_path), timeout=30)
        assert p.returncode == 0, p.stderr
        assert p.stdout == v, (v, p.stdout)
    assert not (tmp_path / "INJECTED").exists()


def test_every_bash_builder_quotes_through_the_one_helper():
    for lib in (AUDIO, MARKER, LIB / "strih-platform.sh"):
        text = lib.read_text()
        # issue 1474: strih-platform.sh prints its cleanup line through the quote lib's own
        # shell_ssh_rm_plan_line (which quotes with shell_single_quote), never a copy
        assert "shell_single_quote" in text or "shell_ssh_rm_plan_line" in text, lib
        assert "//\\'/\\'" not in text, "%s keeps its own copy of the escape" % lib


def test_the_quote_lib_is_source_only():
    code = [ln for ln in QUOTE.read_text().splitlines() if ln.strip() and not ln.lstrip().startswith("#")]
    assert not any(ln.startswith("set -") for ln in code)
