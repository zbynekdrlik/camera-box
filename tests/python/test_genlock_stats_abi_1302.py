"""issue 1302 -- a FAST genlock deploy refuses an obs.dll whose stats ABI differs from the frontend's.

`deploy-genlock-fleet.sh --fast` swaps obs.dll alone and keeps the frontend (obs64.exe) of the last
full-bundle deploy. The frontend allocates `struct obs_genlock_stats` on its stack and passes no size,
so a newer obs.dll that fills a bigger struct (OBS_GENLOCK_STATS_VERSION 3 -> 4) writes past the
frontend's copy and OBS crashes. Live on 7.10.2026: stream ran a v3 build (a33d91a8e, its last deploy
a FAST one) while dev carried v4. Design: issue 1302 comment 6028838843 (Approach 1, part 2).

This suite pins:
  * the pure parts of scripts/lib/genlock-stats-abi.sh: the obs.h reader (the repo's own obs.h and a
    synthetic two-commit repo), the read at a commit, and genlock_fast_abi_verdict over its vectors;
  * the emitted PowerShell gate RUN in pwsh against a fake install dir, on the same vectors, with the
    same verdict and the same refusal text as the bash decision;
  * the emitted deploy program: FAST gates at step (0f), right after the path preflight and before
    anything on the box changes (the obs-websocket read, the power plan, AutoHotkey64, the keep-alive
    tasks, the stop, the backup, the copy); FULL records GENLOCK_STATS_ABI.txt next to the other
    markers, or removes it when the planner could not read the version; the whole FAST program, run in
    pwsh against a box without the marker, exits 13 having changed nothing;
  * the planner: --plan reads the version from `git show <sha>:vendor/obs-studio/libobs/obs.h`,
    REFUSES --fast (exit 3) when it cannot, and warns + removes on --full;
  * the Linux legs: genlock_write_markers' 5th argument (and setup-imag.sh's inline copy, behaviour for
    behaviour), the staged bundle file, setup-strih.sh passing it on, the imag program.

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


def _repo_abi():
    """The version in the checkout's obs.h, read independently of the bash reader."""
    found = re.findall(r"^\s*#\s*define\s+OBS_GENLOCK_STATS_VERSION\s+(\d+)\b", OBS_H.read_text(), re.M)
    assert len(found) == 1, found
    return found[0]


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

VERDICTS = [
    ("4", "present", "4\r\n", 0, "OK frontend stats ABI v4 == new obs.dll v4"),
    ("4", "present", " 4 \n", 0, "OK frontend stats ABI v4 == new obs.dll v4"),
    ("44", "present", "4 4", 0, "OK frontend stats ABI v44 == new obs.dll v44"),
    ("4", "present", "3\r\n", 1, "REFUSED frontend stats ABI v3, new obs.dll v4" + REQUIRED),
    ("3", "present", "4", 1, "REFUSED frontend stats ABI v4, new obs.dll v3" + REQUIRED),
    ("4", "missing", "", 1, "REFUSED frontend stats ABI missing (no GENLOCK_STATS_ABI.txt), new obs.dll v4" + REQUIRED),
    ("4", "present", "", 1,
     "REFUSED frontend stats ABI unreadable (GENLOCK_STATS_ABI.txt is not a version), new obs.dll v4" + REQUIRED),
    ("4", "present", "abc", 1,
     "REFUSED frontend stats ABI unreadable (GENLOCK_STATS_ABI.txt is not a version), new obs.dll v4" + REQUIRED),
    ("4", "present", "04", 1,
     "REFUSED frontend stats ABI unreadable (GENLOCK_STATS_ABI.txt is not a version), new obs.dll v4" + REQUIRED),
    ("", "present", "4", 1, "REFUSED frontend stats ABI v4, new obs.dll unknown" + REQUIRED),
    ("x", "missing", "", 1, "REFUSED frontend stats ABI missing (no GENLOCK_STATS_ABI.txt), new obs.dll unknown" + REQUIRED),
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
    r = _lib('rc=0; genlock_fast_abi_verdict 4 bogus "" || rc=$?; echo "rc=$rc"')
    assert r.returncode == 0
    assert r.stdout == "rc=2\n"
    assert "MARKER_STATE" in r.stderr


# --- the PowerShell gate, RUN in pwsh, against the bash decision ---------------------------------

GATE_VECTORS = [
    (b"4\r\n", "4"), (b"4", "4"), (b"4\n", "4"), (b" 4 \r\n", "4"), (b"3\r\n", "4"), (b"4\r\n", "3"),
    (b"", "4"), (b"abc", "4"), (b"\xef\xbb\xbf4\r\n", "4"), (b"04", "4"), (b"4 4\r\n", "44"),
    (None, "4"), (None, ""), (b"4\r\n", ""), (b"4\r\n", "x"),
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
    r = _lib("genlock_fast_abi_gate_ps full 4")
    assert r.returncode == 0 and r.stdout == ""


# --- the emitted deploy program ------------------------------------------------------------------

@pytest.mark.parametrize("box", BOXES)
def test_the_fast_program_gates_before_anything_changes_1302(box):
    p = _program(box, "fast", "4")
    gate = p.index("# (0f) issue 1302")
    assert p.index("# (0) preflight") < gate
    assert p.index("if (-not (Test-Path $obsDir))") < gate
    # before the obs-websocket read, the power plan, AutoHotkey64, the keep-alive tasks, the stop,
    # the backup and the obs.dll copy
    for later in ("# (0a) issue 1367", "# (0b)", "# (1) ", "# (1b)", FORCE_LINE, "# (3) Back up",
                  "New-Item -ItemType Directory", "try { Copy-Item -Force $src $dst }"):
        assert gate < p.index(later), later
    seg = p[gate:p.index("# (0a) issue 1367")]
    assert "\n$abiNew  = '4'\n" in seg
    assert "$abiFile = Join-Path $obsDir 'GENLOCK_STATS_ABI.txt'" in seg
    assert "FAST DEPLOY REFUSED: frontend stats ABI $abiBoxText, new obs.dll ${abiNewText}" + REQUIRED in seg
    assert seg.index("FAST DEPLOY REFUSED") < seg.index("exit 13")
    assert p.count("# (0f) issue 1302") == 1
    # the fast deploy keeps the frontend, so it keeps the frontend's marker
    assert "GENLOCK_STATS_ABI.txt is left as it is" in p
    assert "Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in p
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in p


@pytest.mark.parametrize("box", BOXES)
def test_the_full_program_records_the_abi_next_to_the_build_sha_1302(box):
    p = _program(box, "full", "4")
    assert "(0f) issue 1302" not in p and "exit 13" not in p
    sha = p.index("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_BUILD_SHA.txt')")
    abi = p.index("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt') '4'")
    assert sha < abi < p.index("# (6) sha256 verify")
    # nothing removes the recorded marker after the copy (a clear BEFORE the copy is allowed)
    after_copy = p[p.index("# (4) FULL bundle"):]
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in after_copy


@pytest.mark.parametrize("box", BOXES)
def test_the_full_program_removes_the_marker_when_the_abi_is_unknown_1302(box):
    p = _program(box, "full", "")
    # the LAST removal: the one after the markers (a clear before the copy may come earlier)
    rm = p.rindex("Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')")
    assert p.index("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_BUILD_SHA.txt')") < rm < p.index("# (6) sha256 verify")
    assert "Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in p
    assert "a fast deploy will refuse until the next full-bundle deploy" in p


@pytest.mark.parametrize("mode", ["fast", "full"])
def test_an_injected_abi_never_reaches_the_program_1302(mode):
    p = _program("stream", mode, "4'; Remove-Item C:\\x -Recurse; '")
    assert "Remove-Item C:" not in p
    if mode == "fast":
        assert "\n$abiNew  = ''\n" in p
    else:
        assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" in p


def test_every_program_parses_and_uses_no_powershell_7_only_syntax_1302(tmp_path):
    files = []
    for box in BOXES:
        for mode in ("fast", "full"):
            for abi in ("4", ""):
                f = tmp_path / f"{box}-{mode}-{abi or 'unknown'}.ps1"
                f.write_text(_program(box, mode, abi))
                files.append(str(f))
    for fn, mode, abi in (("genlock_fast_abi_gate_ps", "fast", "4"), ("genlock_stats_abi_marker_ps", "full", "4"),
                          ("genlock_stats_abi_marker_ps", "full", ""), ("genlock_stats_abi_marker_ps", "fast", "4")):
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
    (None, "frontend stats ABI missing (no GENLOCK_STATS_ABI.txt), new obs.dll v4"),
    (b"3\r\n", "frontend stats ABI v3, new obs.dll v4"),
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
    prog.write_text(_program("stream", "fast", "4", stage=str(stage), obs_dir=str(obs), backup=str(backup)))
    r = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(prog)],
                       capture_output=True, text=True, timeout=180)
    assert r.returncode == 13, r.stdout + r.stderr
    assert f"FAST DEPLOY REFUSED: {want}{REQUIRED}. Nothing on this box was changed." in r.stdout
    assert "issue 1367 clean close" not in r.stdout + r.stderr
    after = sorted((p.relative_to(tmp_path), p.read_bytes() if p.is_file() else None)
                   for p in tmp_path.rglob("*") if p != prog)
    assert after == before
    assert not backup.exists()


# --- the planner ---------------------------------------------------------------------------------

def _plan(tmp_path, mode, sha, boxes="stream"):
    return subprocess.run([str(FLEET), "--plan", "--run-id", "RUN1302", "--sha", sha, "--stage", str(tmp_path),
                           "--boxes", boxes, f"--{mode}"], capture_output=True, text=True, timeout=180, cwd=REPO)


def test_plan_fast_reads_the_abi_of_the_deployed_commit_1302(tmp_path):
    sha = _head_sha()
    r = _plan(tmp_path, "fast", sha)
    assert r.returncode == 0, r.stderr
    assert f"\n$abiNew  = '{_repo_abi()}'\n" in r.stdout
    assert f"genlock stats ABI of {sha}: v{_repo_abi()}" in r.stderr


def test_plan_fast_refuses_an_unreadable_abi_1302(tmp_path):
    r = _plan(tmp_path, "fast", "deadbeefdeadbeef")
    assert r.returncode == 3, r.stdout + r.stderr
    assert "cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr and "--fast" in r.stderr
    assert "$ErrorActionPreference" not in r.stdout


def test_plan_full_records_the_abi_on_windows_and_imag_1302(tmp_path):
    sha = _head_sha()
    abi = _repo_abi()
    r = _plan(tmp_path, "full", sha, "stream,imag")
    assert r.returncode == 0, r.stderr
    assert f"Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt') '{abi}'" in r.stdout
    assert f"genlock_write_markers \"$MARKER_DIR\" '{sha}' '{sha}' '' '{abi}'" in r.stdout


def test_plan_full_with_an_unreadable_abi_removes_the_marker_1302(tmp_path):
    r = _plan(tmp_path, "full", "deadbeefdeadbeef", "stream,imag")
    assert r.returncode == 0, r.stderr
    assert "WARNING: cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" in r.stdout
    assert "genlock_write_markers \"$MARKER_DIR\" 'deadbeefdeadbeef' 'deadbeefdeadbeef' '' ''" in r.stdout


def test_execute_mode_stages_the_abi_before_strih_lx_is_touched_1302():
    s = FLEET.read_text()
    main = s[s.index("main() {"):]
    resolve = main.index('stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/.." "$mode" "$boxes" 1)" || exit 3',
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
    assert 'stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/.." "$mode" "$boxes")" || exit 3' in plan


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
    obs_h.write_text("#define OBS_GENLOCK_STATS_VERSION 3\n")
    subprocess.run(["git", "-C", str(origin), *git, "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(origin), *git, "commit", "-q", "-m", "v3"], check=True)
    subprocess.run(["git", "clone", "-q", str(origin), str(clone)], check=True)
    obs_h.write_text("#define OBS_GENLOCK_STATS_VERSION 4\n")
    subprocess.run(["git", "-C", str(origin), *git, "commit", "-q", "-am", "v4"], check=True)
    sha4 = subprocess.run(["git", "-C", str(origin), "rev-parse", "HEAD"], capture_output=True, text=True,
                          check=True).stdout.strip()
    return clone, sha4


def test_execute_mode_fetches_origin_once_before_giving_up_1302(stale_clone):
    clone, sha4 = stale_clone
    r = _lib('rc=0; genlock_stats_abi_resolve "$2" "$3" fast stream || rc=$?; echo "rc=$rc"', sha4, clone)
    assert r.stdout == "rc=3\n", r.stderr
    r = _lib('genlock_stats_abi_resolve "$2" "$3" fast stream 1', sha4, clone)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "4\n"
    assert "fetching origin once" in r.stderr


@pytest.mark.parametrize("box", BOXES)
def test_a_full_deploy_clears_the_marker_before_the_copy_1302(box):
    """A copy that fails half way must not leave a new frontend under the old marker."""
    p = _program(box, "full", "4")
    clear = p.index("# (3c) issue 1302")
    assert p.index("# (3) Back up") < clear < p.index("# (4) FULL bundle")
    seg = p[clear:p.index("# (4) FULL bundle")]
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" in seg
    assert p.index("# (4) FULL bundle") < p.index("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt') '4'")
    assert "(3c) issue 1302" not in _program(box, "fast", "4")


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


def test_the_output_stats_struct_is_pinned_until_the_gate_covers_it_1302():
    """The frontend keeps struct obs_genlock_output_stats on its stack too, filled by obs.dll with no
    size. The fast-deploy gate compares OBS_GENLOCK_STATS_VERSION only, so this struct must not
    change until the gate also records + compares OBS_GENLOCK_OUTPUT_STATS_VERSION."""
    import hashlib
    version = _define("OBS_GENLOCK_OUTPUT_STATS_VERSION")
    digest = hashlib.sha256(_struct_body("obs_genlock_output_stats").encode()).hexdigest()[:16]
    assert version == "1" and OUTPUT_STATS_STRUCT_PINS.get(version) == digest, (
        f"struct obs_genlock_output_stats changed (version {version}, sha256 {digest}): extend the fast-deploy "
        "stats-ABI gate (scripts/lib/genlock-stats-abi.sh, issue 1302) to record and compare "
        "OBS_GENLOCK_OUTPUT_STATS_VERSION first -- a new obs.dll filling a bigger output struct crashes an old "
        "frontend exactly like the source stats struct")


# --- the Linux legs --------------------------------------------------------------------------------

def _markers(script, *args, which=MARKERS):
    """`script` with genlock_write_markers sourced from `which`; its own arguments are $2, $3, ...
    setup-imag.sh is sourced the way tests/deploy_genlock_fleet.rs sources it (no -e)."""
    return _bash('. "$1"\nset +e\n' + script, which, *args, strict=which == MARKERS)


@pytest.mark.parametrize("which", [MARKERS, SETUP_IMAG], ids=["lib", "setup-imag-inline"])
def test_write_markers_records_or_removes_the_abi_1302(tmp_path, which):
    d = tmp_path / "m"
    r = _markers('genlock_write_markers "$2" g d "" 4', d, which=which)
    assert r.returncode == 0, r.stderr
    assert (d / "GENLOCK_STATS_ABI.txt").read_text() == "4\n"
    assert (d / "GENLOCK_BUILD_SHA.txt").read_text() == "g\n"
    # a later deploy that cannot name its ABI removes the marker an older one left
    for args in ('g d', 'g d ""', 'g d "" ""'):
        (d / "GENLOCK_STATS_ABI.txt").write_text("3\n")
        r = _markers(f'genlock_write_markers "$2" {args}', d, which=which)
        assert r.returncode == 0, r.stderr
        assert not (d / "GENLOCK_STATS_ABI.txt").exists(), args
    (d / "GENLOCK_STATS_ABI.txt").write_text("3\n")
    r = _markers('genlock_write_markers "$2" g d "" "4x"', d, which=which)
    assert r.returncode == 0, r.stderr
    assert not (d / "GENLOCK_STATS_ABI.txt").exists()
    assert "not a version" in r.stderr
    assert not [p for p in d.iterdir() if ".tmp" in p.name]


def test_setup_imag_inline_markers_match_the_lib_with_the_abi_1302(tmp_path):
    at = "2026-10-07T12:00:00+00:00"
    for n, abi in enumerate(("4", None, "bad")):
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
    r = _lib('genlock_stats_abi_stage "$2" 4', d)
    assert r.returncode == 0, r.stderr
    assert (d / "GENLOCK_STATS_ABI.txt").read_text() == "4\n"
    r = _lib('genlock_stats_abi_stage "$2" ""', d)
    assert r.returncode == 0, r.stderr
    assert not (d / "GENLOCK_STATS_ABI.txt").exists()
    r = _lib('rc=0; genlock_stats_abi_stage "$2" 4 || rc=$?; echo "rc=$rc"', tmp_path / "absent")
    assert r.stdout == "rc=1\n"


def test_setup_strih_passes_the_staged_abi_to_the_markers_1302():
    s = SETUP_STRIH.read_text()
    read = s.index('SABI="$(tr -d \'[:space:]\' 2>/dev/null < "${STRIH_LX_BUNDLE_SRC%/}/GENLOCK_STATS_ABI.txt" || true)"')
    write = s.index('genlock_write_markers "$GENLOCK_DIR" "$GSHA" "$DSHA" "" "$SABI" || fail "genlock_write_markers failed"')
    assert s.index('cp -a "${STRIH_LX_BUNDLE_SRC%/}/." "$GENLOCK_DIR/"') < read < write


@pytest.mark.parametrize("abi,want", [("4", "'' '4'"), ("", "'' ''"), ("4'; touch INJECTED; '", "'' ''")])
def test_the_imag_program_passes_the_abi_to_the_markers_1302(abi, want):
    r = _bash('. "$1"; build_imag_deploy_program /tmp/genlock-stage-R /opt/obs-genlock /opt/obs-backup '
              'SHA789 DSHA789 3 0 "$2"', FLEET, abi)
    assert r.returncode == 0, r.stderr
    assert f"genlock_write_markers \"$MARKER_DIR\" 'SHA789' 'DSHA789' {want}" in r.stdout
    assert "touch INJECTED" not in r.stdout
