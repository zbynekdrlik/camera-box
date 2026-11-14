"""issue 1302 -- every full-bundle genlock deploy records its frontend's stats ABI (GENLOCK_STATS_ABI.txt).

The OBS frontend allocates `struct obs_genlock_stats` AND `struct obs_genlock_output_stats` on its
stack and passes no size, so an obs.dll / libobs that fills a bigger struct than its frontend knows
writes past the frontend's copy and OBS crashes. Design: issue 1302 comment 6028838843; ROZHODNUTÉ
6030159870 item 1 extended the marker to the output-stats struct. Issue 1505 retired the Windows
obs.dll-only FAST deploy (the pinned obs release ships the full bundle only), and issue 1522 removed
its step-(0f) gate: every deploy ships the frontend with the library, so the pair is RECORDED, never
compared at deploy time.

The marker GENLOCK_STATS_ABI.txt is two lines: the stats version, then `output_stats=<N>`. The
planner passes the pair as ONE value `<stats>:<output_stats>` (e.g. `4:1`).

This suite pins:
  * the pure parts of scripts/lib/genlock-stats-abi.sh: the obs.h reader (the repo's own obs.h and a
    synthetic two-commit repo), the read at a commit, and the marker reader
    (genlock_stats_abi_pair_from_marker) over its vectors;
  * the emitted FULL Windows program: the marker cleared at (3c) before the copy (fail-closed, run in
    pwsh), recorded at (5b) next to the other markers or removed when the pair is unknown, and the
    marker the (5b) writer writes read back as the same pair; no Windows FAST program and no (0f) gate;
  * the planner: --plan reads imag's pair from `git show <sha>:vendor/obs-studio/libobs/obs.h`, and an
    unreadable define warns + removes the marker (it never refuses a run);
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
BOXES = ["stream-snv", "resolume-snv"]


def _utf8_locale():
    """The UTF-8 locale the bash side runs in: en_US.UTF-8 when the box has it (dev1, the operator's
    locale, where bash ranges like [1-9] also match non-ASCII digits), else C.UTF-8 (the CI runner)."""
    have = subprocess.run(["locale", "-a"], capture_output=True, text=True, check=True).stdout.split()
    for want in ("en_US.utf8", "C.utf8"):
        if want in have:
            return want
    pytest.fail(f"no UTF-8 locale on this box (locale -a: {have})")


UTF8_LOCALE = _utf8_locale()


def _bash(script, *args, strict=True):
    """Run `script` under the caller's strict mode, in a UTF-8 locale. Every variable value goes in as
    a positional ARGUMENT ($1, $2, ...), never into the script text: a value carrying a quote must
    stay data (a first draft interpolated an injection vector and its own `rm -rf /` ran in the
    harness)."""
    head = "set -euo pipefail\n" if strict else "set -uo pipefail\n"
    return subprocess.run(["bash", "-c", head + script, "harness", *map(str, args)],
                          capture_output=True, text=True, timeout=120,
                          env={**os.environ, "LC_ALL": UTF8_LOCALE})


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


def _program(box, abi, stage="C:\\stage", obs_dir="C:\\Program Files\\obs-studio", backup="C:\\obs-backup"):
    """The FULL Windows deploy program (issue 1522: the builder refuses any other mode)."""
    r = _bash(
        '. "$1"; build_windows_deploy_program "$2" full "$3" "$4" "$(fleet_box_ahk_mode "$2")" "$5" '
        '3 abc123 def456 0 "$6"',
        FLEET, box, stage, obs_dir, backup, abi,
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


# --- the emitted deploy program ------------------------------------------------------------------

WRITE_ABI_41 = "Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt') @('4', 'output_stats=1')"


def test_the_fast_program_and_its_gate_are_gone_1522():
    """issue 1522: the Windows FAST program and its step-(0f) stats-ABI gate are unreachable since issue
    1505 (the pinned release ships the full bundle only), so they are removed, not kept as dead code;
    the builder refuses a fast MODE by name (rc 2) and prints no program."""
    for f in (LIB, FLEET, SCRIPTS / "lib" / "genlock-plugin-deploy.sh"):
        text = f.read_text()
        for gone in ("genlock_fast_abi_verdict", "genlock_fast_abi_gate_ps", "PSFAST", "FAST DEPLOY REFUSED"):
            assert gone not in text, (f, gone)
    r = _bash('. "$1"; rc=0; build_windows_deploy_program stream-snv fast "C:\\s" "C:\\o" 0 "C:\\b" 3 a b 0 4:1 || rc=$?; '
              'echo "rc=$rc"', FLEET)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "rc=2\n", r.stdout
    assert "mode 'fast' refused" in r.stderr


@pytest.mark.parametrize("box", BOXES)
def test_the_full_program_records_the_abi_next_to_the_build_sha_1302(box):
    p = _program(box, "4:1")
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
    """Only a complete pair is recorded: a marker never names a struct the planner did not read."""
    p = _program(box, abi)
    # the LAST removal: the one after the markers (a clear before the copy may come earlier)
    rm = p.rindex("Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')")
    assert p.index("Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_BUILD_SHA.txt')") < rm < p.index("# (6) sha256 verify")
    assert "Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" not in p
    assert "removed until a deploy records a known pair" in p


@pytest.mark.parametrize("abi", [
    "4'; Write-Host INJECTED; '",
    "4:1'; Write-Host INJECTED; '",
    "4'; Write-Host INJECTED; ':1",
    # a ':' inside the payload moves the split, never past the validation
    "4'; Write-Host C:\\INJECTED; '",
])
def test_an_injected_abi_never_reaches_the_program_1302(abi):
    p = _program("stream-snv", abi)
    assert "INJECTED" not in p
    assert "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $obsDir 'GENLOCK_STATS_ABI.txt')" in p


def test_every_program_parses_and_uses_no_powershell_7_only_syntax_1302(tmp_path):
    files = []
    for box in BOXES:
        for abi in ("4:1", ""):
            f = tmp_path / f"{box}-{abi.replace(':', '_') or 'unknown'}.ps1"
            f.write_text(_program(box, abi))
            files.append(str(f))
    for fn, abi in (("genlock_stats_abi_marker_ps", "4:1"), ("genlock_stats_abi_marker_ps", ""),
                    ("genlock_stats_abi_clear_ps", "")):
        f = tmp_path / f"{fn}-{len(files)}.ps1"
        f.write_text(_lib(f'{fn} "$2"', abi).stdout)
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


@pytest.mark.parametrize("pair", ["4:1", "12:3"])
def test_the_marker_the_full_program_writes_reads_back_as_its_pair_1302(tmp_path, pair):
    """Writer and reader agree: the FULL program's step (5b), run in pwsh with the program's own
    Write-MarkerAtomic, writes a marker the bash reader (genlock_stats_abi_pair_from_marker) reads back
    as the same pair."""
    box = tmp_path / "obs"
    box.mkdir()
    full = _program("stream-snv", pair)
    writer = full[full.index("function Write-MarkerAtomic("):full.index("# (6) sha256 verify")]
    script = tmp_path / "writer.ps1"
    script.write_text("$ErrorActionPreference = 'Stop'\n"
                      f"$obsDir = '{box}'\n"
                      # the writer block also writes the other markers; they are harmless here
                      + writer.replace("(Get-Date -Format o)", "'2026-10-07T00:00:00'") + "\nexit 0\n")
    r = subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(script)],
                       capture_output=True, text=True, timeout=120)
    assert r.returncode == 0, r.stdout + r.stderr
    stats, out = pair.split(":")
    # Set-Content ends each line in the platform newline: CRLF on the box, LF under pwsh on Linux
    assert (box / "GENLOCK_STATS_ABI.txt").read_bytes().replace(b"\r\n", b"\n") == f"{stats}\noutput_stats={out}\n".encode()
    back = _lib('genlock_stats_abi_pair_from_marker < "$2"', box / "GENLOCK_STATS_ABI.txt")
    assert back.returncode == 0, back.stderr
    assert back.stdout == pair + "\n"


@pytest.mark.parametrize("which", ["MARKERS", "SETUP_IMAG"])
def test_the_marker_the_linux_writer_writes_reads_back_as_its_pair_1302(tmp_path, which):
    """The Linux leg's genlock_write_markers writes the marker the reader reads back as the same pair."""
    d = tmp_path / "m"
    src = MARKERS if which == "MARKERS" else SETUP_IMAG
    r = _markers('genlock_write_markers "$2" g d "" 4:1', d, which=src)
    assert r.returncode == 0, r.stderr
    assert (d / "GENLOCK_STATS_ABI.txt").read_bytes() == b"4\noutput_stats=1\n"
    v = _lib('genlock_stats_abi_pair_from_marker < "$2"', d / "GENLOCK_STATS_ABI.txt")
    assert v.returncode == 0, v.stderr
    assert v.stdout == "4:1\n"


# --- the planner ---------------------------------------------------------------------------------

def _plan(tmp_path, mode, sha, boxes="imag"):
    # issue 1505: the VENDOR-build stats ABI path (vendor obs.h at the anchor SHA) is imag's only: camera-box's
    # own Windows build is retired, a Windows box takes the pinned zip (tests/python/test_obs_release_windows_1505.py)
    # and a pin without that zip refuses it. The planner reads the real pin minus its OBS_RELEASE_WINDOWS_*
    # lines, so a Windows box in BOXES is refused here.
    pin = tmp_path / "obs-release-no-windows.pin"
    pin.write_text("".join(l + "\n" for l in (REPO / "scripts" / "obs-release.pin").read_text().splitlines()
                           if not l.startswith("OBS_RELEASE_WINDOWS_")))
    return subprocess.run([str(FLEET), "--plan", "--run-id", "RUN1302", "--sha", sha, "--stage", str(tmp_path),
                           "--boxes", boxes, f"--{mode}"], capture_output=True, text=True, timeout=180, cwd=REPO,
                          env={**os.environ, "OBS_RELEASE_PIN_FILE": str(pin)})


def test_plan_full_records_the_abi_on_imag_1302(tmp_path):
    sha = _head_sha()
    stats, out = _repo_pair().split(":")
    r = _plan(tmp_path, "full", sha, "imag")
    assert r.returncode == 0, r.stderr
    assert f"genlock_write_markers \"$MARKER_DIR\" '{sha}' '{sha}' '' '{stats}:{out}'" in r.stdout


def test_plan_full_with_an_unreadable_abi_removes_the_marker_1302(tmp_path):
    r = _plan(tmp_path, "full", "deadbeefdeadbeef", "imag")
    assert r.returncode == 0, r.stderr
    assert "WARNING: cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr
    assert "genlock_write_markers \"$MARKER_DIR\" 'deadbeefdeadbeef' 'deadbeefdeadbeef' '' ''" in r.stdout


@pytest.mark.parametrize("mode", ["full", "fast"])
@pytest.mark.parametrize("boxes", ["resolume-snv", "imag,resolume-snv"])
def test_plan_of_a_windows_box_without_the_pinned_zip_is_refused_never_a_vendor_build_1505(tmp_path, mode, boxes):
    """issue 1505: the vendor obs.h path no longer reaches a Windows box -- the pin without its zip refuses it."""
    r = _plan(tmp_path, mode, _head_sha(), boxes)
    assert r.returncode == 3, r.stdout + r.stderr
    assert "the pin carries no Windows zip" in r.stderr
    assert "$ErrorActionPreference" not in r.stdout


def test_execute_mode_stages_the_abi_before_strih_lx_is_touched_1302():
    s = FLEET.read_text()
    main = s[s.index("main() {"):]
    resolve = main.index('stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/.." 1)"',
                         main.index("# --- execute mode"))
    # issue 1487: the Linux boxes take the pinned obs release, its pair from the release's own manifest
    prepare = main.index('strih_lx_prepare "$lx_sha"')
    stage = main.index('genlock_stats_abi_stage "$STRIH_LX_PREP_WORK/bundle" "$(obs_release_stats_abi "$STRIH_LX_PREP_WORK/bundle" || true)" || exit 3')
    apply_ = main.index('strih_lx_apply_all "$lx_sha"')
    assert resolve < prepare < stage < apply_
    assert '"$yes" "$stats_abi"' in main
    assert 'build_imag_deploy_program "$imag_stage" \'/opt/obs-genlock\' \'/opt/obs-backup\' "$sha" "$sha" "$RETENTION_KEEP" "$yes" "$stats_abi"' in main


def test_plan_reads_without_network_and_names_the_boxes_1302():
    s = FLEET.read_text()
    main = s[s.index("main() {"):]
    plan = main[:main.index("# --- execute mode")]
    # issue 1505: only the boxes that still take an anchor build read the vendor obs.h (a Windows box on the
    # pinned zip reads its pair from the pinned manifest)
    assert '[ -z "$anchor_rest" ] || stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/..")"' in plan
    # issue 1522: the resolve has no refusal path any more, so no caller gates on its rc
    assert "genlock_stats_abi_resolve" not in main.replace('stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/..")"', "") \
        .replace('stats_abi="$(genlock_stats_abi_resolve "$sha" "$HERE/.." 1)"', "")
    # the execute mode's own Windows-box switch reads the same predicate (one place names the boxes)
    assert 'want_win="$(fleet_boxes_swap_obs_dll "$boxes")"' in main


@pytest.mark.parametrize("boxes,warned", [("imag", True), ("strih-snv", False), ("strih-snv,imag", True),
                                          ("stream-snv", False), ("strih-snv,stream-snv", False)])
def test_plan_fast_without_a_windows_box_never_refuses_an_unknown_abi_1302(tmp_path, boxes, warned):
    """A run without a Windows box never refuses an unknown pair (issue 1522: no run does). issue 1487: the
    setup-strih boxes take the pinned obs release, whose pair is its manifest's, so a run of only those boxes
    reads no vendor obs.h at all; imag's unreadable pair warns and removes the marker."""
    r = _plan(tmp_path, "fast", "deadbeefdeadbeef", boxes)
    assert r.returncode == 0, r.stdout + r.stderr
    assert ("WARNING: cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr) is warned
    assert "ERROR: cannot read" not in r.stderr


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
    # plan mode (no fetch): unreadable -> "" + the warning, never a refusal (issue 1522)
    r = _lib('rc=0; out="$(genlock_stats_abi_resolve "$2" "$3")" || rc=$?; echo "rc=$rc out=$out"', sha4, clone)
    assert r.stdout == "rc=0 out=\n", r.stderr
    assert "WARNING: cannot read OBS_GENLOCK_STATS_VERSION" in r.stderr
    r = _lib('genlock_stats_abi_resolve "$2" "$3" 1', sha4, clone)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "4:1\n"
    assert "fetching origin once" in r.stderr


def test_an_abbreviated_sha_is_never_fetched_1302(stale_clone):
    """A fetch needs the full object id; a short SHA (every test SHA) never reaches the network."""
    clone, sha4 = stale_clone
    r = _lib('rc=0; out="$(genlock_stats_abi_resolve "$2" "$3" 1)" || rc=$?; echo "rc=$rc out=$out"', sha4[:12], clone)
    assert r.stdout == "rc=0 out=\n", r.stderr
    assert "fetching origin once" not in r.stderr


@pytest.mark.parametrize("box", BOXES)
def test_a_full_deploy_clears_the_marker_before_the_copy_1302(box):
    """A copy that fails half way must not leave a new frontend under the old marker."""
    p = _program(box, "4:1")
    clear = p.index("# (3c) issue 1302")
    assert p.index("# (3) Back up") < clear < p.index("# (4) FULL bundle")
    seg = p[clear:p.index("# (4) FULL bundle")]
    assert "$abiStale = Join-Path $obsDir 'GENLOCK_STATS_ABI.txt'" in seg
    assert "if (Test-Path -LiteralPath $abiStale) { Remove-Item -LiteralPath $abiStale -Force -ErrorAction Stop }" in seg
    assert p.index("# (4) FULL bundle") < p.index(WRITE_ABI_41)


@pytest.mark.parametrize("state,rc", [("file", 0), ("missing", 0), ("blocked", 1)])
def test_the_clear_before_the_copy_is_fail_closed_1302(tmp_path, state, rc):
    """A marker the (3c) step cannot remove stops the program before the copy (never a silent
    no-op that would leave the old version naming a half-copied frontend)."""
    box = tmp_path / "obs"
    box.mkdir()
    marker = box / "GENLOCK_STATS_ABI.txt"
    if state in ("file", "blocked"):
        marker.write_text("3\r\n")
    block = _lib("genlock_stats_abi_clear_ps").stdout
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


@pytest.mark.parametrize("boxes,want", [("stream-snv", "0"), ("resolume-snv", "1"), ("imag,resolume-snv", "1"),
                                        ("strih-snv,stream-snv", "0"), ("strih-snv", "0"), ("imag", "0"),
                                        ("strih-snv,imag", "0"), ("", "0"), ("strih", "0")])
def test_the_per_box_table_names_the_windows_boxes_1302(boxes, want):
    r = _bash('. "$1"; fleet_boxes_swap_obs_dll "$2"', SCRIPTS / "lib" / "genlock-fleet-boxes.sh", boxes)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want + "\n"


def test_a_failed_fetch_names_why_1302(stale_clone):
    clone, _ = stale_clone
    absent = "b" * 40  # a well-formed commit id the origin does not have
    r = _lib('rc=0; out="$(genlock_stats_abi_resolve "$2" "$3" 1)" || rc=$?; echo "rc=$rc out=$out"', absent, clone)
    assert r.stdout == "rc=0 out=\n", r.stderr
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


@pytest.mark.parametrize("tag,out", [("out1", "4:1"), ("out2", "4:2"), ("none", "")])
def test_the_planner_reads_the_output_stats_version_too_1302(output_repo, tag, out):
    """The planner carries OBS_GENLOCK_OUTPUT_STATS_VERSION in the pair and never refuses a version by
    its value (the old plan-time stopgap, retired). An UNREADABLE define never refuses either (issue
    1522: no deploy compares the pair any more): the marker is removed, naming the define."""
    repo, shas = output_repo
    r = _lib('rc=0; out="$(genlock_stats_abi_resolve "$2" "$3")" || rc=$?; echo "rc=$rc out=$out"',
             shas[tag], repo)
    assert r.returncode == 0, r.stderr
    assert r.stdout == f"rc=0 out={out}\n", r.stderr
    if tag == "none":
        assert "cannot read OBS_GENLOCK_OUTPUT_STATS_VERSION" in r.stderr
        assert "cannot read OBS_GENLOCK_STATS_VERSION" not in r.stderr
    assert "ERROR" not in r.stderr and "deploy --full" not in r.stderr


def test_the_plan_time_output_stats_stopgap_is_retired_1302():
    s = LIB.read_text()
    assert "GENLOCK_STATS_ABI_OUTPUT_COVERED" not in s
    assert "does not compare that struct yet" not in s


def _write_markers_text(path):
    """setup-imag.sh's inline genlock_write_markers, or the whole file for the libs."""
    s = path.read_text()
    if path == SETUP_IMAG:
        start = s.index("genlock_write_markers() {")
        s = s[start:s.index("\n}\n", start)]
    return s


@pytest.mark.parametrize("path", [LIB, MARKERS, SETUP_IMAG], ids=["stats-abi", "markers", "setup-imag-inline"])
def test_every_bash_version_pattern_names_ascii_digits_explicitly_1302(path):
    """Review round 1: a bash bracket RANGE ([0-9], [1-9]) follows the locale's collation, and under
    en_US.UTF-8 it also matches non-ASCII digits. Every bash `=~`
    pattern in the stats-ABI code spells its sets out instead -- no `X-Y` range inside any bracket
    (review round 2: the narrower first lint missed the `[A-Z_]` define-name check)."""
    for n, line in enumerate(_write_markers_text(path).splitlines(), 1):
        if "=~" in line:
            assert not re.search(r"\[[^\]]*\w-\w[^\]]*\]", line), (path, n, line)


def test_the_marker_read_back_never_aborts_its_caller_1302(tmp_path):
    """Review round 2: called as a bare statement under the caller's errexit, the read-back of an
    unreadable staged marker (here a directory) prints nothing and returns 0."""
    d = tmp_path / "GENLOCK_STATS_ABI.txt"
    d.mkdir()
    r = _lib('genlock_stats_abi_pair_from_marker < "$2" > "$3"\necho "survived"', d, tmp_path / "out")
    assert r.returncode == 0, r.stderr
    assert r.stdout == "survived\n"
    assert (tmp_path / "out").read_text() == ""


# --- the marker's premise: every stats struct change bumps the version the marker records ----------

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
# comments stripped, whitespace collapsed). The marker records versions, so a field change without a
# version bump would leave a box's marker naming the wrong layout: a change here must bump the version
# AND update the pin in the same commit.
STATS_STRUCT_PINS = {"4": "9d1269d58919eb2d"}
OUTPUT_STATS_STRUCT_PINS = {"1": "ba0f22422814c0f1"}


def test_a_stats_struct_change_bumps_its_version_1302():
    import hashlib
    version = _define("OBS_GENLOCK_STATS_VERSION")
    digest = hashlib.sha256(_struct_body("obs_genlock_stats").encode()).hexdigest()[:16]
    assert STATS_STRUCT_PINS.get(version) == digest, (
        f"struct obs_genlock_stats changed (sha256 {digest}) at OBS_GENLOCK_STATS_VERSION {version}: a "
        "struct change must bump OBS_GENLOCK_STATS_VERSION (GENLOCK_STATS_ABI.txt records versions, issue "
        "1302), then pin the new body here")


def test_an_output_stats_struct_change_bumps_its_version_1302():
    """The frontend keeps struct obs_genlock_output_stats on its stack too, filled by obs.dll with no
    size. The marker records OBS_GENLOCK_OUTPUT_STATS_VERSION (ROZHODNUTÉ 6030159870 item 1), so a
    field change without a bump would leave it naming the wrong layout, exactly like the source stats
    struct above."""
    import hashlib
    version = _define("OBS_GENLOCK_OUTPUT_STATS_VERSION")
    digest = hashlib.sha256(_struct_body("obs_genlock_output_stats").encode()).hexdigest()[:16]
    assert OUTPUT_STATS_STRUCT_PINS.get(version) == digest, (
        f"struct obs_genlock_output_stats changed (sha256 {digest}) at OBS_GENLOCK_OUTPUT_STATS_VERSION "
        f"{version}: a struct change must bump OBS_GENLOCK_OUTPUT_STATS_VERSION (GENLOCK_STATS_ABI.txt "
        "records versions, issue 1302), then pin the new body here")


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
    # removed with a note: a marker never names one struct alone
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
    # the shapes the retired (0f) gate read (issue 1522 kept them on the reader): whitespace inside a
    # line and blank lines are ignored, a lone CR is no line break, every malformed line is unknown
    (b" 4 \r\n output_stats = 1 \r\n\r\n", "4:1"), (b"4\noutput_stats=1\n\x0b\x0c\n", "4:1"),
    (b"4 4\r\noutput_stats=1\r\n", "44:1"), (b"4\routput_stats=1", ""), (b"04\noutput_stats=1", ""),
    (b"4\noutput_stats=01", ""), (b"4\nOUTPUT_STATS=1", ""), (b"4\n1", ""), (b"4\noutput_stats=", ""),
    (b"output_stats=1\n4", ""), (b"4\noutput_stats=1\noutput_stats=1", ""), (b"abc\r\noutput_stats=1", ""),
    (b"\xef\xbb\xbf4\r\noutput_stats=1\r\n", ""), ("4\noutput_stats=\uff11\n".encode(), ""),
    # review round 1: a NUL byte or a non-ASCII digit is never a version
    (b"4\x00\noutput_stats=1\n", ""), (b"4\noutput_stats=1\x00\n", ""),
    ("٤\noutput_stats=1\n".encode(), ""), ("4\noutput_stats=¹\n".encode(), ""),
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
