"""Issue 1511 (12.11.2026): a YouTube VOD id may start with '-' (run 43193587098: `-Q1wo4ezN0o`). The E2E passed
it as `--vod <id>`, argparse read the id as an option ("expected one argument") and the end point went UNKNOWN.
The real argument list from the E2E driver must reach the real verdict tool's measure step with the id intact."""

import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts"))
import youtube_leg_e2e as yle  # noqa: E402
import youtube_leg_verdict as ylv  # noqa: E402


class _Host:
    def path(self, *parts):
        return "/stage/" + "/".join(parts)


def test_a_vod_id_starting_with_a_dash_reaches_the_verdict_tool_1511(tmp_path, monkeypatch):
    st = {"vod_id": "-Q1wo4ezN0o", "record_start_utc": 1794470000.0}
    args = yle._verdict_args(None, st, [("w1", 1794470010.0, 1794470020.0)], _Host(), "/stage/rec.mp4")
    args = [str(tmp_path) if a == "/stage/verdict" else a for a in args]
    seen = {}

    def fake_measure(a):
        seen["vod"] = a.vod
        return {"schema": ylv.SCHEMA, "overall": "UNKNOWN", "criteria": {}, "windows": [], "publishes": [],
                "reasons": ["stub"]}

    monkeypatch.setattr(ylv, "measure", fake_measure)
    rc = ylv.main(args)
    assert seen.get("vod") == "-Q1wo4ezN0o", (rc, args)
