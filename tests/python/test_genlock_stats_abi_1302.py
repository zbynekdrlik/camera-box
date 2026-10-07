"""issue 1302 -- a FAST genlock deploy refuses an obs.dll whose stats ABI differs from the frontend's.

`deploy-genlock-fleet.sh --fast` swaps obs.dll alone and keeps the frontend (obs64.exe) of the last
full-bundle deploy. The frontend allocates `struct obs_genlock_stats` AND `struct
obs_genlock_output_stats` on its stack and passes no size, so a newer obs.dll that fills a bigger
struct (OBS_GENLOCK_STATS_VERSION 3 -> 4, or a bumped OBS_GENLOCK_OUTPUT_STATS_VERSION) writes past
the frontend's copy and OBS crashes. Live on 7.10.2026: stream ran a v3 build (a33d91a8e, its last
deploy a FAST one) while dev carried v4. Design: issue 1302 comment 6028838843 (Approach 1, part 2);
ROZHODNUTÉ 6030159870 item 1 extended the marker to the output-stats struct.

The marker GENLOCK_STATS_ABI.txt is two lines: the stats version, then `output_stats=<N>`. The
planner passes the pair as ONE value `<stats>:<output_stats>` (e.g. `4:1`).

This suite pins:
  * the pure parts of scripts/lib/genlock-stats-abi.sh: the obs.h reader (the repo's own obs.h and a
    synthetic two-commit repo), the read at a commit, and genlock_fast_abi_verdict over its vectors
    (BOTH versions compared, the refusal names whichever struct differs, a one-line marker written
    before the output-stats line refuses as missing output stats);
  * the emitted PowerShell gate RUN in pwsh against a fake install dir, on the same vectors, with the
    same verdict and the same refusal text as the bash decision;
  * the emitted deploy program: FAST gates at step (0f), right after the path preflight and before
    anything on the box changes (the obs-websocket read, the power plan, AutoHotkey64, the keep-alive
    tasks, the stop, the backup, the copy); FULL records GENLOCK_STATS_ABI.txt next to the other
    markers, or removes it when the planner could not read both versions; the whole FAST program, run
    in pwsh against a box without the right marker, exits 13 having changed nothing; the marker the
    FULL program writes passes the FAST gate;
  * the planner: --plan reads both versions from `git show <sha>:vendor/obs-studio/libobs/obs.h`,
    REFUSES --fast (exit 3) when it cannot, and warns + removes on --full; the old plan-time refusal
    of an output-stats version other than 1 is gone (the gate compares it now);
  * the Linux legs: genlock_write_markers' 5th argument (and setup-imag.sh's inline copy, behaviour for
    behaviour), the staged bundle file, setup-strih.sh reading it back, the imag program.

pwsh: ubuntu-latest ships it; dev1 has a portable one at ~/.local/pwsh74/pwsh. A missing pwsh FAILS,
never skips.
"""
import os
import pathlib
import re
import shutil
import subprocess

import pytest

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "genlock-stats-abi.sh"
MARKERS = SCRIPTS / "lib" / "genlock-markers.sh"
FLEET = SCRIPTS / "deploy-genlock-fleet.sh"
SETUP_IMAG = SCRIPTS / "setup-imag.sh"
SETUP_STRIH = SCRIPTS / "setup-strih.sh"
OBS_H = REPO / "vendor" / "obs-studio" / "libobs" / "obs.h"
BOXES = ["stream", "resolume"]
FORCE_LINE = ("Get-Process obs64,obs-browser-page -ErrorAction SilentlyContinue | "
              "Stop-Process -Force -ErrorAction SilentlyContinue")
REQUIRED = ": a full-bundle deploy is required"


def _bash(script, *args, strict=True):
    """Run `script` under the caller's strict mode. Every variable value goes in as a positional
    ARGUMENT ($1, $2, ...), never into the script text: a value carrying a quote must stay data (a
    first draft interpolated an injection vector and its own `rm -rf /` ran in the harness)."""
    head = "set -euo pipefail\n" if strict else "set -uo pipefail\n"
    return subprocess.run(["bash", "-c", head + script, "harness", *map(str, args)],
                          capture_output=True, text=True, timeout=120)


def _lib(script, *args):
    """`script` with the stats-ABI lib sourced; its own arguments are $2, $3, ... ($1 is the lib)."""
    return _bash('. "$1"\n' + script, LIB, *args)


def _repo_abi(define="OBS_GENLOCK_STATS_VERSION"):
    """The version in the checkout's obs.h, read independently of the bash reader."""
    found = re.findall(rf"^\s*#\s*define\s+{define}\s+(\d+)\b", OBS_H.read_text(), re.M)
    assert len(found) == 1, found
    return found[0]


def _repo_pair():
    """The checkout's `<stats>:<output_stats>` pair."""
    return _repo_abi() + ":" + _repo_abi("OBS_GENLOCK_OUTPUT_STATS_VERSION")


def _head_sha():
    return subprocess.run(["git", "-C", str(REPO), "rev-parse", "HEAD"], capture_output=True, text=True,
                          check=True).stdout.strip()


def _pwsh():
    pwsh = os.environ.get("PWSH") or shutil.which("pwsh")
    home_pwsh = os.path.expanduser("~/.local/pwsh74/pwsh")
    if not pwsh and os.access(home_pwsh, os.X_OK):
        pwsh = home_pwsh
    if not pwsh:
        pytest.fail("no pwsh: install PowerShell 7 or set PWSH=/path/to/pwsh (the gate must parse and run)")
    return pwsh


def _program(box, mode, abi, stage="C:\\stage", obs_dir="C:\\Program Files\\obs-studio", backup="C:\\obs-backup"):
    r = _bash(
        '. "$1"; build_windows_deploy_program "$2" "$3" "$4" "$5" "$(fleet_box_ahk_mode "$2")" "$6" '
        '3 abc123 def456 0 "$7"',
        FLEET, box, mode, stage, obs_dir, backup, abi,
    )
    assert r.returncode == 0, r.stderr
    return r.stdout


# --- the obs.h reader ----------------------------------------------------------------------------

def test_the_reader_reads_the_repo_obs_h_1302():
    r = _lib('genlock_stats_abi_from_obs_h < "$2"', OBS_H)
    assert r.returncode == 0, r.stderr
    assert r.stdout == _repo_abi() + "\n"
    r = _lib('genlock_stats_abi_from_obs_h OBS_GENLOCK_OUTPUT_STATS_VERSION < "$2"', OBS_H)
    assert r.returncode == 0, r.stderr
    assert r.stdout == _repo_abi("OBS_GENLOCK_OUTPUT_STATS_VERSION") + "\n"


@pytest.mark.parametrize("text,want", [
    ("#define OBS_GENLOCK_STATS_VERSION 4\n", "4"),
    ("  #  define\tOBS_GENLOCK_STATS_VERSION   7 /* a note */\n", "7"),
    ("#define OBS_GENLOCK_STATS_VERSION 12 // a note\n", "12"),
    (" * bumping OBS_GENLOCK_STATS_VERSION; a consumer reads it\n#define OBS_GENLOCK_STATS_VERSION 5\n", "5"),
])
def test_the_reader_takes_the_one_define_1302(text, want):
    r = subprocess.run(["bash", "-c", f'set -euo pipefail; . "{LIB}"; genlock_stats_abi_from_obs_h'],
                       input=text, capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want + "\n"


@pytest.mark.parametrize("text", [
    "",
    "#define OTHER_VERSION 4\n",
    "#define OBS_GENLOCK_STATS_VERSIONS 4\n",
    "#define OBS_GENLOCK_STATS_VERSION\n",
    "#define OBS_GENLOCK_STATS_VERSION 0\n",
    "#define OBS_GENLOCK_STATS_VERSION 04\n",
    "#define OBS_GENLOCK_STATS_VERSION x\n",
    "#define OBS_GENLOCK_STATS_VERSION 4\n#define OBS_GENLOCK_STATS_VERSION 4\n",
    "// #define OBS_GENLOCK_STATS_VERSION 4\n",
])
def test_the_reader_refuses_a_missing_duplicate_or_malformed_define_1302(text):
    r = subprocess.run(["bash", "-c", f'set -euo pipefail; . "{LIB}"; rc=0; genlock_stats_abi_from_obs_h || rc=$?; echo "rc=$rc"'],
                       input=text, capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "rc=1\n"


@pytest.fixture(scope="module")
def abi_repo(tmp_path_factory):
    """A throwaway repo: obs.h at v3, then v4, then without the define."""
    repo = tmp_path_factory.mktemp("abi_repo")
    git = ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    obs_h = repo / "vendor" / "obs-studio" / "libobs" / "obs.h"
    obs_h.parent.mkdir(parents=True)
    shas = {}
    for tag, body in (("v3", "#define OBS_GENLOCK_STATS_VERSION 3\n"),
                      ("v4", "/* x */\n#define OBS_GENLOCK_STATS_VERSION 4\nstruct s { int a; };\n"),
                      ("none", "struct s { int a; };\n")):
        obs_h.write_text(body)
        subprocess.run(git + ["add", "-A"], check=True)
        subprocess.run(git + ["commit", "-q", "-m", tag], check=True)
        shas[tag] = subprocess.run(git + ["rev-parse", "HEAD"], capture_output=True, text=True,
                                   check=True).stdout.strip()
    # the working tree carries a DIFFERENT version: the read must come from the commit, never the tree
    obs_h.write_text("#define OBS_GENLOCK_STATS_VERSION 9\n")
    return repo, shas


@pytest.mark.parametrize("tag,length,want", [("v3", 40, "3"), ("v4", 40, "4"), ("v4", 9, "4")])
def test_the_read_at_a_commit_uses_that_commits_obs_h_1302(abi_repo, tag, length, want):
    repo, shas = abi_repo
    r = _lib('genlock_stats_abi_at_sha "$2" "$3"', shas[tag][:length], repo)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want + "\n"


@pytest.mark.parametrize("sha", ["NONE", "deadbeefdeadbeef", "HEAD", "--help", "-p", "", "abc"])
def test_the_read_at_a_commit_refuses_what_it_cannot_read_1302(abi_repo, sha):
    repo, shas = abi_repo
    sha = shas["none"] if sha == "NONE" else sha
    r = _lib('rc=0; genlock_stats_abi_at_sha "$2" "$3" || rc=$?; echo "rc=$rc"', sha, repo)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "rc=1\n"


# --- the decision ----------------------------------------------------------------------------------

OK41 = "OK frontend stats ABI v4 == new obs.dll v4; frontend output stats ABI v1 == new obs.dll v1"
NO_FILE = "missing (no GENLOCK_STATS_ABI.txt)"
NO_OUT_LINE = "missing (no output_stats line in GENLOCK_STATS_ABI.txt)"
BAD_LINE1 = "unreadable (line 1 of GENLOCK_STATS_ABI.txt is not a version)"
BAD_LINE2 = "unreadable (line 2 of GENLOCK_STATS_ABI.txt is not output_stats=<version>)"
TOO_LONG = "unreadable (GENLOCK_STATS_ABI.txt has more than two lines)"


def _refused(stats=None, output=None):
    """The expected refusal: one part per struct that differs, stats first, joined with '; '."""
    parts = []
    if stats:
        parts.append("frontend stats ABI {}, new obs.dll {}".format(*stats))
    if output:
        parts.append("frontend output stats ABI {}, new obs.dll {}".format(*output))
    return "REFUSED " + "; ".join(parts) + REQUIRED


VERDICTS = [
    ("4:1", "present", "4\r\noutput_stats=1\r\n", 0, OK41),
    ("4:1", "present", " 4 \n output_stats = 1 \n\n", 0, OK41),
    ("4:1", "present", "4\noutput_stats=1\n\x0b\x0c\n", 0, OK41),
    ("44:1", "present", "4 4\noutput_stats=1", 0,
     "OK frontend stats ABI v44 == new obs.dll v44; frontend output stats ABI v1 == new obs.dll v1"),
    # only the stats struct differs: the refusal names it alone
    ("4:1", "present", "3\r\noutput_stats=1\r\n", 1, _refused(stats=("v3", "v4"))),
    ("3:1", "present", "4\noutput_stats=1", 1, _refused(stats=("v4", "v3"))),
    # only the output stats struct differs: the refusal names it alone
    ("4:2", "present", "4\r\noutput_stats=1\r\n", 1, _refused(output=("v1", "v2"))),
    ("4:1", "present", "4\noutput_stats=2", 1, _refused(output=("v2", "v1"))),
    # both differ: both named, stats first
    ("4:2", "present", "3\noutput_stats=1", 1, _refused(stats=("v3", "v4"), output=("v1", "v2"))),
    # a marker written before the output-stats line existed: REFUSED as missing output stats
    ("4:1", "present", "4\r\n", 1, _refused(output=(NO_OUT_LINE, "v1"))),
    ("4:1", "present", "4", 1, _refused(output=(NO_OUT_LINE, "v1"))),
    ("4:1", "missing", "", 1, _refused(stats=(NO_FILE, "v4"), output=(NO_FILE, "v1"))),
    ("4:1", "present", "", 1, _refused(stats=(BAD_LINE1, "v4"), output=(NO_OUT_LINE, "v1"))),
    ("4:1", "present", "abc\noutput_stats=1", 1, _refused(stats=(BAD_LINE1, "v4"))),
    ("4:1", "present", "04\noutput_stats=1", 1, _refused(stats=(BAD_LINE1, "v4"))),
    ("4:1", "present", "output_stats=1\n4", 1, _refused(stats=(BAD_LINE1, "v4"), output=(BAD_LINE2, "v1"))),
    ("4:1", "present", "4\noutput_stats=01", 1, _refused(output=(BAD_LINE2, "v1"))),
    ("4:1", "present", "4\nOUTPUT_STATS=1", 1, _refused(output=(BAD_LINE2, "v1"))),
    ("4:1", "present", "4\n1", 1, _refused(output=(BAD_LINE2, "v1"))),
    ("4:1", "present", "4\noutput_stats=", 1, _refused(output=(BAD_LINE2, "v1"))),
    ("4:1", "present", "4\noutput_stats=1\nx", 1, _refused(output=(TOO_LONG, "v1"))),
    ("4:1", "present", "4\noutput_stats=1\noutput_stats=1", 1, _refused(output=(TOO_LONG, "v1"))),
    # a lone CR is no line break: one line, which is not a version
    ("4:1", "present", "4\routput_stats=1", 1, _refused(stats=(BAD_LINE1, "v4"), output=(NO_OUT_LINE, "v1"))),
    # the new obs.dll's pair: each part unknown on its own
    ("", "present", "4\noutput_stats=1", 1, _refused(stats=("v4", "unknown"), output=("v1", "unknown"))),
    ("4", "present", "4\noutput_stats=1", 1, _refused(output=("v1", "unknown"))),
    (":1", "present", "4\noutput_stats=1", 1, _refused(stats=("v4", "unknown"))),
    ("4:", "present", "4\noutput_stats=1", 1, _refused(output=("v1", "unknown"))),
    ("4:x", "present", "4\noutput_stats=1", 1, _refused(output=("v1", "unknown"))),
    ("4:1:2", "present", "4\noutput_stats=1", 1, _refused(output=("v1", "unknown"))),
    ("x", "missing", "", 1, _refused(stats=(NO_FILE, "unknown"), output=(NO_FILE, "unknown"))),
]


@pytest.mark.parametrize("new,state,text,rc,line", VERDICTS)
def test_the_fast_verdict_1302(new, state, text, rc, line):
    r = subprocess.run(
        ["bash", "-c", 'set -euo pipefail; . "$1"; rc=0; genlock_fast_abi_verdict "$2" "$3" "$4" || rc=$?; echo "rc=$rc"',
         "x", str(LIB), new, state, text],
        capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    assert r.stdout == f"{line}\nrc={rc}\n"


def test_the_fast_verdict_refuses_a_bad_marker_state_1302():
    r = _lib('rc=0; genlock_fast_abi_verdict 4:1 bogus "" || rc=$?; echo "rc=$rc"')
    assert r.returncode == 0
    assert r.stdout == "rc=2\n"
    assert "MARKER_STATE" in r.stderr


# --- the PowerShell gate, RUN in pwsh, against the bash decision ---------------------------------

GATE_VECTORS = [
    (b"4\r\noutput_stats=1\r\n", "4:1"), (b"4\noutput_stats=1", "4:1"),
    (b" 4 \r\n output_stats=1 \r\n\r\n", "4:1"), (b"4\noutput_stats=1\n\x0b\x0c\n", "4:1"),
    (b"3\r\noutput_stats=1\r\n", "4:1"), (b"4\r\noutput_stats=1\r\n", "4:2"), (b"3\r\noutput_stats=2\r\n", "4:1"),
    (b"4\r\n", "4:1"), (b"", "4:1"), (b"abc\r\noutput_stats=1", "4:1"),
    (b"\xef\xbb\xbf4\r\noutput_stats=1\r\n", "4:1"), (b"04\noutput_stats=1", "4:1"), (b"4\noutput_stats=01", "4:1"),
    (b"4\nOUTPUT_STATS=1", "4:1"), (b"4\noutput_stats=1\nx", "4:1"), (b"output_stats=1\n4", "4:1"),
    (b"4 4\r\noutput_stats=1\r\n", "44:1"), (b"4\routput_stats=1", "4:1"),
    (None, "4:1"), (None, ""), (b"4\r\noutput_stats=1\r\n", ""), (b"4\r\noutput_stats=1\r\n", "4"),
    (b"4\r\noutput_stats=1\r\n", "x"), (b"4\r\noutput_stats=1\r\n", ":1"), (b"4\r\noutput_stats=1\r\n", "4:1:2"),
]


def _run_gate(pwsh, tmp_path, marker, new):
    box = tmp_path / "obs"
    box.mkdir()
    if marker is not None:
        (box / "GENLOCK_STATS_ABI.txt").write_bytes(marker)
    gate = _lib('genlock_fast_abi_gate_ps fast "$2"', new)
    assert gate.returncode == 0, gate.stderr
    assert "(0f) issue 1302" in gate.stdout
    script = tmp_path / "gate.ps1"
    script.write_text("$ErrorActionPreference = 'Stop'\n"
                      f"$obsDir = '{box}'\n" + gate.stdout + "\nWrite-Host 'HARNESS PAST THE GATE'\nexit 0\n")
    return subprocess.run([pwsh, "-NoProfile", "-NonInteractive", "-File", str(script)],
                          capture_output=True, text=True, timeout=120)


@pytest.mark.parametrize("marker,new", GATE_VECTORS)
def test_the_powershell_gate_decides_as_the_bash_verdict_1302(tmp_path, marker, new):
    pwsh = _pwsh()
    ps = _run_gate(pwsh, tmp_path, marker, new)
    if marker is None:
        sh = _lib('rc=0; genlock_fast_abi_verdict "$2" missing "" || rc=$?; echo "rc=$rc"', new)
    else:
        (tmp_path / "marker").write_bytes(marker)
        sh = _lib('rc=0; genlock_fast_abi_verdict "$2" present "$(cat "$3")" || rc=$?; echo "rc=$rc"',
                  new, tmp_path / "marker")
    assert sh.returncode == 0, sh.stderr
    line, rc = sh.stdout.splitlines()
    if rc == "rc=0":
        assert ps.returncode == 0, ps.stdout + ps.stderr
        assert "HARNESS PAST THE GATE" in ps.stdout
        assert f"stats ABI OK: {line[len('OK '):]} -- " in ps.stdout
    else:
        assert rc == "rc=1"
        assert ps.returncode == 13, ps.stdout + ps.stderr
        assert "HARNESS PAST THE GATE" not in ps.stdout
        assert f"FAST DEPLOY REFUSED: {line[len('REFUSED '):]}. Nothing on this box was changed." in ps.stdout


def test_the_full_mode_has_no_gate_1302():
    r = _lib("genlock_fast_abi_gate_ps full 4:1")
    assert r.returncode == 0 and r.stdout == ""


# --- the emitted deploy program ------------------------------------------------------------------

WRITE_ABI_41 = "Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt') @('4', 'output_stats=1')"


@pytest.mark.parametrize("box", BOXES)
def test_the_fast_program_gates_before_anything_changes_1302(box):
    p = _program(box, "fast", "4:1")
    gate = p.index("# (0f) issue 1302")
    assert p.index("# (0) preflight") < gate
    assert p.index("if (-not (Test-Path $obsDir))") < gate
    # before the obs-websocket read, the power plan, AutoHotkey64, the keep-alive tasks, the stop,
    # the backup and the obs.dll copy
    for later in ("# (0a) issue 1367", "# (0b)", "# (1) ", "# (1b)", FORCE_LINE, "# (3) Back up",
                  "New-Item -ItemType Directory", "try { Copy-Item -Force $src $dst }"):
        assert gate < p.index(later), later
    seg = p[gate:p.index("# (0a) issue 1367")]
    assert "\n$abiNew    = '4'\n" in seg
    assert "\n$abiNewOut = '1'\n" in seg
    assert "$abiFile   = Join-Path $obsDir 'GENLOCK_STATS_ABI.txt'" in seg
    assert "FAST DEPLOY REFUSED: $($abiRefused -join '; ')" + REQUIRED in seg
    assert seg.index("FAST DEPLOY REFUSED") < seg.index("exit 13")
    assert p.count("# (0f) issue 1302") == 1
    # the fast deploy keeps the frontend, so it keeps the frontend's marker
    assert "GENLOCK_STATS_ABI.txt is left as it is" in p
    assert "Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in p
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in p


@pytest.mark.parametrize("box", BOXES)
def test_the_full_program_records_the_abi_next_to_the_build_sha_1302(box):
    p = _program(box, "full", "4:1")
    assert "(0f) issue 1302" not in p and "exit 13" not in p
    sha = p.index("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_BUILD_SHA.txt')")
    abi = p.index(WRITE_ABI_41)
    assert sha < abi < p.index("# (6) sha256 verify")
    assert p.count("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')") == 1
    # nothing removes the recorded marker after the copy (a clear BEFORE the copy is allowed)
    after_copy = p[p.index("# (4) FULL bundle"):]
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in after_copy


@pytest.mark.parametrize("box", BOXES)
@pytest.mark.parametrize("abi", ["", "4", "4:", ":1", "4:x", "4:1:2", "04:1"])
def test_the_full_program_removes_the_marker_when_the_abi_is_unknown_1302(box, abi):
    """Only a complete pair is recorded: a marker that names one struct would let a fast deploy
    through on a frontend whose other struct the planner never read."""
    p = _program(box, "full", abi)
    # the LAST removal: the one after the markers (a clear before the copy may come earlier)
    rm = p.rindex("Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')")
    assert p.index("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_BUILD_SHA.txt')") < rm < p.index("# (6) sha256 verify")
    assert "Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in p
    assert "a fast deploy will refuse until the next full-bundle deploy" in p


@pytest.mark.parametrize("mode", ["fast", "full"])
@pytest.mark.parametrize("abi,stats,out", [
    ("4'; Write-Host INJECTED; '", "", ""),
    ("4:1'; Write-Host INJECTED; '", "4", ""),
    ("4'; Write-Host INJECTED; ':1", "", "1"),
    # a ':' inside the payload moves the split, never past the validation
    ("4'; Write-Host C:\\INJECTED; '", "", ""),
])
def test_an_injected_abi_never_reaches_the_program_1302(mode, abi, stats, out):
    p = _program("stream", mode, abi)
    assert "INJECTED" not in p
    if mode == "fast":
        assert f"\n$abiNew    = '{stats}'\n" in p
        assert f"\n$abiNewOut = '{out}'\n" in p
    else:
        assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" in p


def test_every_program_parses_and_uses_no_powershell_7_only_syntax_1302(tmp_path):
    files = []
    for box in BOXES:
        for mode in ("fast", "full"):
            for abi in ("4:1", ""):
                f = tmp_path / f"{box}-{mode}-{abi.replace(':', '_') or 'unknown'}.ps1"
                f.write_text(_program(box, mode, abi))
                files.append(str(f))
    for fn, mode, abi in (("genlock_fast_abi_gate_ps", "fast", "4:1"), ("genlock_stats_abi_marker_ps", "full", "4:1"),
                          ("genlock_stats_abi_marker_ps", "full", ""), ("genlock_stats_abi_marker_ps", "fast", "4:1")):
        f = tmp_path / f"{fn}-{len(files)}.ps1"
        f.write_text(_lib(f'{fn} "$2" "$3"', mode, abi).stdout)
        files.append(str(f))
    scan = tmp_path / "scan.ps1"
    scan.write_text(
        "$bad = 0\n"
        "function Get-AllTokens($ts) { foreach ($x in $ts) { $x; if ($x.NestedTokens) { Get-AllTokens $x.NestedTokens } } }\n"
        "foreach ($f in $args) {\n"
        "  $t = $null; $e = $null\n"
        "  [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$t, [ref]$e)\n"
        "  foreach ($x in $e) { Write-Output \"$f :: line $($x.Extent.StartLineNumber): $($x.Message)\"; $bad++ }\n"
        "  foreach ($x in @(Get-AllTokens $t | Where-Object { $_.Kind -in 'AndAnd','OrOr','QuestionQuestion',"
        "'QuestionQuestionEquals','QuestionDot','QuestionLBracket','QuestionMark' })) {\n"
        "    Write-Output \"$f :: PS7-only $($x.Kind) at line $($x.Extent.StartLineNumber)\"; $bad++ }\n"
        "}\n"
        "Write-Output \"checked $($args.Count) files, $bad problem(s)\"\n"
        "exit $bad\n")
    r = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(scan), *files],
                       capture_output=True, text=True, timeout=180)
    assert r.returncode == 0, r.stdout + r.stderr
    assert f"checked {len(files)} files, 0 problem(s)" in r.stdout


@pytest.mark.parametrize("marker,want", [
    (None, "frontend stats ABI missing (no GENLOCK_STATS_ABI.txt), new obs.dll v4; "
           "frontend output stats ABI missing (no GENLOCK_STATS_ABI.txt), new obs.dll v1"),
    (b"3\r\noutput_stats=1\r\n", "frontend stats ABI v3, new obs.dll v4"),
    (b"4\r\noutput_stats=2\r\n", "frontend output stats ABI v2, new obs.dll v1"),
    # the marker of a full deploy made before the output-stats line existed
    (b"4\r\n", "frontend output stats ABI missing (no output_stats line in GENLOCK_STATS_ABI.txt), new obs.dll v1"),
])
def test_the_whole_fast_program_refuses_and_changes_nothing_1302(tmp_path, marker, want):
    """The emitted FAST program, run whole in pwsh against a box without the right marker: exit 13 at
    step (0f), before the obs-websocket read; the install, the stage and the backup root unchanged."""
    stage = tmp_path / "stage"
    stage.mkdir()
    (stage / "obs.dll").write_bytes(b"NEW")
    obs = tmp_path / "obs"
    (obs / "bin" / "64bit").mkdir(parents=True)
    (obs / "bin" / "64bit" / "obs.dll").write_bytes(b"OLD")
    (obs / "GENLOCK_BUILD_SHA.txt").write_text("old\n")
    if marker is not None:
        (obs / "GENLOCK_STATS_ABI.txt").write_bytes(marker)
    backup = tmp_path / "backup"
    before = sorted((p.relative_to(tmp_path), p.read_bytes() if p.is_file() else None) for p in tmp_path.rglob("*"))
    prog = tmp_path / "deploy.ps1"
    prog.write_text(_program("stream", "fast", "4:1", stage=str(stage), obs_dir=str(obs), backup=str(backup)))
    r = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(prog)],
                       capture_output=True, text=True, timeout=180)
    assert r.returncode == 13, r.stdout + r.stderr
    assert f"FAST DEPLOY REFUSED: {want}{REQUIRED}. Nothing on this box was changed." in r.stdout
    assert "issue 1367 clean close" not in r.stdout + r.stderr
    after = sorted((p.relative_to(tmp_path), p.read_bytes() if p.is_file() else None)
                   for p in tmp_path.rglob("*") if p != prog)
    assert after == before
    assert not backup.exists()


@pytest.mark.parametrize("new,refused", [("4:1", None), ("4:2", "frontend output stats ABI v1, new obs.dll v2"),
                                         ("5:1", "frontend stats ABI v4, new obs.dll v5")])
def test_the_marker_the_full_program_writes_passes_the_fast_gate_1302(tmp_path, new, refused):
    """Writer and reader agree: the FULL program's step (5b), run in pwsh with the program's own
    Write-MarkerAtomic, writes a marker the FAST step (0f) accepts for the same pair and refuses,
    naming the struct, for another."""
    box = tmp_path / "obs"
    box.mkdir()
    full = _program("stream", "full", "4:1")
    writer = full[full.index("function Write-MarkerAtomic("):full.index("# (6) sha256 verify")]
    gate = _lib('genlock_fast_abi_gate_ps fast "$2"', new)
    assert gate.returncode == 0, gate.stderr
    script = tmp_path / "roundtrip.ps1"
    script.write_text("$ErrorActionPreference = 'Stop'\n"
                      f"$obsDir = '{box}'\n"
                      # the writer block also writes the other markers; they are harmless here
                      + writer.replace("(Get-Date -Format o)", "'2026-10-07T00:00:00'")
                      + "\n" + gate.stdout + "\nWrite-Host 'HARNESS PAST THE GATE'\nexit 0\n")
    r = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(script)],
                       capture_output=True, text=True, timeout=120)
    # Set-Content ends each line in the platform newline: CRLF on the box, LF under pwsh on Linux
    assert (box / "GENLOCK_STATS_ABI.txt").read_bytes().replace(b"\r\n", b"\n") == b"4\noutput_stats=1\n"
    if refused is None:
        assert r.returncode == 0, r.stdout + r.stderr
        assert "HARNESS PAST THE GATE" in r.stdout
    else:
        assert r.returncode == 13, r.stdout + r.stderr
        assert f"FAST DEPLOY REFUSED: {refused}{REQUIRED}." in r.stdout


@pytest.mark.parametrize("which", ["MARKERS", "SETUP_IMAG"])
def test_the_marker_the_linux_writer_writes_passes_the_verdict_1302(tmp_path, which):
    """The Linux leg's genlock_write_markers writes the marker the reference verdict accepts for the
    same pair (the verdict's text is what the (0f) gate transcribes)."""
    d = tmp_path / "m"
    src = MARKERS if which == "MARKERS" else SETUP_IMAG
    r = _markers('genlock_write_markers "$2" g d "" 4:1', d, which=src)
    assert r.returncode == 0, r.stderr
    assert (d / "GENLOCK_STATS_ABI.txt").read_bytes() == b"4\noutput_stats=1\n"
    v = _lib('rc=0; genlock_fast_abi_verdict 4:1 present "$(cat "$2")" || rc=$?; echo "rc=$rc"',
             d / "GENLOCK_STATS_ABI.txt")
    assert v.stdout == OK41 + "\nrc=0\n", v.stderr


# --- the planner ---------------------------------------------------------------------------------

def _plan(tmp_path, mode, sha, boxes="stream"):
    return subprocess.run([str(FLEET), "--plan", "--run-id", "RUN1302", "--sha", sha, "--stage", str(tmp_path),
                           "--boxes", boxes, f"--{mode}"], capture_output=True, text=True, timeout=180, cwd=REPO)


def test_plan_fast_reads_the_abi_of_the_deployed_commit_1302(tmp_path):
    sha = _head_sha()
    out = _repo_abi("OBS_GENLOCK_OUTPUT_STATS_VERSION")
    r = _plan(tmp_path, "fast", sha)
    assert r.returncode == 0, r.stderr
    assert f"\n$abiNew    = '{_repo_abi()}'\n" in r.stdout
    assert f"\n$abiNewOut = '{out}'\n" in r.stdout
    assert f"genlock stats ABI of {sha}: v{_repo_abi()}, output stats v{out}" in r.stderr


def test_plan_fast_refuses_an_unreadable_abi_1302(tmp_path):
    r = _plan(tmp_path, "fast", "deadbeefdeadbeef")
    assert r.returncode == 3, r.stdout + r.stderr
    assert "cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr and "--fast" in r.stderr
    assert "OBS_GENLOCK_OUTPUT_STATS_VERSION" in r.stderr
    assert "$ErrorActionPreference" not in r.stdout


def test_plan_full_records_the_abi_on_windows_and_imag_1302(tmp_path):
    sha = _head_sha()
    stats, out = _repo_pair().split(":")
    r = _plan(tmp_path, "full", sha, "stream,imag")
    assert r.returncode == 0, r.stderr
    assert f"Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt') @('{stats}', 'output_stats={out}')" in r.stdout
    assert f"genlock_write_markers \"$MARKER_DIR\" '{sha}' '{sha}' '' '{stats}:{out}'" in r.stdout


def test_plan_full_with_an_unreadable_abi_removes_the_marker_1302(tmp_path):
    r = _plan(tmp_path, "full", "deadbeefdeadbeef", "stream,imag")
    assert r.returncode == 0, r.stderr
    assert "WARNING: cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" in r.stdout
    assert "genlock_write_markers \"$MARKER_DIR\" 'deadbeefdeadbeef' 'deadbeefdeadbeef' '' ''" in r.stdout


def test_execute_mode_stages_the_abi_before_strih_lx_is_touched_1302():
    s = FLEET.read_text()
    main = s[s.index("main() {"):]
    resolve = main.index('stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/.." "$mode" "$(fleet_boxes_swap_obs_dll "$boxes")" 1)" || exit 3',
                         main.index("# --- execute mode"))
    prepare = main.index('strih_lx_prepare "$sha"')
    stage = main.index('genlock_stats_abi_stage "$STRIH_LX_PREP_WORK/bundle" "$stats_abi" || exit 3')
    apply_ = main.index('strih_lx_apply "$sha"')
    assert resolve < prepare < stage < apply_
    assert '"$yes" "$stats_abi"' in main
    assert 'build_imag_deploy_program "$imag_stage" \'/opt/obs-genlock\' \'/opt/obs-backup\' "$sha" "$sha" "$RETENTION_KEEP" "$yes" "$stats_abi"' in main


def test_plan_reads_without_network_and_names_the_boxes_1302():
    s = FLEET.read_text()
    main = s[s.index("main() {"):]
    plan = main[:main.index("# --- execute mode")]
    assert 'stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/.." "$mode" "$(fleet_boxes_swap_obs_dll "$boxes")")" || exit 3' in plan
    # the execute mode's own Windows-box switch reads the same predicate (one place names the boxes)
    assert 'want_win="$(fleet_boxes_swap_obs_dll "$boxes")"' in main


@pytest.mark.parametrize("boxes", ["imag", "strih-lx", "strih-lx,imag"])
def test_plan_fast_without_a_windows_box_never_refuses_an_unknown_abi_1302(tmp_path, boxes):
    """--fast swaps an obs.dll only on stream / resolume; a Linux-only run deploys the full bundle."""
    r = _plan(tmp_path, "fast", "deadbeefdeadbeef", boxes)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARNING: cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr
    assert "ERROR: cannot read" not in r.stderr


@pytest.mark.parametrize("boxes", ["stream", "resolume", "strih-lx,stream", "imag,resolume"])
def test_plan_fast_with_a_windows_box_refuses_an_unknown_abi_1302(tmp_path, boxes):
    r = _plan(tmp_path, "fast", "deadbeefdeadbeef", boxes)
    assert r.returncode == 3, r.stdout + r.stderr


@pytest.fixture()
def stale_clone(tmp_path):
    """An origin at stats v3, a clone of it, then origin moves to v4: the clone lacks the v4 commit."""
    origin = tmp_path / "origin"
    clone = tmp_path / "clone"
    git = ["-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    subprocess.run(["git", "init", "-q", str(origin)], check=True)
    obs_h = origin / "vendor" / "obs-studio" / "libobs" / "obs.h"
    obs_h.parent.mkdir(parents=True)
    obs_h.write_text("#define OBS_GENLOCK_STATS_VERSION 3\n#define OBS_GENLOCK_OUTPUT_STATS_VERSION 1\n")
    subprocess.run(["git", "-C", str(origin), *git, "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(origin), *git, "commit", "-q", "-m", "v3"], check=True)
    subprocess.run(["git", "clone", "-q", str(origin), str(clone)], check=True)
    obs_h.write_text("#define OBS_GENLOCK_STATS_VERSION 4\n#define OBS_GENLOCK_OUTPUT_STATS_VERSION 1\n")
    subprocess.run(["git", "-C", str(origin), *git, "commit", "-q", "-am", "v4"], check=True)
    sha4 = subprocess.run(["git", "-C", str(origin), "rev-parse", "HEAD"], capture_output=True, text=True,
                          check=True).stdout.strip()
    return clone, sha4


def test_execute_mode_fetches_origin_once_before_giving_up_1302(stale_clone):
    clone, sha4 = stale_clone
    r = _lib('rc=0; genlock_stats_abi_resolve "$2" "$3" fast 1 || rc=$?; echo "rc=$rc"', sha4, clone)
    assert r.stdout == "rc=3\n", r.stderr
    r = _lib('genlock_stats_abi_resolve "$2" "$3" fast 1 1', sha4, clone)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "4:1\n"
    assert "fetching origin once" in r.stderr


def test_an_abbreviated_sha_is_never_fetched_1302(stale_clone):
    """A fetch needs the full object id; a short SHA (every test SHA) never reaches the network."""
    clone, sha4 = stale_clone
    r = _lib('rc=0; genlock_stats_abi_resolve "$2" "$3" fast 1 1 || rc=$?; echo "rc=$rc"', sha4[:12], clone)
    assert r.stdout == "rc=3\n", r.stderr
    assert "fetching origin once" not in r.stderr


@pytest.mark.parametrize("box", BOXES)
def test_a_full_deploy_clears_the_marker_before_the_copy_1302(box):
    """A copy that fails half way must not leave a new frontend under the old marker."""
    p = _program(box, "full", "4:1")
    clear = p.index("# (3c) issue 1302")
    assert p.index("# (3) Back up") < clear < p.index("# (4) FULL bundle")
    seg = p[clear:p.index("# (4) FULL bundle")]
    assert "$abiStale = Join-Path $obsDir 'GENLOCK_STATS_ABI.txt'" in seg
    assert "if (Test-Path -LiteralPath $abiStale) { Remove-Item -LiteralPath $abiStale -Force -ErrorAction Stop }" in seg
    assert p.index("# (4) FULL bundle") < p.index(WRITE_ABI_41)
    assert "(3c) issue 1302" not in _program(box, "fast", "4:1")


@pytest.mark.parametrize("state,rc", [("file", 0), ("missing", 0), ("blocked", 1)])
def test_the_clear_before_the_copy_is_fail_closed_1302(tmp_path, state, rc):
    """A marker the (3c) step cannot remove stops the program before the copy (never a silent
    no-op that would leave the old version naming a half-copied frontend)."""
    box = tmp_path / "obs"
    box.mkdir()
    marker = box / "GENLOCK_STATS_ABI.txt"
    if state in ("file", "blocked"):
        marker.write_text("3\r\n")
    block = _lib("genlock_stats_abi_clear_ps full").stdout
    assert "(3c) issue 1302" in block
    script = tmp_path / "clear.ps1"
    script.write_text("$ErrorActionPreference = 'Stop'\n"
                      f"$obsDir = '{box}'\n" + block + "\nWrite-Host 'HARNESS AFTER THE CLEAR'\nexit 0\n")
    if state == "blocked":
        # the Linux stand-in for a file another process holds open on Windows: a read-only parent
        # directory makes the removal a NON-terminating error, which SilentlyContinue would swallow
        if os.geteuid() == 0:
            pytest.fail("run as a non-root user: root ignores the read-only directory this case needs")
        box.chmod(0o555)
    try:
        r = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(script)],
                           capture_output=True, text=True, timeout=120)
    finally:
        box.chmod(0o755)
    if rc == 0:
        assert r.returncode == 0, r.stdout + r.stderr
        assert "HARNESS AFTER THE CLEAR" in r.stdout
        assert not marker.exists()
    else:
        # a named failure with the program's copy-failure code, never a raw exception (OBS is
        # stopped at this point, so the operator must be told what to bring back)
        assert r.returncode == 4, r.stdout + r.stderr
        assert "(3c) FAILED: could not remove" in r.stdout
        assert "nothing was copied" in r.stdout
        assert "HARNESS AFTER THE CLEAR" not in r.stdout
        assert marker.exists()


@pytest.mark.parametrize("boxes,want", [("stream", "1"), ("resolume", "1"), ("imag,resolume", "1"),
                                        ("strih-lx,stream", "1"), ("strih-lx", "0"), ("imag", "0"),
                                        ("strih-lx,imag", "0"), ("", "0"), ("strih", "0")])
def test_the_per_box_table_names_the_boxes_a_fast_deploy_swaps_obs_dll_on_1302(boxes, want):
    r = _bash('. "$1"; fleet_boxes_swap_obs_dll "$2"', SCRIPTS / "lib" / "genlock-fleet-boxes.sh", boxes)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want + "\n"


def test_a_failed_fetch_names_why_1302(stale_clone):
    clone, _ = stale_clone
    absent = "b" * 40  # a well-formed commit id the origin does not have
    r = _lib('rc=0; genlock_stats_abi_resolve "$2" "$3" fast 1 1 || rc=$?; echo "rc=$rc"', absent, clone)
    assert r.stdout == "rc=3\n", r.stderr
    assert f"git fetch origin {absent} failed" in r.stderr


@pytest.fixture(scope="module")
def output_repo(tmp_path_factory):
    """obs.h at stats v4 with the output stats struct at v1, then at v2, then without its define."""
    repo = tmp_path_factory.mktemp("output_repo")
    git = ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    obs_h = repo / "vendor" / "obs-studio" / "libobs" / "obs.h"
    obs_h.parent.mkdir(parents=True)
    shas = {}
    for tag, out in (("out1", "#define OBS_GENLOCK_OUTPUT_STATS_VERSION 1\n"),
                     ("out2", "#define OBS_GENLOCK_OUTPUT_STATS_VERSION 2\n"), ("none", "")):
        obs_h.write_text("#define OBS_GENLOCK_STATS_VERSION 4\n" + out)
        subprocess.run(git + ["add", "-A"], check=True)
        subprocess.run(git + ["commit", "-q", "-m", tag], check=True)
        shas[tag] = subprocess.run(git + ["rev-parse", "HEAD"], capture_output=True, text=True,
                                   check=True).stdout.strip()
    return repo, shas


@pytest.mark.parametrize("tag,mode,swaps,rc,out", [
    ("out1", "fast", "1", 0, "4:1"), ("out2", "fast", "1", 0, "4:2"), ("none", "fast", "1", 3, ""),
    ("out2", "full", "1", 0, "4:2"), ("out2", "fast", "0", 0, "4:2"),
    ("none", "full", "1", 0, ""), ("none", "fast", "0", 0, ""),
])
def test_the_planner_reads_the_output_stats_version_too_1302(output_repo, tag, mode, swaps, rc, out):
    """The gate now compares OBS_GENLOCK_OUTPUT_STATS_VERSION on the box, so the planner carries it in
    the pair and never refuses a version by its value (the old plan-time stopgap, retired). Only an
    UNREADABLE define refuses a fast obs.dll swap (it cannot be compared); any other run removes the
    marker, naming the define it could not read."""
    repo, shas = output_repo
    r = _lib('rc=0; out="$(genlock_stats_abi_resolve "$2" "$3" "$4" "$5")" || rc=$?; echo "rc=$rc out=$out"',
             shas[tag], repo, mode, swaps)
    assert r.returncode == 0, r.stderr
    assert r.stdout == f"rc={rc} out={out}\n", r.stderr
    if tag == "none":
        assert "cannot read OBS_GENLOCK_OUTPUT_STATS_VERSION" in r.stderr
        assert "cannot read OBS_GENLOCK_STATS_VERSION" not in r.stderr
    assert "deploy --full" not in r.stderr or rc == 3


def test_the_plan_time_output_stats_stopgap_is_retired_1302():
    s = LIB.read_text()
    assert "GENLOCK_STATS_ABI_OUTPUT_COVERED" not in s
    assert "does not compare that struct yet" not in s


# --- the gate's premise: every stats struct change bumps a version the gate reads -----------------

def _struct_body(name):
    """The body of `struct <name> {...};` in obs.h, comments stripped, whitespace collapsed."""
    text = OBS_H.read_text()
    start = text.index(f"struct {name} {{")
    body = text[start:text.index("\n};", start) + 3]
    body = re.sub(r"/\*.*?\*/", "", body, flags=re.S)
    body = re.sub(r"//[^\n]*", "", body)
    return " ".join(body.split())


def _define(name):
    found = re.findall(rf"^\s*#\s*define\s+{name}\s+(\d+)\b", OBS_H.read_text(), re.M)
    assert len(found) == 1, (name, found)
    return found[0]


# The struct bodies the pinned versions describe (the first 16 hex digits of the sha256 of the body,
# comments stripped, whitespace collapsed). A field change without a version bump would be invisible
# to the fast-deploy gate (it compares versions), so a change here must bump the version AND update
# the pin in the same commit.
STATS_STRUCT_PINS = {"4": "9d1269d58919eb2d"}
OUTPUT_STATS_STRUCT_PINS = {"1": "ba0f22422814c0f1"}


def test_a_stats_struct_change_bumps_its_version_1302():
    import hashlib
    version = _define("OBS_GENLOCK_STATS_VERSION")
    digest = hashlib.sha256(_struct_body("obs_genlock_stats").encode()).hexdigest()[:16]
    assert STATS_STRUCT_PINS.get(version) == digest, (
        f"struct obs_genlock_stats changed (sha256 {digest}) at OBS_GENLOCK_STATS_VERSION {version}: a "
        "struct change must bump OBS_GENLOCK_STATS_VERSION (the fast-deploy gate compares versions, issue "
        "1302), then pin the new body here")


def test_an_output_stats_struct_change_bumps_its_version_1302():
    """The frontend keeps struct obs_genlock_output_stats on its stack too, filled by obs.dll with no
    size. The fast-deploy gate records and compares OBS_GENLOCK_OUTPUT_STATS_VERSION (ROZHODNUTÉ
    6030159870 item 1), so any version may ship -- but a field change without a bump would be
    invisible to it, exactly like the source stats struct above."""
    import hashlib
    version = _define("OBS_GENLOCK_OUTPUT_STATS_VERSION")
    digest = hashlib.sha256(_struct_body("obs_genlock_output_stats").encode()).hexdigest()[:16]
    assert OUTPUT_STATS_STRUCT_PINS.get(version) == digest, (
        f"struct obs_genlock_output_stats changed (sha256 {digest}) at OBS_GENLOCK_OUTPUT_STATS_VERSION "
        f"{version}: a struct change must bump OBS_GENLOCK_OUTPUT_STATS_VERSION (the fast-deploy gate "
        "compares versions, issue 1302), then pin the new body here")


# --- the Linux legs --------------------------------------------------------------------------------

def _markers(script, *args, which=MARKERS):
    """`script` with genlock_write_markers sourced from `which`; its own arguments are $2, $3, ...
    setup-imag.sh is sourced the way tests/deploy_genlock_fleet.rs sources it (no -e)."""
    return _bash('. "$1"\nset +e\n' + script, which, *args, strict=which == MARKERS)


@pytest.mark.parametrize("which", [MARKERS, SETUP_IMAG], ids=["lib", "setup-imag-inline"])
def test_write_markers_records_or_removes_the_abi_1302(tmp_path, which):
    d = tmp_path / "m"
    r = _markers('genlock_write_markers "$2" g d "" 4:1', d, which=which)
    assert r.returncode == 0, r.stderr
    assert (d / "GENLOCK_STATS_ABI.txt").read_text() == "4\noutput_stats=1\n"
    assert (d / "GENLOCK_BUILD_SHA.txt").read_text() == "g\n"
    # a later deploy that cannot name its ABI removes the marker an older one left
    for args in ('g d', 'g d ""', 'g d "" ""'):
        (d / "GENLOCK_STATS_ABI.txt").write_text("3\noutput_stats=1\n")
        r = _markers(f'genlock_write_markers "$2" {args}', d, which=which)
        assert r.returncode == 0, r.stderr
        assert not (d / "GENLOCK_STATS_ABI.txt").exists(), args
    # anything but a complete pair (incl. a bare stats version, the value before this change) is
    # removed with a note: a marker naming one struct would pass a fast deploy unchecked on the other
    for bad in ("4", "4x", "4:", ":1", "4:01", "4:1:2", "4:1 "):
        (d / "GENLOCK_STATS_ABI.txt").write_text("3\noutput_stats=1\n")
        r = _markers('genlock_write_markers "$2" g d "" "$3"', d, bad, which=which)
        assert r.returncode == 0, r.stderr
        assert not (d / "GENLOCK_STATS_ABI.txt").exists(), bad
        assert "is not a <stats>:<output_stats> pair" in r.stderr, bad
    assert not [p for p in d.iterdir() if ".tmp" in p.name]


def test_setup_imag_inline_markers_match_the_lib_with_the_abi_1302(tmp_path):
    at = "2026-10-07T12:00:00+00:00"
    for n, abi in enumerate(("4:1", None, "bad", "4", "12:3")):
        a, b = tmp_path / f"a{n}", tmp_path / f"b{n}"
        for d in (a, b):
            d.mkdir()
            (d / "GENLOCK_STATS_ABI.txt").write_text("3\n")
        call = 'genlock_write_markers "$2" shaG shaD "$3"' + ("" if abi is None else ' "$4"')
        extra = () if abi is None else (abi,)
        ra = _markers(call, a, at, *extra, which=SETUP_IMAG)
        rb = _markers(call, b, at, *extra, which=MARKERS)
        assert ra.returncode == rb.returncode == 0, ra.stderr + rb.stderr
        files_a = {p.name: p.read_bytes() for p in a.iterdir()}
        files_b = {p.name: p.read_bytes() for p in b.iterdir()}
        assert files_a == files_b, abi


def test_the_staged_bundle_file_1302(tmp_path):
    d = tmp_path / "bundle"
    d.mkdir()
    r = _lib('genlock_stats_abi_stage "$2" 4:1', d)
    assert r.returncode == 0, r.stderr
    assert (d / "GENLOCK_STATS_ABI.txt").read_text() == "4\noutput_stats=1\n"
    for unknown in ("", "4", "4:", "4:x"):
        (d / "GENLOCK_STATS_ABI.txt").write_text("4\noutput_stats=1\n")
        r = _lib('genlock_stats_abi_stage "$2" "$3"', d, unknown)
        assert r.returncode == 0, r.stderr
        assert not (d / "GENLOCK_STATS_ABI.txt").exists(), unknown
    r = _lib('rc=0; genlock_stats_abi_stage "$2" 4:1 || rc=$?; echo "rc=$rc"', tmp_path / "absent")
    assert r.stdout == "rc=1\n"


@pytest.mark.parametrize("marker,pair", [
    (b"4\noutput_stats=1\n", "4:1"), (b"4\r\noutput_stats=1\r\n", "4:1"), (b"12\noutput_stats=3", "12:3"),
    (b"4\n", ""), (b"", ""), (b"4\noutput_stats=1\nx\n", ""), (b"4\noutput_stats=x\n", ""),
    (b"x\noutput_stats=1\n", ""), (None, ""),
])
def test_the_pair_read_back_from_a_marker_1302(tmp_path, marker, pair):
    """setup-strih.sh reads the planner's staged marker back into the pair it hands genlock_write_markers:
    only a complete, well-formed marker yields one (anything else = unknown = the marker is removed)."""
    f = tmp_path / "GENLOCK_STATS_ABI.txt"
    if marker is not None:
        f.write_bytes(marker)
    r = _lib('p="$(genlock_stats_abi_pair_from_marker 2>/dev/null < "$2" || true)"; echo "pair=$p"', f)
    assert r.returncode == 0, r.stderr
    assert r.stdout == f"pair={pair}\n"


def test_the_staged_marker_round_trips_to_the_install_dir_1302(tmp_path):
    """What the planner stages is what setup-strih.sh's markers record on the box."""
    bundle, install = tmp_path / "bundle", tmp_path / "opt"
    bundle.mkdir()
    r = _lib('genlock_stats_abi_stage "$2" 4:1', bundle)
    assert r.returncode == 0, r.stderr
    r = _bash('. "$1"; . "$2"; p="$(genlock_stats_abi_pair_from_marker 2>/dev/null < "$3/GENLOCK_STATS_ABI.txt" || true)"\n'
              'genlock_write_markers "$4" g d "" "$p"', LIB, MARKERS, bundle, install)
    assert r.returncode == 0, r.stderr
    assert (install / "GENLOCK_STATS_ABI.txt").read_bytes() == (bundle / "GENLOCK_STATS_ABI.txt").read_bytes()


def test_setup_strih_passes_the_staged_abi_to_the_markers_1302():
    s = SETUP_STRIH.read_text()
    assert '. "${HERE}/lib/genlock-stats-abi.sh"' in s
    read = s.index('SABI="$(genlock_stats_abi_pair_from_marker 2>/dev/null < "${STRIH_LX_BUNDLE_SRC%/}/GENLOCK_STATS_ABI.txt" || true)"')
    write = s.index('genlock_write_markers "$GENLOCK_DIR" "$GSHA" "$DSHA" "" "$SABI" || fail "genlock_write_markers failed"')
    assert s.index('cp -a "${STRIH_LX_BUNDLE_SRC%/}/." "$GENLOCK_DIR/"') < read < write


@pytest.mark.parametrize("abi,want", [("4:1", "'' '4:1'"), ("", "'' ''"), ("4", "'' ''"), ("4:x", "'' ''"),
                                      ("4'; touch INJECTED; '", "'' ''"), ("4:1'; touch INJECTED; '", "'' ''")])
def test_the_imag_program_passes_the_abi_to_the_markers_1302(abi, want):
    r = _bash('. "$1"; build_imag_deploy_program /tmp/genlock-stage-R /opt/obs-genlock /opt/obs-backup '
              'SHA789 DSHA789 3 0 "$2"', FLEET, abi)
    assert r.returncode == 0, r.stderr
    assert f"genlock_write_markers \"$MARKER_DIR\" 'SHA789' 'DSHA789' {want}" in r.stdout
    assert "touch INJECTED" not in r.stdout
