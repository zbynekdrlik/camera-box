"""issue 1387 (owner 29.10.2026, comment 6509641172) -- verify-strih item 49: a running JPEG XS source
(camera_box_jxs_source) with lost_slices > 0 in its recent windows FAILs.

The log lines are rendered from the SHIPPED C format string of vendor/distroav/src/jxs-source.cpp, so a
change of the status line that would blind the grader fails here. Every bash call runs under the
callers' `set -euo pipefail`; values are passed as ARGUMENTS, never spliced into the script text.
"""
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-jxs-verify.sh"
SOURCE = REPO / "vendor" / "distroav" / "src" / "jxs-source.cpp"


def _bash(body, *args):
    pre = "set -euo pipefail\n" + f'. "{LIB}"\n'
    return subprocess.run(["bash", "-c", pre + body, "harness", *map(str, args)], capture_output=True,
                          text=True, cwd=str(REPO), timeout=60)


def _status_format():
    """The shipped `jxs-source '%s': fps=...` format (adjacent literals joined, PRIu64 -> llu)."""
    s = SOURCE.read_text()
    i = s.index("\"jxs-source '%s': fps=%.2f")
    out, j = "", i
    while True:
        m = re.match(r'\s*(?:"((?:[^"\\]|\\.)*)"|(PRIu64|PRId64))', s[j:])
        if not m:
            break
        out += m.group(1) if m.group(1) is not None else "llu"
        j += m.end()
    assert out.endswith("(issue 1387)"), out
    return out


def window(t, name, lost, link=1000):
    """One window line as libobs + DistroAV write it (`HH:MM:SS.mmm: [distroav] <message>`)."""
    vals = [name, 30.0, 150, lost, 1 if lost else 0, 0, 0, 0, 8, 0, 5, 0, 0, 0, 100, link]
    text = _status_format().replace("%.2f", "{:.2f}").replace("%llu", "{}").replace("%u", "{}").replace(
        "%s", "{}").replace("%02u", "{:02d}")
    return f"{t}: [distroav] " + text.format(*vals)


def summary(lines, windows=12, running=30):
    r = _bash('strih_jxs_lost_summary "$1" "$2" < "$3"', windows, running, _write(lines))
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


_files = []


def _write(lines):
    import tempfile
    f = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
    f.write("\n".join(lines) + "\n")
    f.close()
    _files.append(f.name)
    return f.name


def ts(sec):
    return f"{sec // 3600:02d}:{sec // 60 % 60:02d}:{sec % 60:02d}.000"


def test_the_rendered_line_is_the_shipped_shape():
    line = window("10:00:00.000", "JXS PGM src", 3)
    assert "jxs-source 'JXS PGM src': fps=30.00 frames=150 lost_slices=3 " in line
    assert line.endswith(" link_mbps=1000 (issue 1387)"), line


def test_a_running_source_sums_its_last_windows_only():
    base = 36_000
    lines = ["09:59:59.000: other line"]
    # an early loss, then 12 clean windows: the loss left the last minute
    lines.append(window(ts(base), "JXS PGM src", 7))
    lines += [window(ts(base + 5 * (k + 1)), "JXS PGM src", 0) for k in range(12)]
    assert summary(lines) == ["running|JXS PGM src|12|0"]
    # one lossy window inside the last 12 counts
    lines.append(window(ts(base + 70), "JXS PGM src", 2))
    assert summary(lines) == ["running|JXS PGM src|12|2"]
    assert summary(lines, windows=1) == ["running|JXS PGM src|1|2"]


def test_a_source_that_stopped_writing_is_not_running_and_names_survive_quotes():
    lines = [window(ts(36_000), "JXS cam5 src", 4),
             window(ts(36_005), "it's 'quoted'", 0),
             f"{ts(36_100)}: [distroav] something later"]
    assert summary(lines) == ["stopped|JXS cam5 src|1|4", "stopped|it's 'quoted'|1|0"]
    # within the running window it is graded
    lines.append(window(ts(36_110), "JXS cam5 src", 0))
    assert summary(lines)[0] == "running|JXS cam5 src|2|4"


def test_midnight_wraps_the_age():
    lines = [window("23:59:58.000", "JXS PGM src", 1), "00:00:10.000: later"]
    assert summary(lines) == ["running|JXS PGM src|1|1"]


def _row(lines):
    body = ('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }\n'
            'strih_jxs_verify_row "$1"')
    r = _bash(body, _write(lines) if lines is not None else "")
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_the_row_fails_a_lossy_running_source_and_passes_a_clean_one():
    rows = _row([window(ts(40_000), "JXS PGM src", 0), window(ts(40_005), "JXS PGM src", 5),
                 window(ts(40_006), "JXS cam5 src", 0)])
    assert rows[0].startswith("FAIL (jxs) 'JXS PGM src': lost_slices=5 in its last 2 window(s)"), rows
    assert "(issue 1387)" in rows[0]
    assert rows[1] == "PASS (jxs) 'JXS cam5 src': lost_slices=0 in its last 1 window(s)"


def test_the_row_notes_no_source_a_stopped_source_and_no_log():
    assert _row(["10:00:00.000: nothing about jpeg xs"]) == [
        "NOTE (jxs) no JPEG XS source (camera_box_jxs_source) in the running OBS log -- nothing to grade (issue 1387)"]
    rows = _row([window(ts(40_000), "JXS PGM src", 9), f"{ts(40_200)}: later"])
    assert rows[0].startswith("NOTE (jxs) 'JXS PGM src': no window line in the last 30 s")
    assert _row(None)[0].startswith("NOTE (jxs) no OBS log")


def test_verify_strih_carries_item_49_within_budget():
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert v.count('strih_jxs_verify_row "$LOG"') == 1
    assert v.index('LOG="$(newest_log)"') < v.index("strih_jxs_verify_row")
    assert len(v.splitlines()) <= 1000
    p = (SCRIPTS / "lib" / "strih-provision.sh").read_text()
    assert '. "$(dirname "${BASH_SOURCE[0]}")/strih-jxs-verify.sh"' in p
