#!/usr/bin/env bash
# Tier-0 pure-bash RED->GREEN test for scripts/lib/bisect-smoothness.sh (#1150).
set -euo pipefail
# Full header below: this sources the REAL lib and exercises its pure functions
# (parse / deploy-plan / marker / status / next-pending) over fixtures — NO rig,
# NO cargo. Runs directly: `bash tests/bisect-smoothness.test.sh`. The load-bearing
# safety assertion is that bisect_deploy_plan NEVER emits cam3 (the control box).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB="$HERE/../scripts/lib/bisect-smoothness.sh"
# shellcheck source=/dev/null
. "$LIB"

fails=0
pass=0
check() { # check "desc" "expected" "actual"
  if [ "$2" = "$3" ]; then pass=$((pass+1));
  else fails=$((fails+1)); printf 'FAIL: %s\n  expected: %q\n  actual:   %q\n' "$1" "$2" "$3" >&2; fi
}
check_rc() { # check_rc "desc" expected_rc actual_rc
  if [ "$2" = "$3" ]; then pass=$((pass+1));
  else fails=$((fails+1)); printf 'FAIL: %s\n  expected rc=%s actual rc=%s\n' "$1" "$2" "$3" >&2; fi
}

# --- bisect_parse_point_line: valid line -> tab-joined fields ---
out="$(bisect_parse_point_line "$(printf 'P2-post889\t31036919641\t1.7.0-dev.432\t#889 in')")"
check "parse valid line" "$(printf 'P2-post889\t31036919641\t1.7.0-dev.432\t#889 in')" "$out"

# --- comment + blank lines are skipped (rc!=0, no output) ---
rc=0; out="$(bisect_parse_point_line '# a comment' 2>/dev/null)" || rc=$?
check_rc "comment line skipped (rc)" 1 "$rc"
check "comment line no output" "" "$out"
rc=0; out="$(bisect_parse_point_line '   ' 2>/dev/null)" || rc=$?
check_rc "blank line skipped (rc)" 1 "$rc"

# --- non-numeric run-id is rejected (rc=2) ---
rc=0; bisect_parse_point_line "$(printf 'Pbad\tNOTNUM\t1.7.0-dev.1\tx')" >/dev/null 2>&1 || rc=$?
check_rc "non-numeric run-id rejected" 2 "$rc"

# --- bisect_deploy_plan: ALWAYS cam1 cam2, NEVER cam3 ---
plan="$(bisect_deploy_plan P3-bad462 31897259559 1.7.0-dev.462)"
check "deploy plan exact" 'CAMERA_SET="cam1 cam2" scripts/deploy-fleet.sh --run 31897259559' "$plan"
case "$plan" in *cam3*) check "deploy plan must not mention cam3" "no-cam3" "HAS-cam3";; *) check "deploy plan must not mention cam3" "no-cam3" "no-cam3";; esac

# --- bisect_marker_line: deterministic with injected timestamp ---
BISECT_NOW="2026-08-20T12:00:00Z"
ml="$(BISECT_NOW="$BISECT_NOW" bisect_marker_line P5-post1111 32208214341 1.7.0-dev.481 deployed 'cam1,cam2')"
check "marker line format" "$(printf '2026-08-20T12:00:00Z\tP5-post1111\t32208214341\t1.7.0-dev.481\tdeployed\tcam1,cam2')" "$ml"

# --- bisect_latest_status: last status for a label wins ---
markers="$(printf '%s\n%s\n%s\n' \
  '2026-08-20T10:00:00Z\tP1\t1\tv\tdeployed\tcam1,cam2' \
  '2026-08-20T11:00:00Z\tP1\t1\tv\tresult\tuniformity=0.998' \
  '2026-08-20T10:30:00Z\tP2\t2\tv\tdeployed\tcam1,cam2')"
markers="$(printf '%b' "$markers")"
check "latest status P1 = result" "result" "$(bisect_latest_status P1 "$markers")"
check "latest status P2 = deployed" "deployed" "$(bisect_latest_status P2 "$markers")"
check "latest status P9 (none) empty" "" "$(bisect_latest_status P9 "$markers")"

# --- bisect_next_pending: first label whose latest status != result ---
points="$(printf 'P1\t1\tv\tn\nP2\t2\tv\tn\nP3\t3\tv\tn\n')"
check "next pending = P2 (P1 has result)" "P2" "$(bisect_next_pending "$points" "$markers")"
# all done -> rc!=0
allmark="$(printf '%b' '2026\tP1\t1\tv\tresult\tx\n2026\tP2\t2\tv\tresult\tx\n2026\tP3\t3\tv\tresult\tx')"
rc=0; out="$(bisect_next_pending "$points" "$allmark" 2>/dev/null)" || rc=$?
check_rc "next pending all-done rc" 1 "$rc"

# --- bisect_camera_set: the single source of truth = cam1 cam2, never cam3 (issue 1150 review 🔴2) ---
cs="$(bisect_camera_set)"
check "camera set = cam1 cam2" "cam1 cam2" "$cs"
case "$cs" in *cam3*) check "camera set never cam3" "no-cam3" "HAS-cam3";; *) check "camera set never cam3" "no-cam3" "no-cam3";; esac
# and the deploy plan is built FROM it
check "deploy plan uses the camera set" "CAMERA_SET=\"$cs\" scripts/deploy-fleet.sh --run 9" "$(bisect_deploy_plan L 9 v)"

# --- REAL marker-append round-trip: deployed then result must land on SEPARATE lines and flip
#     latest_status to result / advance next_pending (issue 1150 review 🔴1). This writes through the
#     actual `bisect_marker_line ... >> file` path the driver uses, not a synthetic \n-joined string. ---
rtlog="$(mktemp)"
BISECT_NOW="2026-08-20T10:00:00Z" bisect_marker_line PX 7 v1 deployed cam1,cam2 >> "$rtlog"
BISECT_NOW="2026-08-20T11:00:00Z" bisect_marker_line PX 7 v1 result "uniformity=0.99" >> "$rtlog"
check "two appended markers = two physical lines" "2" "$(wc -l < "$rtlog")"
check "latest_status after append flips to result" "result" "$(bisect_latest_status PX "$(cat "$rtlog")")"
rtpoints="$(printf 'PX\t7\tv1\tn\nPY\t8\tv1\tn\n')"
check "next_pending advances past a resulted point" "PY" "$(bisect_next_pending "$rtpoints" "$(cat "$rtlog")")"
rm -f "$rtlog"

printf '\n== bisect-smoothness pure tests: %d passed, %d failed ==\n' "$pass" "$fails"
[ "$fails" -eq 0 ]
