"""issue 1441 (stream-pp, 18.11.2026 17:16): a crashing OBS must never freeze its box.

OBS aborted (SIGABRT after `amdgpu: Not enough memory for command submission`) and systemd-coredump,
installed on every OBS box by the shared baseline, then read, wrote and compressed the multi-GB core
(the compiled defaults ProcessSizeMax=32G / ExternalSizeMax=32G): the notebook froze for minutes until
the owner power-cycled it.

obs_box_coredump_bound (scripts/lib/obs-box-coredump.sh, sourced by scripts/lib/obs-box-baseline.sh)
writes a coredump.conf drop-in that caps both sizes; the shared grader row `coredump`
(scripts/lib/obs-box-baseline-verify.sh) FAILs a box without it, or one whose EFFECTIVE config (the
`systemd-analyze cat-config` merge) is not the bound. Path seams and a fake systemd-analyze on PATH
keep every test off the real /etc.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BASE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"
LIB = REPO / "scripts" / "lib" / "obs-box-coredump.sh"
REAL_DROPIN = "/etc/systemd/coredump.conf.d/90-obs-box-coredump.conf"

# The stock 26.04 / 24.04 main file, as `systemd-analyze cat-config` prints it (dev1, systemd 255).
STOCK = """# /etc/systemd/coredump.conf
#  This file is part of systemd.
[Coredump]
#Storage=external
#Compress=yes
#ProcessSizeMax=32G
#ExternalSizeMax=32G
#JournalSizeMax=767M
#MaxUse=
#KeepFree=
"""


def _bash(body, *args, env=None):
    """Run BODY after sourcing the baseline under the callers' strict mode; values go in as $1..."""
    return subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$0"\n' + body, str(BASE), *args],
                          capture_output=True, text=True, timeout=60, env=env)


def _render():
    r = _bash("obs_box_coredump_dropin_text")
    assert r.returncode == 0, r.stderr
    return r.stdout


def _effective(text):
    r = _bash('obs_box_coredump_effective <<<"$1"', text)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def _me():
    return "%s:%s" % (subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip(),
                      subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip())


def test_the_dropin_caps_both_sizes_and_keeps_storage_external():
    text = _render()
    assert text.splitlines()[0].startswith("#")
    assert "[Coredump]\n" in text
    assert "ProcessSizeMax=256M\n" in text and "ExternalSizeMax=256M\n" in text
    # Storage stays the default (external): small cores keep their stack trace in coredumpctl.
    assert not any(ln.startswith("Storage=") for ln in text.splitlines())
    assert _bash("obs_box_coredump_dropin_path").stdout == REAL_DROPIN


def test_the_installer_writes_the_rendered_dropin_once_under_strict_mode(tmp_path):
    dest = tmp_path / "coredump.conf.d" / "90-obs-box-coredump.conf"
    r = _bash('obs_box_coredump_bound "$1" "$2"', str(dest), _me())
    assert r.returncode == 0, r.stdout + r.stderr
    assert dest.read_text() == _render()
    assert oct(dest.stat().st_mode & 0o777) == "0o644"
    assert "written" in r.stdout
    r = _bash('obs_box_coredump_bound "$1" "$2"', str(dest), _me())
    assert r.returncode == 0, r.stderr
    assert "unchanged" in r.stdout and "written" not in r.stdout


def test_the_installer_fails_loud_when_it_cannot_write(tmp_path):
    ro = tmp_path / "ro"
    ro.mkdir()
    ro.chmod(0o555)
    try:
        r = _bash('obs_box_coredump_bound "$1" "$2"', str(ro / "sub" / "x.conf"), _me())
    finally:
        ro.chmod(0o755)
    assert r.returncode != 0
    assert "1441" in r.stderr


def test_the_effective_values_are_the_last_ones_systemd_reads():
    assert _effective(STOCK) == "external,unset,unset"
    with_ours = STOCK + "\n# " + REAL_DROPIN + "\n" + _render()
    assert _effective(with_ours) == "external,256M,256M"
    # a later drop-in (sorted after ours) wins, and so does a Storage=journal anywhere
    later = with_ours + "\n# /etc/systemd/coredump.conf.d/99-x.conf\n[Coredump]\nProcessSizeMax = 2G\nStorage=journal\n"
    assert _effective(later) == "journal,2G,256M"
    # a key of another section is not the coredump config
    other = with_ours + "\n[Other]\nExternalSizeMax=9G\n"
    assert _effective(other) == "external,256M,256M"


def test_an_empty_or_unreadable_config_reads_empty_never_the_bound():
    assert _effective("") == ""


def _gather(tmp_path, dropin_text, analyze_out):
    stub = tmp_path / "bin"
    stub.mkdir()
    if analyze_out is not None:
        (tmp_path / "cat-config").write_text(analyze_out)
        (stub / "systemd-analyze").write_text(
            '#!/bin/bash\n[ "$1 $2" = "cat-config systemd/coredump.conf" ] || exit 9\ncat "%s"\n'
            % (tmp_path / "cat-config"))
    else:
        (stub / "systemd-analyze").write_text("#!/bin/bash\nexit 1\n")
    (stub / "systemd-analyze").chmod(0o755)
    dropin = tmp_path / "dropin.conf"
    if dropin_text is not None:
        dropin.write_text(dropin_text)
    env = dict(os.environ, PATH="%s:%s" % (stub, os.environ["PATH"]))
    body = ('s="$(obs_box_baseline_gather_snippet nosuchbox nosuchuser nosuch.service)"\n'
            's="${s//"$1"/"$2"}"\nbash -c "$s"')
    r = subprocess.run(["bash", "-c", '. "$0"\n' + body, str(VERIFY), REAL_DROPIN, str(dropin)],
                       capture_output=True, text=True, timeout=120, env=env)
    assert r.returncode == 0, r.stderr
    assert r.stdout.endswith("gather_done=1\n")
    facts = dict(ln.split("=", 1) for ln in r.stdout.splitlines() if "=" in ln)
    return facts, r.stdout


def test_the_real_gather_reads_the_dropin_and_the_effective_config(tmp_path):
    ours = _render()
    facts, _ = _gather(tmp_path, ours, STOCK + "\n# x\n" + ours)
    assert facts["coredump_dropin"] == "1"
    assert facts["coredump_effective"] == "external,256M,256M"


def test_the_gather_reports_a_missing_or_foreign_dropin(tmp_path):
    facts, _ = _gather(tmp_path, None, STOCK)
    assert facts["coredump_dropin"] == "0"
    assert facts["coredump_effective"] == "external,unset,unset"


def test_the_gather_reads_empty_when_cat_config_fails(tmp_path):
    facts, _ = _gather(tmp_path, _render(), None)
    assert facts["coredump_effective"] == ""


def _row(facts):
    r = subprocess.run(["bash", "-c", '. "$0"\nobs_box_baseline_verdict <<<"$1"', str(VERIFY), facts],
                       capture_output=True, text=True, timeout=60)
    rows = [ln.split("|") for ln in r.stdout.splitlines() if ln.startswith("coredump|")]
    assert len(rows) == 1, r.stdout
    return rows[0]


def test_the_row_is_ok_only_for_the_bound():
    good = "gather_done=1\ncoredump_dropin=1\ncoredump_effective=external,256M,256M\n"
    assert _row(good)[1] == "OK"
    assert _row(good.replace("coredump_effective=external", "coredump_effective=none"))[1] == "OK"
    for bad in ("coredump_dropin=0", "coredump_dropin="):
        assert _row(good.replace("coredump_dropin=1", bad))[1] == "FAIL", bad
    for bad in ("external,unset,unset", "external,32G,256M", "external,256M,2G", "journal,256M,256M", ""):
        assert _row(good.replace("external,256M,256M", bad))[1] == "FAIL", bad


def test_both_setups_provision_it_on_the_existing_line():
    for name in ("setup-strih.sh", "setup-imag.sh"):
        text = (REPO / "scripts" / name).read_text()
        line = [ln for ln in text.splitlines() if ln.startswith("obs_box_rtprio_off ")]
        assert len(line) == 1, name
        assert "obs_box_coredump_bound || fail" in line[0], name
        assert text.count("obs_box_coredump_bound") == 1, name
    assert len((REPO / "scripts" / "setup-strih.sh").read_text().splitlines()) <= 1000


def test_the_lib_is_sourced_by_the_baseline_and_needs_no_restart():
    base = BASE.read_text()
    assert base.count("/obs-box-coredump.sh") == 1
    lib = LIB.read_text()
    # systemd-coredump reads coredump.conf.d per crash: the item never restarts or reloads anything.
    assert "systemctl" not in lib.split("obs_box_coredump_bound()", 1)[1]
