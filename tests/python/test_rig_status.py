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
import os
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


# --------------------------------------------- #787 review 🔴: no false-green on a broken audit
def test_overall_state_empty_records_is_error_not_pass():
    # a crashed/empty audit (0 node lines) must NEVER read as PASS -- it is UNKNOWN, and the whole
    # point of the page is that a green banner PROVES health. summarize([]) alone returns PASS,
    # which is the exact false-green this guards against.
    assert _mod.overall_state([], 0) == "ERROR"
    assert _mod.overall_state([], None) == "ERROR"
    assert _mod.overall_state([], 2) == "ERROR"


def test_overall_state_crash_exit_is_error():
    recs = _mod.parse_audit("[PASS] cam1    svc=active\n")           # non-empty, but...
    assert _mod.overall_state(recs, 124) == "ERROR"                  # ...the audit timed out/crashed
    assert _mod.overall_state(recs, 127) == "ERROR"


def test_overall_state_normal_exit_uses_verdicts():
    assert _mod.overall_state(_mod.parse_audit(_AUDIT), 2) == "FAIL"
    assert _mod.overall_state(_mod.parse_audit("[PASS] cam1    svc=active\n"), 0) == "PASS"
    assert _mod.overall_state(_mod.parse_audit("[WARN] cam1    x=1  <<warn:y>>\n"), 1) == "WARN"


def test_render_html_empty_audit_is_never_green():
    html = _mod.render_html([], "1.7.0-dev.473", "2026-08-17T21:00:00Z", exit_code=0)
    assert 'data-overall="ERROR"' in html
    assert 'data-overall="PASS"' not in html
    assert "VŠETKY NODY ZDRAVÉ" not in html


def test_render_json_empty_audit_overall_error():
    payload = json.loads(_mod.render_json([], "v", "t", exit_code=0))
    assert payload["overall"] == "ERROR"


# --------------------------------------------- #787 review 🔴/dedup: alert also on a down prober
def test_alert_condition_fires_on_prober_down():
    assert _mod.alert_condition([], 0) == "prober-down:exit0"
    assert _mod.alert_condition(_mod.parse_audit("[PASS] cam1    x=1\n"), 124) == "prober-down:exit124"


def test_alert_condition_fires_on_fail_nodes():
    assert _mod.alert_condition(_mod.parse_audit(_AUDIT), 2) == "fail:cam3,stream"


def test_alert_condition_empty_when_healthy():
    assert _mod.alert_condition(_mod.parse_audit("[PASS] cam1    x=1\n"), 0) == ""
    assert _mod.alert_condition(_mod.parse_audit("[WARN] cam1    x=1  <<warn:y>>\n"), 1) == ""


# --------------------------------------------- #787 review 🟡: --dry-run must not mutate state
def test_dry_run_never_mutates_throttle_state(tmp_path):
    recs = _mod.parse_audit(_AUDIT)                                  # cam3 + stream FAIL
    state = str(tmp_path)
    fired = _mod._maybe_alert(recs, 2, state, "/nonexistent-notify",
                              dry_run=True, log=lambda *_: None)
    assert fired is False
    # a dry run must leave NO throttle fingerprint behind, or it silently eats the next real alert.
    assert not os.path.exists(os.path.join(state, "alert.state"))


# --------------------------------------------- issue 1108: dantesync NTP step-rate feeder facet
# The step-rate facet the FEEDER (rig-health-audit.py) adds renders through the SAME generic parser
# with zero page code -- this pins that forward-compat contract at the renderer layer (no rig-status
# change was needed for it, exactly like the cadence #1089 / build-sha #789 facets).
_STEPRATE_AUDIT = """\
[PASS] cam1    svc=active fps=60.0/60.0 chroma=colour dante=+12us root=ro load=0.35 steprate=28/h
[FAIL] strih   obs64=1 render=30.0fps/6.2ms audio_buf=64ms arrivals[cam1=60] cadence[cam1=60] steprate=147/h  <<ntp-step-storm=147/h(>=120/h)>>
[PASS] stream  obs64=1 render=30.0fps/6.2ms audio_buf=64ms arrivals[PGM=30] steprate=n/a pgm_latency_ms=3

=== RIG AUDIT: 2 PASS / 0 WARN / 1 FAIL (PROBLEMS ABOVE) ===
"""


def test_steprate_facet_parses_as_a_generic_chip():
    recs = _mod.parse_audit(_STEPRATE_AUDIT)
    assert _facet(_rec(recs, "cam1"), "steprate") == "28/h"
    # `steprate=n/a` (a slash, no internal `=`/bracket) parses as ONE chip too.
    assert _facet(_rec(recs, "stream"), "steprate") == "n/a"


def test_steprate_storm_problem_is_captured_not_swallowed():
    strih = _rec(_mod.parse_audit(_STEPRATE_AUDIT), "strih")
    assert strih["problems"] == "ntp-step-storm=147/h(>=120/h)"
    # the graded facet chip is still present before the <<...>> block.
    assert _facet(strih, "steprate") == "147/h"


def test_steprate_facet_renders_on_the_page():
    html = _mod.render_html(_mod.parse_audit(_STEPRATE_AUDIT),
                            "1.7.0-dev.474", "2026-08-18T09:00:00Z")
    assert "steprate" in html
    assert "28/h" in html and "147/h" in html          # a healthy + a storm value both shown
    assert "ntp-step-storm" in html                    # the FAIL problem visible so the operator sees WHY


def test_steprate_storm_drives_overall_fail_and_pages():
    recs = _mod.parse_audit(_STEPRATE_AUDIT)
    assert _mod.overall_state(recs, 2) == "FAIL"
    assert _mod.alert_condition(recs, 2) == "fail:strih"   # only the storm node pages
