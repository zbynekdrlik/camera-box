"""#787 -- unit tests for the rig status-page renderer (scripts/rig-status.py).

The status page is a RENDERER over scripts/rig-health-audit.py's line output -- it never
ssh/WS-es a node itself (rig-health-audit.py is the ONE prober). These tests pin the PURE
pieces that carry all the logic, with NO ssh / WS / subprocess to the rig:

  * parse_audit()      -- turn the audit's `[VERDICT] node key=value... <<problems>>`
                          stdout into structured per-node records (verdict, node, ordered
                          facet chips, raw problems string, raw line). Bracket groups
                          (`arrivals[...]`, `cadence[...]`) stay whole; the summary/blank
                          lines are ignored; a NEVER-SEEN facet key renders as a chip too
                          (the forward-compat contract: cadence #1089 today, build-sha #789
                          whenever the FEEDER adds it -- no page change).
  * summarize()        -- PASS/WARN/FAIL counts + the overall verdict.
  * alert_signature()  -- the deduped Discord-on-FAIL fingerprint = the sorted FAIL node set.
  * history_entry()    -- one JSONL history row from (audit text, exit code, ts).
  * render_json()      -- machine-readable page payload.
  * render_html()      -- the page: version label in the DOM (version-on-dashboard), a row
                          per node, FAIL highlighted, an unseen facet still shown.
"""
import importlib.util
import json
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location("rig_status", SCRIPTS / "rig-status.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()


# A realistic multi-node audit sweep: every verdict tier, the two heterogeneous detail
# shapes (cam vs windows-box), bracket groups, a problems block that CONTAINS A SPACE
# (`warn:cadence cam2=...`), and a NEVER-SEEN facet key (`build_sha=abc1234`) on imag to
# prove the generic renderer surfaces future feeder facets with zero page code.
_AUDIT = """\
[PASS] cam1    svc=active fps=60.0/60.0 chroma=colour dante=+12us root=ro load=0.35
[WARN] cam2    svc=active fps=60.0/60.0 chroma=colour dante=+8us root=ro load=0.40  <<warn:capture-dropped=2>>
[FAIL] cam3    svc=inactive fps=? chroma=? dante=? root=ro load=0.30  <<svc=inactive no-streaming-report>>
[PASS] imag    render=60.0fps/8.1ms skip=0.02% arrivals[CAM1=60,CAM2=60,CAM3=60] isolcpus=none dante=-5us build_sha=abc1234
[WARN] strih   obs64=1 render=30.0fps/6.2ms audio_buf=64ms arrivals[cam1=60,cam2=50] cadence[cam1=60,cam2=50]  <<warn:cadence cam2=50fps(!=60)>>
[FAIL] stream  obs64=1 render=30.0fps/6.2ms audio_buf=120ms arrivals[PGM=30] pgm_latency_ms=3  <<AUDIO-BUF=120ms>>

=== RIG AUDIT: 2 PASS / 2 WARN / 2 FAIL (PROBLEMS ABOVE) ===
"""


def _rec(records, node):
    return next(r for r in records if r["node"] == node)


def _facet(rec, key):
    for f in rec["facets"]:
        if f.get("key") == key:
            return f["value"]
    return None


# ---------------------------------------------------------------- parse_audit
def test_parse_audit_finds_every_node_and_verdict():
    recs = _mod.parse_audit(_AUDIT)
    got = {(r["node"], r["verdict"]) for r in recs}
    assert got == {
        ("cam1", "PASS"), ("cam2", "WARN"), ("cam3", "FAIL"),
        ("imag", "PASS"), ("strih", "WARN"), ("stream", "FAIL"),
    }


def test_parse_audit_ignores_summary_and_blank_lines():
    recs = _mod.parse_audit(_AUDIT)
    assert len(recs) == 6
    assert all(r["node"] not in ("===", "RIG", "") for r in recs)


def test_parse_audit_facets_are_key_value_pairs():
    cam1 = _rec(_mod.parse_audit(_AUDIT), "cam1")
    assert _facet(cam1, "svc") == "active"
    assert _facet(cam1, "fps") == "60.0/60.0"
    assert _facet(cam1, "chroma") == "colour"
    assert _facet(cam1, "dante") == "+12us"
    assert _facet(cam1, "root") == "ro"


def test_parse_audit_keeps_bracket_groups_whole():
    imag = _rec(_mod.parse_audit(_AUDIT), "imag")
    # `arrivals[CAM1=60,CAM2=60,CAM3=60]` is ONE facet keyed `arrivals`, not split on its
    # internal `=`/`,`.
    arrivals = _facet(imag, "arrivals")
    assert arrivals is not None
    assert "CAM1=60" in arrivals and "CAM3=60" in arrivals
    assert not any(f.get("key", "").startswith("arrivals[") for f in imag["facets"])


def test_parse_audit_surfaces_an_unseen_facet_key():
    # build_sha is a facet the renderer has NEVER been coded for -- it must still parse as a
    # normal key=value chip (forward-compat with the #789 build-sha feeder enrichment).
    imag = _rec(_mod.parse_audit(_AUDIT), "imag")
    assert _facet(imag, "build_sha") == "abc1234"


def test_parse_audit_captures_problems_block_with_internal_spaces():
    strih = _rec(_mod.parse_audit(_AUDIT), "strih")
    # the whole `<<...>>` inner text is preserved verbatim, spaces and all.
    assert strih["problems"] == "warn:cadence cam2=50fps(!=60)"
    cam1 = _rec(_mod.parse_audit(_AUDIT), "cam1")
    assert cam1["problems"] is None


def test_parse_audit_problems_are_not_swallowed_into_facets():
    strih = _rec(_mod.parse_audit(_AUDIT), "strih")
    # nothing from the `<<...>>` block leaks into the facet chips.
    assert not any("cadence cam2" in str(f.get("value", "")) for f in strih["facets"])
    # the cadence[...] FACET (before <<) is still parsed.
    assert _facet(strih, "cadence") is not None


def test_parse_audit_preserves_raw_line():
    cam3 = _rec(_mod.parse_audit(_AUDIT), "cam3")
    assert cam3["raw"].startswith("[FAIL] cam3")


# ---------------------------------------------------------------- summarize
def test_summarize_counts_and_overall_fail():
    s = _mod.summarize(_mod.parse_audit(_AUDIT))
    assert s["pass"] == 2 and s["warn"] == 2 and s["fail"] == 2
    assert s["overall"] == "FAIL"


def test_summarize_overall_warn_when_no_fail():
    recs = _mod.parse_audit(
        "[PASS] cam1    svc=active\n[WARN] cam2    svc=active  <<warn:x>>\n")
    s = _mod.summarize(recs)
    assert s["fail"] == 0 and s["warn"] == 1
    assert s["overall"] == "WARN"


def test_summarize_overall_pass_when_all_clean():
    recs = _mod.parse_audit("[PASS] cam1    svc=active\n[PASS] imag    render=60fps\n")
    assert _mod.summarize(recs)["overall"] == "PASS"


# ---------------------------------------------------------------- alert_signature
def test_alert_signature_is_sorted_fail_node_set():
    sig = _mod.alert_signature(_mod.parse_audit(_AUDIT))
    # cam3 + stream failed -> a stable, order-independent fingerprint of exactly those.
    assert sig == "cam3,stream"


def test_alert_signature_empty_when_no_fail():
    recs = _mod.parse_audit("[PASS] cam1    svc=active\n[WARN] cam2    svc=active  <<warn:x>>\n")
    assert _mod.alert_signature(recs) == ""


# ---------------------------------------------------------------- history_entry
def test_history_entry_shape():
    e = _mod.history_entry(_AUDIT, 2, "2026-08-17T21:00:00Z")
    assert e["ts"] == "2026-08-17T21:00:00Z"
    assert e["exit"] == 2
    assert e["counts"] == {"pass": 2, "warn": 2, "fail": 2}
    # nodes carry enough to re-render the run compactly (verdict + node at least).
    nodes = {n["node"]: n["verdict"] for n in e["nodes"]}
    assert nodes["cam3"] == "FAIL" and nodes["cam1"] == "PASS"


# ---------------------------------------------------------------- render_json
def test_render_json_has_version_generated_counts_nodes():
    recs = _mod.parse_audit(_AUDIT)
    payload = json.loads(
        _mod.render_json(recs, "1.7.0-dev.473", "2026-08-17T21:00:00Z"))
    assert payload["version"] == "1.7.0-dev.473"
    assert payload["generated_at"] == "2026-08-17T21:00:00Z"
    assert payload["counts"] == {"pass": 2, "warn": 2, "fail": 2}
    assert payload["overall"] == "FAIL"
    assert len(payload["nodes"]) == 6


# ---------------------------------------------------------------- render_html
def test_render_html_carries_version_in_dom():
    html = _mod.render_html(_mod.parse_audit(_AUDIT), "1.7.0-dev.473", "2026-08-17T21:00:00Z")
    # version-on-dashboard: DOM-readable version (a data-attribute AND visible text).
    assert 'data-version="1.7.0-dev.473"' in html
    assert "1.7.0-dev.473" in html
    assert "2026-08-17T21:00:00Z" in html


def test_render_html_has_a_row_per_node_and_overall_banner():
    html = _mod.render_html(_mod.parse_audit(_AUDIT), "v", "t")
    for node in ("cam1", "cam2", "cam3", "imag", "strih", "stream"):
        assert node in html
    # overall FAIL banner present.
    assert "FAIL" in html


def test_render_html_highlights_fail_problems():
    html = _mod.render_html(_mod.parse_audit(_AUDIT), "v", "t")
    # a failing node's problems text is present so the operator can see WHY.
    assert "AUDIO-BUF=120ms" in html


def test_render_html_shows_unseen_facet_chip():
    # the forward-compat proof at the render layer: build_sha appears on the page though the
    # renderer has no build_sha-specific code.
    html = _mod.render_html(_mod.parse_audit(_AUDIT), "v", "t")
    assert "build_sha" in html and "abc1234" in html


def test_render_html_escapes_markup():
    # a facet value containing HTML metacharacters must be escaped, never injected raw.
    recs = _mod.parse_audit("[FAIL] cam1    note=<script>x</script>  <<bad<tag>>>\n")
    html = _mod.render_html(recs, "v", "t")
    assert "<script>x</script>" not in html
    assert "&lt;script&gt;" in html
