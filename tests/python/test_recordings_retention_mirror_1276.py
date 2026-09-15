"""#1276 -- STATIC parity pins for the recordings-retention SIZE floor.

The production-size PROTECT floor (owner ruling 15.9.2026, issue 1276) lives in TWO places that
must stay byte-identical: the canonical pure decision src/recordings_retention.rs
(``PRODUCTION_SIZE_FLOOR_BYTES``) and its PowerShell delete-gate mirror
scripts/strih-recordings-retention.ps1 (``$ProductionSizeFloorBytes``). There is no pwsh on dev1
CI, so this test validates the .ps1 STRUCTURALLY (same pattern as
tests/python/test_strih_nic_selfheal_1199.py): the two constants are numerically identical, the
mirror has the ``-ge`` PROTECT branch, tags the reason ``production-sized``, and prints the floor
in the header block so a reviewer sees it in every dry-run.
"""

import pathlib
import re

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_RS = _ROOT / "src" / "recordings_retention.rs"
_PS1 = _ROOT / "scripts" / "strih-recordings-retention.ps1"

_ONE_GIB = 1_073_741_824


def _rs():
    return _RS.read_text(encoding="utf-8")


def _ps1():
    return _PS1.read_text(encoding="utf-8")


def _rust_floor():
    # pub const PRODUCTION_SIZE_FLOOR_BYTES: u64 = 1_073_741_824;
    m = re.search(
        r"pub const PRODUCTION_SIZE_FLOOR_BYTES:\s*u64\s*=\s*([0-9_]+)\s*;", _rs()
    )
    assert m, "PRODUCTION_SIZE_FLOOR_BYTES const not found in recordings_retention.rs"
    return int(m.group(1).replace("_", ""))


def _ps1_floor():
    # [long]$ProductionSizeFloorBytes = 1073741824,
    m = re.search(
        r"\$ProductionSizeFloorBytes\s*=\s*([0-9]+)", _ps1()
    )
    assert m, "$ProductionSizeFloorBytes default not found in strih-recordings-retention.ps1"
    return int(m.group(1))


def test_both_floor_constants_are_identical():
    assert _rust_floor() == _ps1_floor()


def test_floor_is_one_gib():
    # Calibrated ~1 GiB: above the 0.8 GB E2E max, below the 5.6 GB smallest production file.
    assert _rust_floor() == _ONE_GIB
    assert _ps1_floor() == _ONE_GIB


def test_rust_has_production_sized_reason_and_guard():
    rs = _rs()
    assert "ProductionSized" in rs, "KeepReason::ProductionSized variant missing"
    # The PROTECT guard: at-or-above the floor.
    assert re.search(
        r"size_bytes\s*>=\s*PRODUCTION_SIZE_FLOOR_BYTES", rs
    ), "the >= floor PROTECT guard is missing from plan()"


def test_ps1_has_ge_protect_branch():
    ps1 = _ps1()
    # -ge mirrors the Rust `>=` (at-or-above protected); guards against a stray -gt drift.
    assert re.search(
        r"\.Length\s+-ge\s+\$ProductionSizeFloorBytes", ps1
    ), "the -ge $ProductionSizeFloorBytes PROTECT branch is missing from the .ps1"
    assert "-gt $ProductionSizeFloorBytes" not in ps1, "floor compare must be -ge, never -gt"


def test_ps1_tags_production_sized_reason():
    assert 'Reason = "production-sized"' in _ps1(), (
        'the .ps1 must tag the protected files Reason = "production-sized"'
    )


def test_ps1_prints_the_floor_in_the_header():
    ps1 = _ps1()
    # A header Write-Output line that names the floor -- so every dry-run shows WHY files are kept.
    header_line = re.search(
        r'Write-Output\s*\(\s*"SizeFloor[^\n]*\$ProductionSizeFloorBytes', ps1
    )
    assert header_line, "the .ps1 header must print SizeFloor with $ProductionSizeFloorBytes"
