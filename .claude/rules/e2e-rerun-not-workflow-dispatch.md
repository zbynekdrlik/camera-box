---
paths:
  - ".github/workflows/full-path-e2e.yml"
  - "scripts/recording-e2e.sh"
---

<!-- Moved verbatim from CLAUDE.md (issue 1552). -->

## GOTCHA — a live-triggered E2E gate run can race ahead of a mid-cycle fleet redeploy

If a PR's fix requires a fleet redeploy to actually take effect on the live rig BEFORE the gate
can pass (e.g. a WARN threshold recalibration — #685), the PR's own automatic `pull_request`-
triggered "Full-path E2E" run can start (and fail, against the STILL-stale rig) before the redeploy
finishes — pushing the fix and deploying it is NOT atomic with the CI trigger. Don't chase that
failed run; once the redeploy is verified live (journal/WS read-back), get a fresh REAL verdict.

**CORRECTED 2026-07-12/13 (#717, #719/#726 dispatch) — this section used to recommend
`gh workflow run "Full-path E2E ..." --ref dev` here. DO NOT DO THAT — it is DANGEROUSLY WRONG
for this purpose.** `full-path-e2e.yml` branches `E2E_EXECUTE_VERDICT`/`ALL_CAMBOX` on
`github.event_name == 'pull_request'` — a `workflow_dispatch` run (what `gh workflow run` always
creates) ALWAYS gets `E2E_EXECUTE_VERDICT=0`/`ALL_CAMBOX=0` and stays in the OLD plan-print-only
mode: it never decodes strih/stream, never computes a real verdict, and "succeeds" trivially —
**yet GitHub still posts a check-run with the SAME required-check NAME on the SAME commit SHA**,
which SATISFIES the PR's branch-protection requirement. Following this GOTCHA as originally
written could let a genuinely broken PR merge behind a MEANINGLESS green. The correct way to get
a fresh REAL verdict on an already-pushed commit (no new push, e.g. after a fleet-side fix or an
infra repair with no code diff) is:

```bash
gh run rerun <the-original-pull_request-run-id>
```

`gh run rerun` preserves the ORIGINAL trigger's event context (`github.event_name` stays
`pull_request`), so `E2E_EXECUTE_VERDICT`/`ALL_CAMBOX` are correctly `1` again. Find the run id via
`gh run list --branch dev --workflow "Full-path E2E (recording-based · hardware · self-hosted
dev1)" --json databaseId,event,headSha` and pick the one with `"event": "pull_request"` matching
your commit. Full detail + the "two same-commit `pull_request` runs can disagree for reasons
outside your own diff" corollary: `.claude/skills/e2e`'s own `gh workflow run` section (the
canonical source now — this CLAUDE.md section is kept only as a pointer + a loud warning against
the old advice; do not restore the `gh workflow run` snippet here). Same "manual re-trigger after a
stale/superseded run" IDEA the `linux-genlock.yml` GOTCHA above documents for a cancelled run — but
`gh workflow run` is the WRONG mechanism for this specific workflow; `gh run rerun` is correct.
