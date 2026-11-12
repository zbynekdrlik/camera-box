"""Issue 1509 (12.11.2026): every recording run of the full-path E2E must claim resolume-snv in the rig lease.

The run's program-audio preflight and its mid-run watch (issue 1421) fail the run on SongPlayer's
SP-program audio in the stream mix, so the run depends on resolume-snv staying silent. Before the fix
the lease claimed resolume-snv only with the CG_CHAIN profile, a songplayer deploy read the
resolume-snv view as free at 06:08:14Z and played into run 43165398426.
"""

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "full-path-e2e.yml"


def _gate_resources_expr():
    text = WORKFLOW.read_text()
    start = text.index("- name: Rig-busy gate")
    end = text.index("run: bash scripts/rig-busy-gate.sh", start)
    step = text[start:end]
    m = re.search(r"^\s*RIG_LEASE_RESOURCES:\s*(.+)$", step, re.M)
    assert m, "the rig-busy gate step declares no RIG_LEASE_RESOURCES"
    return m.group(1).strip()


def _evaluate(expr, event, cg_chain):
    """Evaluate the GitHub expression `cond && 'a' || 'b'` for one event + CG_CHAIN value."""
    m = re.fullmatch(r"\$\{\{\s*(.+?)\s*&&\s*'([^']+)'\s*\|\|\s*'([^']+)'\s*\}\}", expr)
    assert m, f"unexpected expression shape: {expr}"
    cond, yes, no = m.groups()
    cond_py = (cond.replace("github.event_name", repr(event))
               .replace("vars.CG_CHAIN", repr(cg_chain))
               .replace("&&", " and ").replace("||", " or "))
    return yes if eval(cond_py, {"__builtins__": {}}) else no  # noqa: S307 - a fixed two-variable expression


def test_every_pull_request_run_claims_resolume_snv_1509():
    expr = _gate_resources_expr()
    for cg in ("", "0", "1"):
        names = _evaluate(expr, "pull_request", cg).split(",")
        assert "resolume-snv" in names and "snv-video" in names, (cg, names)


def test_a_dispatch_run_without_a_recording_keeps_the_video_set_1509():
    assert _evaluate(_gate_resources_expr(), "workflow_dispatch", "") == "snv-video"
