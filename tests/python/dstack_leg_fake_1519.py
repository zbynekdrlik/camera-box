"""issue 1519: dstack's two pinned Dante-leg filter files under a file:// mirror (not collected: no `test_`
prefix). A strih deploy stages them into its tree, so every test that runs the deploy end to end feeds this
mirror and a temp cache instead of the network."""
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
FIX = REPO / "tests" / "fixtures" / "dstack_leg_1519"


def fake_dstack_leg_env(tmp_path):
    base = Path(tmp_path) / "dstack-mirror"
    d = base / "v0.6.129" / "tools" / "linux"
    d.mkdir(parents=True, exist_ok=True)
    for f in ("dante-leg.nft", "dante-leg-notrack.nft"):
        (d / f).write_bytes((FIX / f).read_bytes())
    return {"STRIH_DANTE_LEG_URL": "file://%s" % base, "STRIH_DANTE_LEG_CACHE": str(Path(tmp_path) / "dstack-cache"),
            "STRIH_DANTE_LEG_PIN_FILE": str(REPO / "scripts" / "dstack-leg-filter.pin")}
