"""Issue 1380 slice-2 follow-up: the report-only [4c/8] pixel-hash line labels the gate's exit code.

frozen-camera-gate.py exits 0 = PASS, 1 = FROZEN, 2 = ERROR (a refused scene select or a guard that
failed to load, issue 1380), 124 = the `timeout` wrapper fired. The line must name each of these
honestly, so an ERROR never pollutes the pixel-vs-received evidence as a fake FROZEN.

The block is extracted from scripts/recording-e2e.sh (the harness has no source guard) and the gate
call is replaced by a stub that exits with the code under test.
"""

import pathlib
import re
import subprocess

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = REPO / "scripts" / "recording-e2e.sh"


def _label_block() -> str:
    text = SCRIPT.read_text()
    start = text.index("frozen_pixel_verdict=PASS\n")
    end = text.index('echo "    [frozen-camera-gate] #1233 pixel-hash REPORT-ONLY', start)
    block = text[start:end]
    call = re.search(r'timeout "\$\{FROZEN_CAM_PIXEL_REPORT_TIMEOUT_S.*?\|\| frozen_pixel_rc=\$\?', block, re.S)
    assert call, "the pixel-gate call shape moved; update this test's extraction"
    return block.replace(call.group(0), '(exit "$STUB_RC") || frozen_pixel_rc=$?')


@pytest.mark.parametrize(
    "rc,label",
    [(0, "PASS"), (1, "FROZEN"), (2, "ERROR"), (124, "TIMEOUT")],
)
def test_pixel_line_names_each_exit_code(rc, label):
    script = "set -euo pipefail\n" + _label_block() + 'printf "%s" "$frozen_pixel_verdict"\n'
    out = subprocess.run(
        ["bash", "-c", script],
        env={"STUB_RC": str(rc), "PATH": "/usr/bin:/bin"},
        capture_output=True,
        text=True,
        check=True,
    )
    assert out.stdout == label
