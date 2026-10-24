"""issue 1474 item 5 -- the `[8/8e]` #652 cleanup plan line for the stream recording names the Linux
stream box's exact-path `rm` over plain ssh, never a `win-stream-snv Shell: Remove-Item` (the
stream box is the Linux stream-snv since 22.10.2026, issue 1462). The strih twin is issue 1317 part
4's `strih_lx_recording_cleanup_note`; both now print through ONE quoting helper
(`shell_ssh_rm_plan_line`, scripts/lib/shell-quote.sh). A Windows stream box (an OBS_FLEET override)
keeps the Windows line byte for byte.

The three plan sites are cut out of recording-e2e.sh and RUN under `set -euo pipefail`; the printed
line is parsed by bash twice (the local paste + ssh's remote shell), as the strih test does. Tier-0.
"""
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib"
E2E = REPO / "scripts" / "recording-e2e.sh"
WIN_STREAM_FLEET = "strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always"
REC_LX = "/home/newlevel/2026-10-24 10-00-00.mkv"
REC_WIN = "C:\\Users\\newlevel\\Videos\\2026-10-24 10-00-00.mkv"


def _bash(body, env=None):
    full = {k: v for k, v in os.environ.items()
            if k not in ("OBS_FLEET", "OBS_FLEET_ALIASES", "STRIH_PLATFORM", "STRIH_LX_HOST", "STREAM_USER", "STRIH_USER")}
    full.update(env or {})
    return subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=full, timeout=60)


def _note(args, env=None):
    return _bash('. "%s/stream-box.sh"\n. "%s/stream-lx-verdict.sh"\nstream_lx_recording_cleanup_note %s'
                 % (LIB, LIB, args), env=env)


def _one_path_through_two_shells(line, user_host, path):
    cmd = line.strip().split("ssh:", 1)[1].strip()
    assert cmd.startswith("ssh %s " % user_host), cmd
    local = _bash('ssh() { printf "%s" "$#"; printf "\\0%s" "$@"; }\n' + cmd)
    parts = local.stdout.split("\0")
    assert parts[0] == "2" and parts[1] == user_host, parts
    remote = _bash('rm() { printf "%s\\0" "$@"; }\n' + parts[2])
    assert remote.stdout.split("\0")[:-1] == ["-f", "--", path], (path, remote.stdout)


def test_the_stream_note_is_an_exact_path_rm_on_the_stream_box():
    r = _note("'  ' 10.77.9.204 \"%s\"" % REC_LX)
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("  stream-snv ssh:"), r.stdout
    assert "Remove-Item" not in r.stdout and "*" not in r.stdout and "strih" not in r.stdout, r.stdout
    for path in [REC_LX, "/home/newlevel/a'b.mkv", '/home/newlevel/q"$x`y\\z.mkv']:
        esc = path.replace("'", "'\\''")
        r = _note("'' 10.77.9.204 '%s'" % esc)
        _one_path_through_two_shells(r.stdout, "newlevel@10.77.9.204", path)


def test_the_stream_note_takes_the_stream_user():
    r = _note("'' 10.77.9.204 /home/op/x.mkv", env={"STREAM_USER": "op"})
    _one_path_through_two_shells(r.stdout, "op@10.77.9.204", "/home/op/x.mkv")
    r = _note("'' 10.77.9.204 /home/other/x.mkv other")
    _one_path_through_two_shells(r.stdout, "other@10.77.9.204", "/home/other/x.mkv")


def test_the_strih_note_is_unchanged_by_the_shared_helper():
    r = _bash('. "%s/strih-platform.sh"\nstrih_lx_recording_cleanup_note "  " 10.77.9.202 "/srv/_REC/a b.mkv"' % LIB)
    assert r.stdout == '  strih-snv ssh:         ssh newlevel@10.77.9.202 "rm -f -- \'/srv/_REC/a b.mkv\'"\n', r.stdout


# --- the three recording-e2e.sh plan sites ---------------------------------------------------------

def _sites():
    """Each plan site: from the strih platform `if` that precedes a stream Remove-Item line, to the
    first `fi` at that `if`'s indent after the stream line (the stream branch's own `fi`)."""
    lines = E2E.read_text().splitlines(keepends=True)
    out = []
    for k, ln in enumerate(lines):
        if "Remove-Item -Force -LiteralPath '${STREAM_HOST_PATH:-<unknown>}'" not in ln:
            continue
        start = max(i for i in range(k) if lines[i].strip() == 'if [ "$(strih_platform "$STRIH")" = "linux" ]; then')
        indent = lines[start][:len(lines[start]) - len(lines[start].lstrip())]
        end = next(i for i in range(k + 1, len(lines)) if lines[i] == indent + "fi\n")
        out.append("".join(lines[start:end + 1]))
    return out


def test_there_are_three_plan_sites_and_each_branches_the_stream_line_by_class():
    sites = _sites()
    assert len(sites) == 3, sites
    for site in sites:
        assert site.count('if [ "$(stream_box_platform "$STREAM")" = "linux" ]; then') == 1, site
        assert site.count('stream_lx_recording_cleanup_note') == 1, site
        assert site.index("stream_lx_recording_cleanup_note") < site.index("win-stream-snv Shell: Remove-Item"), site


def _run_site(site, env):
    body = ('set -euo pipefail\n. "%s/strih-platform.sh"\n. "%s/stream-box.sh"\n. "%s/stream-lx-verdict.sh"\n'
            'STRIH=10.77.9.202 STREAM=10.77.9.204 STRIH_HOST_PATH="/srv/_REC/s.mkv" STREAM_HOST_PATH="%s"\n%s'
            'echo SITE-DONE\n') % (LIB, LIB, LIB, env.pop("_REC"), site)
    r = _bash(body, env=env)
    assert r.returncode == 0 and "SITE-DONE" in r.stdout, r.stdout + r.stderr
    return r.stdout


def test_each_site_prints_the_linux_stream_line_on_the_linux_stream_box():
    for site in _sites():
        out = _run_site(site, {"_REC": REC_LX})
        assert "win-stream-snv" not in out, out
        line = next(ln for ln in out.splitlines() if "stream-snv ssh:" in ln)
        _one_path_through_two_shells(line, "newlevel@10.77.9.204", REC_LX)


def test_each_site_keeps_the_windows_line_for_a_windows_stream_box():
    for site in _sites():
        out = _run_site(site, {"_REC": REC_WIN, "OBS_FLEET": WIN_STREAM_FLEET})
        assert "stream-snv ssh:" not in out, out
        assert ("win-stream-snv Shell: Remove-Item -Force -LiteralPath '%s'" % REC_WIN) in out, out
