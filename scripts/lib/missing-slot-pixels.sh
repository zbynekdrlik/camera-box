#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions; the only top-level statement is the guarded
# win-ssh-exec.sh source below) — the sibling scripts/lib/*.sh convention: sourcing runs in the
# CALLER's shell (recording-e2e.sh already sets `set -euo pipefail`), and every runtime function
# ALWAYS returns 0 on its best-effort paths so it can never trip the caller's `set -e`.
#
# scripts/lib/missing-slot-pixels.sh — issue 1367: pixel proof for every CLASSIFIED missing /
# unreadable slot the merge could not extract.
#
# WHY: the verdict classifies a slot (`full_chain.loss.<node>.classified[]`, e.g. BURN-UNREADABLE)
# in the MERGE on dev1, where the recordings are not present, so those entries keep `png: null`. The
# on-box `--extract-partial` flags only its own undecodable / missing-burn frames, and the #652
# cleanup plan then removes the recordings. Nobody could SEE a damaged frame (blocky NDI decode,
# tear, blend, blur — each points to a different cause).
#
# WHAT: after the merge, before the Discord report and the #652 cleanup plan, for the classified
# slots with `png == null` (capped, MISSING_SLOT_PIXELS_CAP, default 12 per run) this exports the
# slot frame plus its two neighbours as PNG ON the box that holds the recording (camN -> the strih
# recording, strih/stream -> the stream recording — the verdict's NodeSpec pairing), pulls them to
# `$OUTDIR/<node>-missing/`, logs every path, and records a `missing_slot_pixels` block in the
# verdict JSON the Discord report reads. Best-effort: it never changes the verdict or $GATE.
#
# THE INDEXING CONTRACT (the load-bearing part): the verdict's `frame_index` is the ordinal of the
# raw frame on the pipe of `ffmpeg -v error -nostdin -i <rec> -f rawvideo -pix_fmt gray pipe:1`
# (src/probe/recording.rs `read_frames`). That output is CFR: a timestamp gap in the recording is
# filled with a duplicate, so a naive `select=eq(n,k)` on the source is off after the first gap. So
# stage 1 here IS that decode, byte-for-byte, and stage 2 selects by raw-frame ordinal:
#   ffmpeg <verdict decode> | ffmpeg -f rawvideo -pixel_format gray -video_size WxH -framerate 1
#     -i pipe:0 -vf select=... -fps_mode passthrough -frame_pts 1 frame-%d.png
# With `-framerate 1` the pts of each raw frame IS its ordinal, so `-frame_pts 1` names every PNG
# after the verdict frame_index. The PNGs are GRAY — exactly the luma the decoder saw.
# tests/harness_missing_slot_pixels_1367.rs pins this against a VFR fixture with real ffmpeg.

if ! declare -F win_ssh_ps_encoded_command >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/win-ssh-exec.sh
  . "$(dirname "${BASH_SOURCE[0]}")/win-ssh-exec.sh"
fi

# ---- pure builders -------------------------------------------------------------------------------

# The per-run cap on exported SLOTS (each slot = up to 3 PNGs). Env MISSING_SLOT_PIXELS_CAP, default
# 12; a non-positive or non-integer value falls back to 12. Pure.
missing_slot_pixels_cap() {
  case "${MISSING_SLOT_PIXELS_CAP:-}" in
    '' | *[!0-9]* | 0) printf '12' ;;
    *) printf '%s' "$MISSING_SLOT_PIXELS_CAP" ;;
  esac
}

# Which box holds the recording a node's classified slots index into: camN -> strih (the camera
# burns are read from the strih recording), strih / stream -> stream (their burns are read from the
# stream recording). Anything else (imag, the cam2 optical node) prints nothing. Pure.
missing_slot_pixels_box_for_node() {
  case "$1" in
    strih | stream) printf 'stream' ;;
    cam[0-9] | cam[0-9][0-9]) printf 'strih' ;;
  esac
  return 0
}

# The classified slots with no pixel proof, as `node<TAB>frame_index` lines sorted by (node, index),
# deduplicated, only for nodes a box backs, the first $2 of them ($2 = 0: all). $1 = the merged
# verdict JSON. An unreadable JSON prints nothing. ALWAYS returns 0.
missing_slot_pixels_frames() {
  python3 -c '
import json, re, sys
path, cap = sys.argv[1], int(sys.argv[2])
try:
    with open(path) as f:
        loss = json.load(f)["full_chain"]["loss"]
except Exception:
    sys.exit(0)
rows = set()
for node, nv in (loss.items() if isinstance(loss, dict) else []):
    if not re.fullmatch(r"cam[0-9]{1,2}|strih|stream", node) or not isinstance(nv, dict):
        continue
    for c in nv.get("classified") or []:
        if not isinstance(c, dict):
            continue
        fi = c.get("frame_index")
        if c.get("png") is None and isinstance(fi, int) and not isinstance(fi, bool) and fi >= 0:
            rows.add((node, fi))
rows = sorted(rows)
if cap > 0:
    rows = rows[:cap]
for node, fi in rows:
    print(f"{node}\t{fi}")
' "$1" "${2:-0}" 2>/dev/null || true
  return 0
}

# The raw-frame ordinals to export for slot indices "$@": each index and its two neighbours (never
# below 0), sorted, unique, one per line. Pure.
missing_slot_pixels_indices() {
  local i
  for i in "$@"; do
    if [ "$i" -gt 0 ]; then printf '%s\n' "$((i - 1))"; fi
    printf '%s\n%s\n' "$i" "$((i + 1))"
  done | sort -n -u
}

# The ffmpeg `select` expression for the ordinals "$@" (already expanded by
# missing_slot_pixels_indices): `select=eq(n\,A)+eq(n\,B)+…` — the comma escaped for the filter
# graph. Pure.
missing_slot_pixels_select_expr() {
  local out="" i
  for i in "$@"; do out="${out:+$out+}eq(n\\,$i)"; done
  printf 'select=%s' "$out"
}

# Stage 1 = the verdict's own decode (src/probe/recording.rs read_frames), split around the input
# path. The Tier-0 test pins both halves to the Rust argument arrays. Pure.
missing_slot_pixels_decode_head() { printf '%s' '-v error -nostdin -i'; }
missing_slot_pixels_decode_tail() { printf '%s' '-f rawvideo -pix_fmt gray pipe:1'; }

# Stage 2 = select by raw-frame ordinal, PNG named after it. $1 = WxH, $2 = select expression (the
# caller quotes it for its shell), $3 = frame count, $4 = the output pattern (caller-quoted). Pure.
missing_slot_pixels_stage2_args() {
  printf -- '-v error -nostdin -f rawvideo -pixel_format gray -video_size %s -framerate 1 -i pipe:0 -vf %s -fps_mode passthrough -frame_pts 1 -frames:v %s %s' \
    "$1" "$2" "$3" "$4"
}

# The strih-lx idle-priority prefix: byte-identical to recording-verdict-on-strih-lx.sh's
# LOWPRIO_SNIPPET (nice 19, pinned to the E-cores when the box has them — issue 1354: a bulk decode
# on the P-cores relock-storms the live NDI receivers). A test pins the two copies equal. Pure.
missing_slot_pixels_lowprio_snippet() {
  # shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
  printf '%s' 'LP="nice -n 19"; if [ -s /sys/devices/cpu_atom/cpus ]; then LP="$LP taskset -c $(cat /sys/devices/cpu_atom/cpus)"; fi;'
}

# The REMOTE bash script for a Linux box: probe the size, then run the two-stage export at idle
# priority into $2 (its exit status is stage 2's). $1 = recording path on the box, $2 = remote output dir,
# $3..$N = the ordinals (missing_slot_pixels_indices output). Every dynamic value is %q-quoted, so
# spaces in an OBS file name are safe. Pure.
missing_slot_pixels_linux_script() {
  local rec="$1" out="$2" expr n
  shift 2
  expr="$(missing_slot_pixels_select_expr "$@")"
  n="$#"
  # shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
  printf 'REC=%q; OUT=%q; mkdir -p "$OUT" || exit 3; ' "$rec" "$out"
  # shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
  printf '%s' 'WH="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0:s=x "$REC" | head -n 1)"; [ -n "$WH" ] || exit 4; '
  printf '%s ' "$(missing_slot_pixels_lowprio_snippet)"
  # shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
  printf '$LP ffmpeg %s "$REC" %s | $LP ffmpeg %s\n' \
    "$(missing_slot_pixels_decode_head)" "$(missing_slot_pixels_decode_tail)" \
    "$(missing_slot_pixels_stage2_args '"$WH"' "$(printf '%q' "$expr")" "$n" '"$OUT/frame-%d.png"')"
}

# A PowerShell single-quoted string literal for $1 (' doubled). Pure.
missing_slot_pixels_ps_quote() {
  printf "'%s'" "${1//\'/\'\'}"
}

# The PowerShell program for a Windows box (the stream box; a Windows strih too): at the shared
# on-box decode priority (onbox_decode_priority_class), probe the size, write the two-stage pipeline
# to a .cmd file and run it through cmd.exe — cmd's `|` is binary-safe, Windows PowerShell 5.1's is
# not. In the .cmd the output pattern's `%` is doubled. $1 = recording path, $2 = remote output dir,
# $3..$N = the ordinals. Returns 1 (prints nothing) when a path holds a character the .cmd line
# cannot carry safely (`"` `%` `^` `&` `|` `<` `>`). Pure.
missing_slot_pixels_windows_ps() {
  local rec="$1" out="$2" expr n prio
  shift 2
  case "$rec$out" in
    *'"'* | *%* | *'^'* | *'&'* | *'|'* | *'<'* | *'>'*) return 1 ;;
  esac
  expr="$(missing_slot_pixels_select_expr "$@")"
  n="$#"
  prio="$(onbox_decode_priority_class)"
  printf '%s\n' \
    "[System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = '$prio'" \
    "\$rec = $(missing_slot_pixels_ps_quote "$rec")" \
    "\$out = $(missing_slot_pixels_ps_quote "$out")" \
    "New-Item -ItemType Directory -Force -Path \$out | Out-Null" \
    "\$wh = (& ffprobe -v error -select_streams v:0 -show_entries 'stream=width,height' -of 'csv=p=0:s=x' \$rec | Select-Object -First 1)" \
    "if (-not \$wh) { exit 4 }" \
    "\$wh = \$wh.Trim()" \
    "\$line = 'ffmpeg $(missing_slot_pixels_decode_head) \"' + \$rec + '\" $(missing_slot_pixels_decode_tail) | ffmpeg ' + ('$(missing_slot_pixels_stage2_args '{0}' "\"$expr\"" "$n" '"{1}\frame-%%d.png"')' -f \$wh, \$out)" \
    "\$cmdFile = Join-Path \$out 'extract.cmd'" \
    "Set-Content -Encoding ASCII -Path \$cmdFile -Value ('@echo off', \$line)" \
    "& cmd.exe /c \$cmdFile" \
    "Get-ChildItem -Name -Path \$out -Filter 'frame-*.png'"
}

# ---- the best-effort runner ------------------------------------------------------------------------

# Export + pull one box's PNGs. $1 = box (strih|stream) $2 = host $3 = os (linux|windows)
# $4 = user $5 = pw $6 = recording path on the box $7 = remote output dir $8 = local parent dir,
# $9.. = the ordinals. Returns 0 when the pull landed, 1 otherwise (loud, never an abort). MUST be
# called from an `if`.
missing_slot_pixels_box_extract() {
  local box="$1" host="$2" os="$3" user="$4" pw="$5" rec="$6" rdir="$7" lparent="$8"
  shift 8
  local tmo="${MISSING_SLOT_PIXELS_TIMEOUT:-600}" script
  local -a ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10)
  mkdir -p "$lparent"
  if [ "$os" = linux ]; then
    script="$(missing_slot_pixels_linux_script "$rec" "$rdir" "$@")"
    if ! timeout "$tmo" sshpass -p "$pw" ssh "${ssh_opts[@]}" "$user@$host" "$script" >/dev/null 2>&1; then
      echo "[missing-slot-pixels] WARNING: $box ($host): export failed or timed out (${tmo}s)" >&2
      return 1
    fi
    if ! sshpass -p "$pw" scp -r "${ssh_opts[@]}" "$user@$host:$rdir" "$lparent/" >/dev/null 2>&1; then
      echo "[missing-slot-pixels] WARNING: $box ($host): pull of $rdir failed" >&2
      return 1
    fi
    sshpass -p "$pw" ssh "${ssh_opts[@]}" "$user@$host" \
      "rm -f -- $(printf '%q' "$rdir")/frame-*.png; rmdir -- $(printf '%q' "$rdir")" >/dev/null 2>&1 || true
    return 0
  fi
  if ! script="$(missing_slot_pixels_windows_ps "$rec" "$rdir" "$@")"; then
    echo "[missing-slot-pixels] WARNING: $box ($host): recording path unusable in a .cmd line: $rec" >&2
    return 1
  fi
  if ! timeout "$tmo" sshpass -p "$pw" ssh "${ssh_opts[@]}" "$user@$host" \
    "powershell -NoProfile -NonInteractive -EncodedCommand $(win_ssh_ps_encoded_command "$script")" >/dev/null 2>&1; then
    echo "[missing-slot-pixels] WARNING: $box ($host): export failed or timed out (${tmo}s)" >&2
    return 1
  fi
  if ! win_ssh_download_dir "$user" "$pw" "$host" "$rdir" "$lparent/" >/dev/null 2>&1; then
    echo "[missing-slot-pixels] WARNING: $box ($host): pull of $rdir failed" >&2
    return 1
  fi
  win_ssh_run "$user" "$pw" "$host" \
    "Remove-Item -Force -Path (Join-Path $(missing_slot_pixels_ps_quote "$rdir") 'frame-*.png'), (Join-Path $(missing_slot_pixels_ps_quote "$rdir") 'extract.cmd') -ErrorAction SilentlyContinue; Remove-Item -LiteralPath $(missing_slot_pixels_ps_quote "$rdir") -ErrorAction SilentlyContinue" \
    >/dev/null 2>&1 || true
  return 0
}

# The whole step. $1 = merged verdict JSON $2 = OUTDIR $3 = RUN_ID $4 = strih host $5 = strih os
# (strih_platform) $6 = strih recording path on the box $7 = stream host $8 = stream recording path on
# the box $9 = the Windows boxes' output dir (OUT_DIR_WIN). Credentials: STRIH_USER/STRIH_PW,
# STREAM_USER/STREAM_PW (default newlevel). The strih-lx remote dir is under
# STRIH_LX_REMOTE_OUT_DIR (default /home/newlevel/verdict-out). Writes
# $OUTDIR/missing-slot-pixels-<RUN_ID>.json and merges it into the verdict JSON as
# `missing_slot_pixels` (report-only — overall_pass untouched). ALWAYS returns 0.
missing_slot_pixels_run() {
  local report="$1" outdir="$2" run_id="$3" strih_host="$4" strih_os="$5" strih_rec="$6"
  local stream_host="$7" stream_rec="$8" win_out="$9"
  [ -f "$report" ] || return 0
  local cap all rows total
  cap="$(missing_slot_pixels_cap)"
  all="$(missing_slot_pixels_frames "$report" 0)"
  if [ -z "$all" ]; then
    echo "    [missing-slot-pixels] issue 1367: no classified slot without a pixel proof — nothing to export"
    return 0
  fi
  total="$(printf '%s\n' "$all" | wc -l)"
  rows="$(missing_slot_pixels_frames "$report" "$cap")"
  echo "    [missing-slot-pixels] issue 1367: $total classified slot(s) without a pixel proof; exporting $(printf '%s\n' "$rows" | wc -l) (cap $cap)"
  local manifest_rows="$outdir/missing-slot-pixels-${run_id}.tsv" box host os user pw rec rdir
  local lparent="$outdir/missing-slot-pixels-raw-${run_id}" status node idx k png pngs
  : >"$manifest_rows"
  for box in strih stream; do
    local -a slots=()
    while IFS=$'\t' read -r node idx; do
      [ -n "$node" ] || continue
      if [ "$(missing_slot_pixels_box_for_node "$node")" = "$box" ]; then slots+=("$idx"); fi
    done <<<"$rows"
    [ "${#slots[@]}" -gt 0 ] || continue
    if [ "$box" = strih ]; then
      host="$strih_host" os="$strih_os" user="${STRIH_USER:-newlevel}" pw="${STRIH_PW:-newlevel}" rec="$strih_rec"
    else
      host="$stream_host" os=windows user="${STREAM_USER:-newlevel}" pw="${STREAM_PW:-newlevel}" rec="$stream_rec"
    fi
    if [ "$os" = linux ]; then
      rdir="${STRIH_LX_REMOTE_OUT_DIR:-/home/newlevel/verdict-out}/missing-slot-pixels-$box-$run_id"
    else
      rdir="$win_out\\missing-slot-pixels-$box-$run_id"
    fi
    status=failed
    case "$rec" in
      '' | '<'*) echo "[missing-slot-pixels] WARNING: $box: no recording path this run — skipped" >&2 ;;
      *)
        # shellcheck disable=SC2046  # one ordinal per word, on purpose
        if missing_slot_pixels_box_extract "$box" "$host" "$os" "$user" "$pw" "$rec" "$rdir" "$lparent" \
          $(missing_slot_pixels_indices "${slots[@]}"); then status=ok; fi
        ;;
    esac
    printf 'box\t%s\t%s\n' "$box" "$status" >>"$manifest_rows"
    while IFS=$'\t' read -r node idx; do
      [ -n "$node" ] || continue
      [ "$(missing_slot_pixels_box_for_node "$node")" = "$box" ] || continue
      pngs=""
      if [ "$status" = ok ]; then
        mkdir -p "$outdir/$node-missing"
        for k in $(missing_slot_pixels_indices "$idx"); do
          png="$lparent/missing-slot-pixels-$box-$run_id/frame-$k.png"
          if [ -f "$png" ]; then
            cp -f -- "$png" "$outdir/$node-missing/frame-$k.png"
            pngs="${pngs:+$pngs,}$outdir/$node-missing/frame-$k.png"
            echo "    [missing-slot-pixels] $node slot $idx: $outdir/$node-missing/frame-$k.png"
          fi
        done
      fi
      printf 'slot\t%s\t%s\t%s\t%s\n' "$node" "$idx" "$box" "$pngs" >>"$manifest_rows"
    done <<<"$rows"
  done
  missing_slot_pixels_manifest "$manifest_rows" "$cap" "$total" "$outdir/missing-slot-pixels-${run_id}.json" "$report"
  return 0
}

# Build the manifest JSON $4 from the runner's TSV $1 (cap $2, total $3) and merge it into the
# verdict JSON $5 as `missing_slot_pixels` (atomic rewrite). A failure is a WARNING. ALWAYS returns 0.
missing_slot_pixels_manifest() {
  python3 -c '
import json, os, sys
tsv, cap, total, out, report = sys.argv[1:6]
boxes, slots = {}, []
with open(tsv) as f:
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if parts[0] == "box":
            boxes[parts[1]] = parts[2]
        elif parts[0] == "slot":
            slots.append({"node": parts[1], "frame_index": int(parts[2]), "box": parts[3],
                          "pngs": [p for p in parts[4].split(",") if p]})
m = {"cap": int(cap), "total_slots": int(total), "boxes": boxes, "slots": slots,
     "exported_slots": sum(1 for s in slots if s["pngs"]),
     "dirs": sorted({os.path.dirname(p) for s in slots for p in s["pngs"]})}
with open(out, "w") as f:
    json.dump(m, f, indent=1, ensure_ascii=False)
with open(report, encoding="utf-8") as f:
    v = json.load(f)
v["missing_slot_pixels"] = m
tmp = report + ".tmp"
with open(tmp, "w", encoding="utf-8") as f:
    json.dump(v, f, indent=2, ensure_ascii=False)
os.replace(tmp, report)
' "$1" "$2" "$3" "$4" "$5" 2>/dev/null \
    || echo "[missing-slot-pixels] WARNING: could not write the manifest / merge it into $5" >&2
  return 0
}
