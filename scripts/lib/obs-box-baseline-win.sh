#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing caller owns strict mode (sourcing runs in the caller's shell); the grader must grade every item and never abort mid-way
# obs-box-baseline-win.sh (issue 1357) -- the ONE Windows OBS-box baseline: the list, a PURE grader,
# the read-only gather program and the deploy's power-plan set-and-verify block.
#
# WHY: on 26.9.2026 RESOLUME-SNV ran the Windows `Balanced` power plan while stream / mbc ran
# `Bitsum Highest Performance` and fohabl `High performance`. Balanced produced 10-22 ms host stalls
# that made the FOH VB-Matrix underrun on BOTH resolume VBAN senders (U+58/+50 per 5 min, U+1/+16
# after switching to High performance). A per-box difference nothing detected. This is the Windows
# sibling of the Linux baseline (scripts/lib/obs-box-baseline.sh + obs-box-baseline-verify.sh): the
# same split into a thin read-only GATHER that runs on the box and a PURE grader over its text.
#
# THE LIST (graded per item, verdict OK / DRIFT / UNKNOWN -- an unreadable item is UNKNOWN, never OK):
#   power_scheme          the active power scheme is max-performance class: the stock High performance
#                         or Ultimate Performance GUID, or a scheme NAMED High performance / Ultimate
#                         Performance / Bitsum Highest Performance (stream's High performance is a
#                         duplicate with its own GUID, and Process Lasso's Bitsum GUID is per-install)
#   sleep_ac              "Sleep after" (STANDBYIDLE) on AC = 0 (never)
#   hibernate_ac          "Hibernate after" (HIBERNATEIDLE) on AC = 0 (never)
#   usb_selective_suspend USB selective suspend on AC = 0 (Disabled)
#   wer_dontshowui        HKLM\SOFTWARE\Microsoft\Windows\Windows Error Reporting DontShowUI = 1, so no
#                         crash dialog blocks an unattended OBS (absent = the dialog shows = DRIFT)
# NOT graded: the timer resolution / MMCSS (not settable persistently).
#
# MUTATION POLICY (the design, Approach 1): ONLY the power plan is ever SET, and only by the Windows
# genlock deploy program (scripts/deploy-genlock-fleet.sh embeds win_baseline_power_plan_ensure_ps).
# Sleep / USB / WER are owner machine settings -- reported, never written.
#
#   win_baseline_gather_ps1                 -> the read-only gather program (run it as a FILE:
#                                              `powershell -NoProfile -ExecutionPolicy Bypass -File x.ps1`,
#                                              never nested PowerShell over ssh -- rig-state-inspection.md)
#   win_baseline_grade FILE                 -> `<item> <OK|DRIFT|UNKNOWN> <detail>` per item in list order;
#                                              rc 20 any DRIFT, else 11 any UNKNOWN, else 0
#   win_baseline_scheme_is_maxperf GUID NAME -> rc 0 iff the scheme is max-performance class
#   win_baseline_power_plan_ensure_ps       -> the deploy's set-and-verify PowerShell block
#
# The gather output format (the grader's input): `==WINBASELINE-BEGIN== v1 host=<name>`, then per item
# `==WINBASELINE-SECTION== <item>`, the command's raw stdout+stderr lines, `==WINBASELINE-EXIT== <rc>`,
# and a final `==WINBASELINE-END==`. CRLF is tolerated. A section without its EXIT marker (the gather
# died inside it) is unread -> UNKNOWN. Tests: tests/python/test_obs_box_baseline_win_1357.py.

WIN_BASELINE_ITEMS="power_scheme sleep_ac hibernate_ac usb_selective_suspend wer_dontshowui"
WIN_BASELINE_HIGH_PERF_GUID="8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c"
WIN_BASELINE_HIGH_PERF_NAME="High performance"
# space-separated stock GUIDs (lower case) / `|`-separated scheme NAMES (compared case-insensitively)
WIN_BASELINE_MAXPERF_GUIDS="${WIN_BASELINE_HIGH_PERF_GUID} e9a42b02-d5df-448d-aa00-03f14749eb61"
WIN_BASELINE_MAXPERF_NAMES="${WIN_BASELINE_HIGH_PERF_NAME}|Ultimate Performance|Bitsum Highest Performance"
WIN_BASELINE_SLEEP_GUID="29f6c1db-86da-48c5-9fdb-f2b67b1f44da"
WIN_BASELINE_HIBERNATE_GUID="9d7815a6-7ee4-497e-8888-515a05f02364"
WIN_BASELINE_USB_SUBGROUP="2a737441-1930-4402-8d77-b2bebba308a3"
WIN_BASELINE_USB_SS_GUID="48e6b7a6-50f5-4782-a5d4-53bb8f07e226"
WIN_BASELINE_WER_KEY='HKLM\SOFTWARE\Microsoft\Windows\Windows Error Reporting'

# win_baseline_scheme_is_maxperf GUID NAME -> rc 0 iff GUID is a stock max-performance GUID or NAME is a
# max-performance scheme name (case-insensitive). An empty GUID and NAME is never max-performance.
win_baseline_scheme_is_maxperf() {
    local guid="${1,,}" name="${2,,}" g n
    for g in $WIN_BASELINE_MAXPERF_GUIDS; do
        [ -n "$guid" ] && [ "$guid" = "$g" ] && return 0
    done
    local IFS='|'
    for n in $WIN_BASELINE_MAXPERF_NAMES; do
        [ -n "$name" ] && [ "$name" = "${n,,}" ] && return 0
    done
    return 1
}

# _win_baseline_section FILE ITEM -> the section's body lines (CR stripped) on stdout; rc 0 only when
# the section exists AND ended with its EXIT marker (a complete read), rc 1 otherwise. First match wins.
_win_baseline_section() {
    local file="$1" id="$2"
    [ -r "$file" ] || return 1
    awk -v id="$id" '
        { sub(/\r$/, "") }
        done { next }
        !on && !stop && $0 == "==WINBASELINE-SECTION== " id { on = 1; next }
        on && /^==WINBASELINE-EXIT==/ { on = 0; done = 1; next }
        on && /^==WINBASELINE-/ { on = 0; stop = 1; next }
        on && !stop { print }
        END { exit done ? 0 : 1 }' "$file"
}

# _win_baseline_ac_index BODY SETTING_GUID -> the decimal "Current AC Power Setting Index" of the
# `powercfg /q` section BODY, only when BODY is a reading of SETTING_GUID; empty otherwise.
_win_baseline_ac_index() {
    local body="$1" want="${2,,}" line seen=0
    while IFS= read -r line; do
        if [[ "${line,,}" =~ power\ setting\ guid:\ ([0-9a-f-]{36}) ]]; then
            [ "${BASH_REMATCH[1]}" = "$want" ] && seen=1 || seen=0
        elif [ "$seen" = 1 ] && [[ "$line" =~ Current\ AC\ Power\ Setting\ Index:\ 0x([0-9a-fA-F]{1,8})[[:space:]]*$ ]]; then
            printf '%d' "$((16#${BASH_REMATCH[1]}))"
            return 0
        fi
    done <<<"$body"
    return 1
}

_win_baseline_grade_power() {
    local body line guid name
    body="$(_win_baseline_section "$1" power_scheme)" || { echo "UNKNOWN active scheme unread (no complete power_scheme section)"; return; }
    while IFS= read -r line; do
        if [[ "$line" =~ Power\ Scheme\ GUID:\ ([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})[[:space:]]+\((.*)\)[[:space:]]*$ ]]; then
            guid="${BASH_REMATCH[1],,}"; name="${BASH_REMATCH[2]}"
            break
        fi
    done <<<"$body"
    if [ -z "$guid" ]; then
        echo "UNKNOWN active scheme unparseable (powercfg /getactivescheme)"
    elif win_baseline_scheme_is_maxperf "$guid" "$name"; then
        echo "OK ${name} (${guid})"
    else
        echo "DRIFT ${name} (${guid}) is not max-performance class (want High performance / Ultimate / Bitsum Highest Performance)"
    fi
}

# _win_baseline_grade_idle FILE ITEM SETTING_GUID LABEL -> a "never on AC" timeout item.
_win_baseline_grade_idle() {
    local body v
    body="$(_win_baseline_section "$1" "$2")" || { echo "UNKNOWN $4 unread (no complete $2 section)"; return; }
    v="$(_win_baseline_ac_index "$body" "$3")" || { echo "UNKNOWN $4 AC index unparseable"; return; }
    if [ "$v" = 0 ]; then
        echo "OK never on AC"
    else
        echo "DRIFT $4 after ${v} s on AC (want 0 = never)"
    fi
}

_win_baseline_grade_usb() {
    local body v
    body="$(_win_baseline_section "$1" usb_selective_suspend)" || { echo "UNKNOWN USB selective suspend unread (no complete section)"; return; }
    v="$(_win_baseline_ac_index "$body" "$WIN_BASELINE_USB_SS_GUID")" || { echo "UNKNOWN USB selective suspend AC index unparseable"; return; }
    case "$v" in
        0) echo "OK Disabled on AC" ;;
        1) echo "DRIFT Enabled on AC (want Disabled)" ;;
        *) echo "UNKNOWN unexpected USB selective suspend AC index ${v}" ;;
    esac
}

_win_baseline_grade_wer() {
    local body line v
    body="$(_win_baseline_section "$1" wer_dontshowui)" || { echo "UNKNOWN DontShowUI unread (no complete section)"; return; }
    while IFS= read -r line; do
        if [[ "$line" =~ ^[[:space:]]*DontShowUI[[:space:]]+REG_DWORD[[:space:]]+0x([0-9a-fA-F]{1,8})[[:space:]]*$ ]]; then
            v="$((16#${BASH_REMATCH[1]}))"
            break
        fi
    done <<<"$body"
    if [ -n "$v" ]; then
        if [ "$v" = 1 ]; then echo "OK DontShowUI=0x1"; else printf 'DRIFT DontShowUI=0x%x (want 0x1: a crash dialog can block an unattended OBS)\n' "$v"; fi
    elif grep -q 'unable to find the specified registry key or value' <<<"$body"; then
        echo "DRIFT DontShowUI not set (absent = Windows shows the crash dialog; want 0x1)"
    else
        echo "UNKNOWN DontShowUI unparseable (reg query)"
    fi
}

# win_baseline_grade FILE -> one `<item> <VERDICT> <detail>` line per item, in WIN_BASELINE_ITEMS order.
# Pure over FILE (the gather output). rc 20 = at least one DRIFT, 11 = at least one UNKNOWN and no
# DRIFT, 0 = every item OK. A missing / unreadable FILE grades every item UNKNOWN.
win_baseline_grade() {
    local file="${1:-}" item res drift=0 unknown=0
    for item in $WIN_BASELINE_ITEMS; do
        case "$item" in
            power_scheme) res="$(_win_baseline_grade_power "$file")" ;;
            sleep_ac) res="$(_win_baseline_grade_idle "$file" sleep_ac "$WIN_BASELINE_SLEEP_GUID" "sleep")" ;;
            hibernate_ac) res="$(_win_baseline_grade_idle "$file" hibernate_ac "$WIN_BASELINE_HIBERNATE_GUID" "hibernate")" ;;
            usb_selective_suspend) res="$(_win_baseline_grade_usb "$file")" ;;
            wer_dontshowui) res="$(_win_baseline_grade_wer "$file")" ;;
        esac
        case "${res%% *}" in
            DRIFT) drift=1 ;;
            OK) ;;
            *) unknown=1 ;;
        esac
        printf '%s %s\n' "$item" "$res"
    done
    [ "$drift" = 1 ] && return 20
    [ "$unknown" = 1 ] && return 11
    return 0
}

# win_baseline_report_rows NAME=FILE... -> the REPORT-ONLY rows version-integrity-gate.sh prints for its
# --win-baseline facet: a header, then `<name> win_baseline <item> <VERDICT> <detail>` per item of each
# gather FILE. Always rc 0 -- a drifted or unread baseline names itself, it never blocks the gate
# (a `NAME` with no `=FILE`, or an unreadable FILE, grades every item UNKNOWN).
win_baseline_report_rows() {
    local entry name file item verdict detail
    echo
    echo "  -- Windows OBS-box baseline (issue 1357: report-only, NEVER gates the run) --"
    for entry in "$@"; do
        name="${entry%%=*}"; file="${entry#*=}"
        [ "$file" = "$entry" ] && file=""
        while read -r item verdict detail; do
            [ -n "$item" ] || continue
            printf '  %-14s win_baseline %-22s %-7s %s (report-only, does NOT block)\n' \
                "$name" "$item" "$verdict" "$detail"
        done < <(win_baseline_grade "$file" || true)
    done
    return 0
}

# win_baseline_gather_ps1 -> the READ-ONLY gather program. It only runs `powercfg /getactivescheme`,
# `powercfg /q` and `reg query`; stdout AND stderr of each land in its section (the grader reads the
# `reg query` "unable to find" message as "value absent"). Emitted text, run by the caller as a file.
win_baseline_gather_ps1() {
    cat <<PSHEAD
# issue 1357 -- Windows OBS-box baseline GATHER (READ-ONLY; emitted by scripts/lib/obs-box-baseline-win.sh).
# Run it as a FILE: powershell -NoProfile -ExecutionPolicy Bypass -File <path>. It writes nothing.
\$wbUsbSub = '${WIN_BASELINE_USB_SUBGROUP}'
\$wbUsbSs = '${WIN_BASELINE_USB_SS_GUID}'
\$wbWerKey = '${WIN_BASELINE_WER_KEY}'
PSHEAD
    cat <<'PSBODY'
$ErrorActionPreference = 'Continue'
function Write-WbSection([string]$Id, [scriptblock]$Read) {
  Write-Output "==WINBASELINE-SECTION== $Id"
  $global:LASTEXITCODE = 0
  $lines = @(& $Read 2>&1 | ForEach-Object { "$_" })
  $rc = $LASTEXITCODE
  foreach ($l in $lines) { Write-Output $l }
  Write-Output "==WINBASELINE-EXIT== $rc"
}
Write-Output "==WINBASELINE-BEGIN== v1 host=$env:COMPUTERNAME"
Write-WbSection 'power_scheme' { powercfg /getactivescheme }
Write-WbSection 'sleep_ac' { powercfg /q SCHEME_CURRENT SUB_SLEEP STANDBYIDLE }
Write-WbSection 'hibernate_ac' { powercfg /q SCHEME_CURRENT SUB_SLEEP HIBERNATEIDLE }
Write-WbSection 'usb_selective_suspend' { powercfg /q SCHEME_CURRENT $wbUsbSub $wbUsbSs }
Write-WbSection 'wer_dontshowui' { reg query $wbWerKey /v DontShowUI }
Write-Output "==WINBASELINE-END=="
exit 0
PSBODY
}

# win_baseline_power_plan_ensure_ps -> the deploy program's step (0b): when the active scheme is NOT
# max-performance class, activate the INSTALLED High performance scheme (the stock GUID when present,
# else the first scheme NAMED High performance) and read it back; a failed read / no such scheme /
# a set that does not take fails loud (exit 11) BEFORE the deploy touches OBS. It is the ONLY baseline
# mutation; an already max-performance scheme (Bitsum, Ultimate, a duplicated High performance) is left
# exactly as it is. The lists come from the constants above (the ONE list).
win_baseline_power_plan_ensure_ps() {
    local guids="" names="" g n
    for g in $WIN_BASELINE_MAXPERF_GUIDS; do guids="${guids:+$guids, }'${g}'"; done
    local IFS='|'
    for n in $WIN_BASELINE_MAXPERF_NAMES; do names="${names:+$names, }'${n//\'/\'\'}'"; done
    unset IFS
    cat <<PSHEAD
# (0b) issue 1357 -- Windows OBS-box baseline POWER PLAN, the ONLY baseline item this deploy sets
#      (sleep / USB selective suspend / WER stay report-only: scripts/win-baseline-check.sh). The
#      Balanced plan made resolume's host stall and the FOH VB-Matrix underrun on both VBAN senders
#      (26.9.2026). Not max-performance class -> activate the installed High performance scheme and
#      read it back; any failure stops here, before OBS is touched.
\$wbMaxPerfGuids = @(${guids})
\$wbMaxPerfNames = @(${names})
\$wbHighPerfGuid = '${WIN_BASELINE_HIGH_PERF_GUID}'
\$wbHighPerfName = '${WIN_BASELINE_HIGH_PERF_NAME}'
PSHEAD
    cat <<'PSBODY'
$wbGuidRe = '([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\s+\((.*)\)'
function Get-WbActiveScheme {
  $t = (powercfg /getactivescheme) -join ' '
  if ($t -match $wbGuidRe) { return @($Matches[1].ToLower(), $Matches[2].Trim()) }
  return $null
}
function Test-WbMaxPerf($s) {
  if (-not $s) { return $false }
  return (($wbMaxPerfGuids -contains $s[0]) -or ($wbMaxPerfNames -contains $s[1]))
}
$wbBefore = Get-WbActiveScheme
if (-not $wbBefore) { Write-Error "issue 1357 FAIL: could not read the active power scheme -- fix it before deploying"; exit 11 }
if (Test-WbMaxPerf $wbBefore) {
  Write-Host "issue 1357 power plan: $($wbBefore[1]) ($($wbBefore[0])) is max-performance class -- unchanged."
} else {
  $wbTarget = $null
  foreach ($wbLine in @(powercfg /list)) {
    if ($wbLine -match $wbGuidRe) {
      $wbG = $Matches[1].ToLower(); $wbN = $Matches[2].Trim()
      if ($wbG -eq $wbHighPerfGuid) { $wbTarget = @($wbG, $wbN); break }
      if ((-not $wbTarget) -and ($wbN -eq $wbHighPerfName)) { $wbTarget = @($wbG, $wbN) }
    }
  }
  if (-not $wbTarget) { Write-Error "issue 1357 FAIL: the active power scheme $($wbBefore[1]) ($($wbBefore[0])) is not max-performance class and no High performance scheme is installed -- activate a max-performance scheme by hand, then rerun"; exit 11 }
  powercfg /setactive $wbTarget[0]
  $wbAfter = Get-WbActiveScheme
  if ((-not $wbAfter) -or ($wbAfter[0] -ne $wbTarget[0])) { Write-Error "issue 1357 FAIL: activating $($wbTarget[1]) ($($wbTarget[0])) did not take -- the active scheme reads $($wbAfter -join ' ')"; exit 11 }
  Write-Host "issue 1357 power plan: SET $($wbAfter[1]) ($($wbAfter[0])), was $($wbBefore[1]) ($($wbBefore[0])) -- read back OK."
}
PSBODY
}
