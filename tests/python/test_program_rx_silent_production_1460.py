"""Issue 1460 (owner ruling 12.11.2026): the program-rx SILENT verdict pages ONLY during a production,
i.e. while the venue's stream OBS programs `PRO` AND streams. Before, SILENT was machine-channel only
(a console switched off overnight reads the same as the 8.11 dead stream-snv NIC).

The production read is the read-only stuck-output probe of the venue's STREAM box (derived from the box
facts: the stream-role box with the same STRIH_VENUE), through the AUDIO_MIXER_PRODUCTION_CMD seam here.
"""
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_program_audio_rx_1460 as t  # noqa: E402

amd = t.amd
SILENT_AGE = amd.DEFAULT_STALE_AFTER_S + 1
PRO_LIVE = "scene0=PRO\nscene1=PRO\nactive0=1\nactive1=1\nwindow_s=0.5\n"
SNV_BOXES = "strih-snv|10.77.9.202 stream-snv|10.77.9.204"


def _silent(tmp_path, state, production, box="stream-pp|stream-pp.lan", rc=1, key="obs-vban:Zvuk z mixu"):
    return t._run(tmp_path, t._body(0, age=SILENT_AGE, head=2, key=key), state, box=box,
                  production=production, production_rc=rc)


def _silent_pages(log):
    return [ln for ln in log.splitlines() if "WOULD alert (SILENT)" in ln]


def test_a_silent_program_during_a_production_pages_after_two_passes(tmp_path):
    state = tmp_path / "state"
    first = _silent(tmp_path, state, PRO_LIVE)
    assert "program_rx=SILENT" in first and "production=yes" in first, first
    assert not [ln for ln in _silent_pages(first) if "alert_now=1" in ln], first
    second = _silent(tmp_path, state, PRO_LIVE)
    page = [ln for ln in _silent_pages(second) if "alert_now=1" in ln]
    assert len(page) == 1 and "stream-pp" in page[0], second


def test_silence_outside_a_production_never_pages(tmp_path):
    cases = {
        "another scene": "scene0=Development\nscene1=Development\nactive0=1\nactive1=1\n",
        "PRO not streaming": "scene0=PRO\nscene1=PRO\nactive0=0\nactive1=0\n",
        "cut away mid-read": "scene0=PRO\nscene1=POST\nactive0=1\nactive1=1\n",
        "unreadable": "scene0=\nscene1=\nactive0=\nactive1=\n",
    }
    for name, prod in cases.items():
        d = tmp_path / name.replace(" ", "-")
        d.mkdir()
        state = d / "state"
        for _ in range(3):
            log = _silent(d, state, prod)
            assert "program_rx=SILENT" in log and not _silent_pages(log), (name, log)
            assert "production=yes" not in log, (name, log)


def test_an_absent_or_hung_stream_obs_never_pages(tmp_path):
    for rc in (1, 2, 124):
        d = tmp_path / f"rc{rc}"
        d.mkdir()
        state = d / "state"
        for _ in range(3):
            log = _silent(d, state, None, rc=rc)
            assert "program_rx=SILENT" in log and not _silent_pages(log), (rc, log)


def test_silent_pages_only_with_the_program_rx_page_switch(tmp_path):
    state = tmp_path / "state"
    for _ in range(3):
        log = t._run(tmp_path, t._body(0, age=SILENT_AGE, head=2), state, page=None, production=PRO_LIVE)
        assert "program_rx=SILENT" in log and not _silent_pages(log), log


def test_the_strih_box_reads_its_venues_stream_box(tmp_path):
    # strih-snv's program is silent: production is read on stream-snv (same venue), dialled at its
    # roster address -- never strih-snv's own OBS.
    state = tmp_path / "state"
    _silent(tmp_path, state, PRO_LIVE, box=SNV_BOXES, key="dante")
    args = (tmp_path / "production.args").read_text().split()
    assert args[0] == "10.77.9.204", args
    assert "10.77.9.202" not in args, args


def test_the_venue_stream_box_comes_from_the_facts():
    for box, want in (("strih-snv", "stream-snv"), ("stream-snv", "stream-snv"),
                      ("strih-pp", "stream-pp"), ("stream-pp", "stream-pp"), ("resolume-snv", "")):
        out = subprocess.run(["bash", "-c", '. "$1"; program_audio_rx_stream_box "$2"', "h",
                              str(t._SCRIPTS / "lib" / "program-audio-rx.sh"), box],
                             capture_output=True, text=True, timeout=60)
        assert out.returncode == 0 and out.stdout.strip() == want, (box, out.stdout, out.stderr)
