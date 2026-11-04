"""issue 1474 follow-up -- a failed renderer never replaces an installed OBS-box config file.

`obs_box_write_if_changed` (scripts/lib/obs-box-baseline.sh) read its content from stdin, so a caller
that piped a renderer into it (`render | obs_box_write_if_changed DEST ...`) or fed it through a process
substitution (`< <(render)`) could not see the renderer fail: a partial or empty text was compared and
WRITTEN, and pipefail reported the failure only after the file was replaced. The command form
`obs_box_write_if_changed_rendered DEST MODE OWNER LABEL CMD [ARGS...]` runs the renderer itself, and
the stdin form refuses an empty input. Hermetic: every path is a temp file, the host tools are fakes."""
import os
import re
import stat
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"
ME = "%s:%s" % (subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip(),
                subprocess.run(["id", "-gn"], capture_output=True, text=True, check=True).stdout.strip())
OLD = "the installed config\n"


def _bash(body, args=(), env=None, stdin=None):
    """BODY under the callers' strict mode, the baseline lib sourced, a caller-style fail()."""
    script = ('set -euo pipefail\nfail() { echo "FAIL: $1" >&2; exit 1; }\nYELLOW=; NC=\n. "$1"\nshift\n'
              + body)
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "harness", str(BASELINE), *args], capture_output=True,
                          text=True, env=e, input=stdin, timeout=60)


def _installed(tmp_path, name="f.conf"):
    f = tmp_path / name
    f.write_text(OLD)
    f.chmod(0o644)
    return f


def _no_temp_left(d):
    return [p.name for p in d.iterdir() if p.name.startswith(".")] == []


# --- the command form -------------------------------------------------------------------------------
def test_a_failing_renderer_leaves_the_file_untouched_and_returns_non_zero(tmp_path):
    f = _installed(tmp_path)
    r = _bash('half() { printf "half"; return 3; }\n'
              'obs_box_write_if_changed_rendered "$1" 0644 "$2" "probe conf" half || echo "RC=$?"',
              args=(str(f), ME))
    assert r.returncode == 0, r.stderr
    assert "RC=1\n" in r.stdout
    assert f.read_text() == OLD
    assert "written" not in r.stdout
    assert "probe conf" in r.stderr and "rc 3" in r.stderr and str(f) in r.stderr
    assert _no_temp_left(tmp_path)


def test_an_empty_render_is_refused(tmp_path):
    f = _installed(tmp_path)
    r = _bash('nothing() { :; }\n'
              'obs_box_write_if_changed_rendered "$1" 0644 "$2" "probe conf" nothing || echo "RC=$?"',
              args=(str(f), ME))
    assert "RC=1\n" in r.stdout, r.stderr
    assert f.read_text() == OLD
    assert "renders nothing" in r.stderr


def test_a_failing_renderer_never_creates_a_missing_file(tmp_path):
    f = tmp_path / "absent.conf"
    r = _bash('obs_box_write_if_changed_rendered "$1" 0644 "$2" probe false || echo "RC=$?"',
              args=(str(f), ME))
    assert "RC=1\n" in r.stdout, r.stderr
    assert not f.exists()


def test_the_rendered_bytes_are_kept_exactly_and_a_rerun_is_quiet(tmp_path):
    f = _installed(tmp_path)
    body = ('two() { printf "a b\\n\\n\\n"; }\n'
            'obs_box_write_if_changed_rendered "$1" 0640 "$2" "probe conf" two')
    r = _bash(body, args=(str(f), ME))
    assert r.returncode == 0, r.stderr
    assert f.read_bytes() == b"a b\n\n\n"
    assert stat.S_IMODE(f.stat().st_mode) == 0o640
    assert f"probe conf: {f} written" in r.stdout
    again = _bash(body, args=(str(f), ME))
    assert again.returncode == 0, again.stderr
    assert f"probe conf: {f} unchanged" in again.stdout


def test_the_renderer_gets_its_arguments_verbatim(tmp_path):
    f = tmp_path / "args.conf"
    r = _bash('obs_box_write_if_changed_rendered "$1" 0644 "$2" probe printf "%s|%s\\n" "a b" "c*"',
              args=(str(f), ME))
    assert r.returncode == 0, r.stderr
    assert f.read_text() == "a b|c*\n"


# --- the stdin form ---------------------------------------------------------------------------------
def test_the_stdin_form_refuses_an_empty_input(tmp_path):
    f = _installed(tmp_path)
    r = _bash('obs_box_write_if_changed "$1" 0644 "$2" "probe conf" < /dev/null || echo "RC=$?"',
              args=(str(f), ME))
    assert "RC=1\n" in r.stdout, r.stderr
    assert f.read_text() == OLD
    assert "empty" in r.stderr


def test_the_stdin_form_still_writes_a_real_text(tmp_path):
    f = _installed(tmp_path)
    r = _bash('printf "new\\n" | obs_box_write_if_changed "$1" 0644 "$2" "probe conf"', args=(str(f), ME))
    assert r.returncode == 0, r.stderr
    assert f.read_text() == "new\n"


# --- real callers -----------------------------------------------------------------------------------
def test_gpu_ready_install_keeps_the_drop_in_when_its_renderer_fails(tmp_path):
    w = tmp_path / "wait"
    d = _installed(tmp_path, "50-obs-box-gpu-ready.conf")
    r = _bash('obs_box_gpu_ready_dropin_text() { printf "[Service]\\n"; return 2; }\n'
              'obs_box_gpu_ready_install "$1" "$2" "$3" || echo "RC=$?"', args=(str(w), str(d), ME))
    assert r.returncode != 0 or "RC=" in r.stdout, r.stdout
    assert d.read_text() == OLD


def _background(tmp_path, override):
    root = tmp_path / "root"
    (root / "etc" / "sudoers.d").mkdir(parents=True)
    (root / "etc" / "strih-isolated-cpus.conf").write_text("2,3,4,5,6,7\n")
    online = tmp_path / "online"
    online.write_text("0-7\n")
    b = tmp_path / "bin"
    b.mkdir()
    for name, text in (("systemctl", "#!/bin/bash\nexit 0\n"), ("visudo", "#!/bin/bash\nexit 0\n")):
        (b / name).write_text(text)
        (b / name).chmod(0o755)
    slice_file = root / "etc/systemd/system/rig-background.slice"
    slice_file.parent.mkdir(parents=True)
    slice_file.write_text(OLD)
    slice_file.chmod(0o644)
    r = _bash(override + '\nobs_box_background_slice strih "$(id -un)"',
              env={"PATH": f"{b}:{os.environ['PATH']}", "OBS_BOX_BACKGROUND_ROOT": str(root),
                   "OBS_BOX_BACKGROUND_OWNER": ME, "OBS_BOX_ONLINE_CPUS": str(online)})
    return r, slice_file


def test_background_slice_keeps_the_slice_when_its_renderer_fails(tmp_path):
    r, slice_file = _background(tmp_path, 'obs_box_background_slice_text() { printf "[Slice]\\n"; return 4; }')
    assert r.returncode != 0, r.stdout
    assert slice_file.read_text() == OLD


def test_background_slice_still_installs_with_a_working_renderer(tmp_path):
    r, slice_file = _background(tmp_path, ":")
    assert r.returncode == 0, r.stderr
    assert "AllowedCPUs=0-1\n" in slice_file.read_text()


# --- the sweep --------------------------------------------------------------------------------------
SAFE_PIPE = re.compile(r"""^\s*(?:[A-Za-z_]+="\$\()?printf '%s\\n' "\$[A-Za-z_][A-Za-z_0-9]*"\s*$""")


def _logical_lines(text):
    out, cur, start = [], "", 0
    for n, line in enumerate(text.splitlines(), 1):
        if not cur:
            start = n
        if line.endswith("\\"):
            cur += line[:-1] + " "
            continue
        if line.rstrip().endswith(("|", "&&", "||")) and not line.lstrip().startswith("#"):
            cur += line + " "
            continue
        out.append((start, cur + line))
        cur = ""
    return out


def _calls():
    for path in sorted(SCRIPTS.rglob("*.sh")):
        for n, line in _logical_lines(path.read_text()):
            code = line.split(" #", 1)[0] if not line.lstrip().startswith("#") else ""
            if re.search(r"\bobs_box_write_if_changed(?:_rendered)?\b(?![(:])", code) and "() {" not in code:
                yield path.relative_to(REPO), n, code


def test_no_install_wrapper_is_fed_through_a_process_substitution():
    bad = []
    for path in sorted(SCRIPTS.rglob("*.sh")):
        for n, line in _logical_lines(path.read_text()):
            if re.search(r"\b(?:obs_box_write_if_changed\w*|_bg_put)\b", line) and "< <(" in line:
                bad.append(f"{path.relative_to(REPO)}:{n}: {line.strip()}")
    assert bad == [], "\n".join(bad)


def test_every_call_is_checked_and_only_an_unfailing_producer_is_piped():
    seen, bad = 0, []
    for path, n, code in _calls():
        seen += 1
        where = f"{path}:{n}: {code.strip()}"
        if "< <(" in code:
            bad.append("process substitution hides the renderer's status -- " + where)
        if "||" not in code:
            bad.append("no || return/fail after the call -- " + where)
        m = re.search(r"\|\s*obs_box_write_if_changed\b(?!_rendered)", code)
        if m and not SAFE_PIPE.match(code[:m.start()]):
            bad.append("a renderer piped into the stdin form -- " + where)
    assert seen >= 20, seen
    assert bad == [], "\n".join(bad)
