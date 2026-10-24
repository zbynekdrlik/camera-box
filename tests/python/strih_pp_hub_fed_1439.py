"""Issue 1439 on strih-pp (24.10.2026, design 6385519443): no deployed strih is hub-fed any more.

The hub-fed shape (all four program facts `none`, which the loader still takes on the strih role) is kept
under test on a synthetic copy of the box facts: strih-pp.env with its four program facts set back to none.
ONE list of those edits, shared by every test that needs the shape (not a test module: pytest collects only
`test_*` files).
"""
from pathlib import Path

BOXES = Path(__file__).resolve().parent.parent.parent / "scripts" / "strih-boxes"

HUB_FED_PAIRS = [("STRIH_PROGRAM_VBAN_STREAMS=strih-obs\n", "STRIH_PROGRAM_VBAN_STREAMS=none\n"),
                 ("STRIH_PROGRAM_VBAN_PORT=6981\n", "STRIH_PROGRAM_VBAN_PORT=none\n"),
                 ("STRIH_PROGRAM_VBAN_RESERVE_MS=40\n", "STRIH_PROGRAM_VBAN_RESERVE_MS=none\n"),
                 ("STRIH_PROGRAM_AUDIO_INPUT=ASIO zvuk\n", "STRIH_PROGRAM_AUDIO_INPUT=none\n")]


def hub_fed_boxes(dest):
    """Copy every box fact file into the new directory DEST, strih-pp.env made hub-fed; returns DEST (for
    STRIH_BOXES_DIR). Each edit must match exactly once, so a fact change fails here by name."""
    dest = Path(dest)
    dest.mkdir()
    for f in BOXES.glob("*.env"):
        t = f.read_text()
        if f.stem == "strih-pp":
            for old, new in HUB_FED_PAIRS:
                assert t.count(old) == 1, old
                t = t.replace(old, new)
        (dest / f.name).write_text(t)
    return dest
