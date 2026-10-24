#!/usr/bin/env bash
# airuleset:script-ok source-only lib (never executed directly) -- must NOT set -e: sourcing runs it
# in the CALLER's shell (recording-e2e.sh), whose own `set -euo pipefail` would otherwise be changed
# here. Same convention as scripts/lib/e2e-win-baseline.sh. The function below is safe under the
# caller's `set -euo pipefail` and always returns 0.
#
# scripts/lib/e2e-linux-bytes.sh -- issue 1474 (owner 24.10.2026: the Linux strih/stream must be
# tested at least as hard as the Windows boxes were). The full-path E2E `[0/8]` hands the
# version-integrity gate, per Linux OBS box (strih-snv, stream-snv), the DEPLOYED bytes of its bundle
# files and the BUNDLE_MANIFEST.json of the linux-genlock artifact it was deployed from, so the gate's
# `linux_so_bytes` row grades them -- the Linux counterpart of the Windows obs.dll / distroav.dll
# byte rows those boxes SKIP. Design-by: main (issue 1474, comment 6382198015, Approach 1).
#
# Reuses, never re-implements (scripts/lib/manifest-autosource.sh, the issue-1082 imag path):
#   * linux_obs_box_so_gather_cmd -- the remote sha256 gather of the ONE file list;
#   * linux_obs_box_so_bytes_csv -- its parser;
#   * manifest_autosource_fetch + linux_obs_box_bundle_artifact -- the CI manifest at the box's OWN
#     marker sha (its `:8899` genlock_build_sha), cached per CI run.
#
# FAIL-CLOSED by construction: this helper only COLLECTS. Whatever it could not get (no answer, no
# sha256sum, no marker sha, no manifest) is simply not handed over, and the gate reads a missing
# gather or manifest for a --linux box as UNKNOWN and refuses the run. Each miss is ONE named line.
#
# State across calls: E2E_LINUX_BYTES_MEMO_SHA / _MEMO_PATH (the last build sha fetched and its
# result, so the second box of one build reuses it).
# Test seams (offline): manifest-autosource.sh's MANIFEST_AUTOSOURCE_CMD; E2E_LINUX_BYTES_REMOTE_PREFIX
# (the remote install prefix, default /usr); E2E_LINUX_BYTES_SSH_TIMEOUT (default 30 s);
# E2E_LINUX_BYTES_FETCH_TIMEOUT (the manifest fetch's gh bound, default 900 s, sized below).
# Tests: tests/python/test_linux_bytes_gate_1474.py.

command -v linux_obs_box_so_gather_cmd >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/manifest-autosource.sh"

# e2e_linux_bytes_gather ROLE HOST USER PW STATE_FILE OUTDIR REPO -> appends
# `--linux-manifest ROLE=<path>` and `--linux-bytes ROLE=<csv>` to the caller's LINUX_BYTES_GATE_ARGS
# array (each only when obtained) and prints one line per box. ROLE is the gate's box label
# (strih / stream), STATE_FILE the box's fetched bundle-state JSON (its genlock_build_sha keys the
# manifest). Plain ssh with UserKnownHostsFile=/dev/null (dev1's known_hosts held the Windows keys of
# 10.77.9.204, which with StrictHostKeyChecking=no disables password auth, issue 1462). Always rc 0.
e2e_linux_bytes_gather() {
  local role="$1" host="$2" user="$3" pw="$4" state="$5" outdir="$6" repo="$7"
  local tmo="${E2E_LINUX_BYTES_SSH_TIMEOUT:-30}" probe="" csv="" sha="" manifest=""
  probe="$(timeout "$tmo" sshpass -p "$pw" ssh -n -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=8 "${user}@${host}" \
    "$(linux_obs_box_so_gather_cmd "${E2E_LINUX_BYTES_REMOTE_PREFIX:-/usr}")" 2>/dev/null || true)"
  csv="$(linux_obs_box_so_bytes_csv "$probe" || true)"
  if [ -n "$csv" ]; then
    LINUX_BYTES_GATE_ARGS+=(--linux-bytes "${role}=${csv}")
    local -a files=()
    IFS=, read -r -a files <<<"$csv"
    echo "  ${role} (${host}): bundle bytes gathered (${#files[@]} files)"
  else
    echo "  ${role} (${host}): NO bundle bytes gathered (no answer over ssh, or no sha256sum) -- the gate's linux_so_bytes row reads UNKNOWN and refuses (issue 1474)"
  fi
  sha="$(genlock_build_sha_state_read "$state" || true)"
  if [ -z "$sha" ]; then
    echo "  ${role} (${host}): no genlock_build_sha in its bundle state -- no manifest to grade against, the gate refuses (issue 1474)"
    return 0
  fi
  # GitHub serves an artifact only as one zip, and the strih bundle is ~600 MB (run 40031379800:
  # 601430199 bytes); the Windows full bundle (270 MB) took 145 s, so this download needs ~320 s on
  # the first fetch of a build -- over the 300 s default gh bound. 900 s covers it with room, for the
  # DOWNLOAD only (the run lookup keeps 300 s). Every later fetch of the same CI run is the per-run
  # cache (3 s). Two boxes of ONE build (strih + stream share a marker sha) fetch once per E2E: the
  # result, a FAILURE included, is reused for the second box (review round 1: a repeated failed
  # ~600 MB download would double the worst-case [0/8] wall time under the rig lease).
  if [ "$sha" = "${E2E_LINUX_BYTES_MEMO_SHA:-}" ]; then
    manifest="${E2E_LINUX_BYTES_MEMO_PATH:-}"
  else
    manifest="$(MANIFEST_AUTOSOURCE_DOWNLOAD_TIMEOUT_S="${E2E_LINUX_BYTES_FETCH_TIMEOUT:-900}" \
      manifest_autosource_fetch "$repo" linux-genlock.yml "$(linux_obs_box_bundle_artifact)" \
      "$sha" "${outdir}/${role}-linux-manifest.json" || true)"
    E2E_LINUX_BYTES_MEMO_SHA="$sha"
    E2E_LINUX_BYTES_MEMO_PATH="$manifest"
  fi
  if [ -n "$manifest" ]; then
    LINUX_BYTES_GATE_ARGS+=(--linux-manifest "${role}=${manifest}")
    # the manifest's own build stamp: one sha can have two linux-genlock runs (a re-run, a tag
    # dispatch) whose bytes differ, so a DRIFT is read against this stamp vs the box's own
    # /opt/obs-genlock/BUNDLE_MANIFEST.json generated_at (.claude/rules/version-integrity-gate.md)
    local gen=""
    gen="$(sed -n 's/^[[:space:]]*"generated_at":[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" 2>/dev/null | sed -n 1p || true)"
    echo "  ${role} (${host}): linux bundle manifest of build ${sha} (generated ${gen:-unknown}) fetched"
  else
    echo "  ${role} (${host}): could not fetch the $(linux_obs_box_bundle_artifact) manifest of build ${sha} -- the gate refuses (issue 1474)"
  fi
  return 0
}
