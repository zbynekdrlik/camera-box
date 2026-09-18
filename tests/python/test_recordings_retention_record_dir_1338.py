r"""issue 1338 -- STATIC parity pins for the strih record-dir default (C:\_REC).

Owner ROZHODNUTE 18.9.2026: strih OBS records PERMANENTLY to ``C:\_REC`` (the D: NVMe dropped off
the bus 17.9., the owner chose to leave recordings on the C: system disk). The retention tooling's
static default must follow: both wrappers default to ``C:\_REC`` -- the dev1 planner
``scripts/strih-recordings-retention.sh`` (``RECORD_DIR``) and its on-box executor mirror
``scripts/strih-recordings-retention.ps1`` (``$RecordDir``). A stale ``D:\_REC`` default makes a
headless dry-run scan a device the box no longer has and report 0 candidates while the C: disk grows
unbounded -- the false-green this pins against.

There is no pwsh on dev1 CI, so the .ps1 is validated STRUCTURALLY (same style as
tests/python/test_recordings_retention_mirror_1276.py): text reads of the two script files.
"""

import pathlib
import re

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SH = _ROOT / "scripts" / "strih-recordings-retention.sh"
_PS1 = _ROOT / "scripts" / "strih-recordings-retention.ps1"


def _sh():
    return _SH.read_text(encoding="utf-8")


def _ps1():
    return _PS1.read_text(encoding="utf-8")


def _sh_record_dir_drive():
    # RECORD_DIR="C:\\_REC"  (bash double-quote: \\ is one literal backslash)
    m = re.search(r'^RECORD_DIR="([A-Za-z]):\\\\_REC"', _sh(), re.MULTILINE)
    assert m, "RECORD_DIR default assignment not found in strih-recordings-retention.sh"
    return m.group(1)


def _ps1_record_dir_drive():
    # [string]$RecordDir = "C:\_REC"
    m = re.search(r'\$RecordDir\s*=\s*"([A-Za-z]):\\_REC"', _ps1())
    assert m, "$RecordDir param default not found in strih-recordings-retention.ps1"
    return m.group(1)


def test_bash_default_record_dir_is_c_rec():
    assert _sh_record_dir_drive() == "C", (
        "the bash RECORD_DIR default must be C:\\_REC since 17.9.2026 (issue 1338)"
    )


def test_ps1_default_record_dir_is_c_rec():
    assert _ps1_record_dir_drive() == "C", (
        "the ps1 $RecordDir default must be C:\\_REC since 17.9.2026 (issue 1338)"
    )


def test_both_wrapper_defaults_agree_on_c_rec():
    # The dev1 planner default and the on-box executor default must be the SAME drive so an
    # unflagged sweep hits the real record dir on both legs.
    assert _sh_record_dir_drive() == _ps1_record_dir_drive() == "C"


def test_neither_wrapper_default_is_the_stale_d_rec():
    # Belt-and-suspenders: a half-migration (one wrapper moved, the other not) is exactly the
    # false-green this guards against.
    assert _sh_record_dir_drive() != "D"
    assert _ps1_record_dir_drive() != "D"


def test_ps1_missing_dir_guard_precedes_enumeration():
    # Pristup 1 / script-failure-policy: a missing record dir must FAIL LOUD (Test-Path false ->
    # named error -> non-zero exit) BEFORE the first Get-ChildItem enumeration, never an empty
    # "0 candidates" dry-run against a device the box no longer has.
    ps1 = _ps1()
    g = ps1.find("Test-Path -LiteralPath $RecordDir")
    e = ps1.find("Get-ChildItem -LiteralPath $RecordDir")
    assert g >= 0, "the Test-Path -LiteralPath $RecordDir guard is missing from the .ps1"
    assert e >= 0, "the Get-ChildItem -LiteralPath $RecordDir enumeration is missing from the .ps1"
    assert g < e, "the missing-dir guard must appear BEFORE the Get-ChildItem enumeration"
    # A non-zero exit sits between the guard test and the enumeration.
    assert re.search(
        r"Test-Path -LiteralPath \$RecordDir.*?exit\s+[1-9]", ps1[g:e], re.DOTALL
    ), "the guard must exit non-zero (never fall through to an empty sweep)"
