#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines a pure function only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (mv-reverify-escalate.sh, mv-fps-preflight.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's
# shell, so strict mode here would leak into whichever caller sources it (the dev1 alert-watchdogs
# run `set -uo pipefail` WITHOUT `-e`, and the Tier-0 harness sources them). This is exactly why the
# #1258 fix inlined its encode rather than sourcing scripts/lib/win-ssh-exec.sh (which carries its
# own top-level `set -euo pipefail`).
#
# scripts/lib/ps-encoded.sh -- issue 1259 (issue 1258 fleet follow-up): the ONE shared helper that
# base64-UTF16LE-encodes a PowerShell command string for `powershell -EncodedCommand`. strih/stream
# run Win32-OpenSSH whose default shell is cmd.exe; a naive `ssh host "powershell -Command \"…| sort
# …\""` leaks its `|` pipes (and `$`/`;`/`{}`/`()`) to cmd.exe BEFORE PowerShell parses them -> a
# mangled/blind read (the issue-1258 4/4-INCONCLUSIVE root cause). The base64 blob is pure ASCII with
# no shell-special char, so cmd.exe cannot touch it and PowerShell decodes it back to the exact
# command -- the same mechanism scripts/lib/win-ssh-exec.sh::win_ssh_run already uses.

# ps_encoded_command <ps-command-text> -> stdout: the base64 UTF-16LE encoding of the PowerShell
# command, ready to splice after `powershell -NoProfile -NonInteractive -EncodedCommand `. Pure (no
# network). ALWAYS exits 0: iconv + base64 are present fleet-wide (win_ssh_ps_encoded_command relies
# on the same pair), but if either is somehow absent the encode yields "" -> an empty -EncodedCommand
# -> an empty read -> the caller's classifier treats it as INCONCLUSIVE/UNKNOWN, NEVER an abort (the
# self-contained-under-a-future-`set -e`-caller discipline the source-only libs document).
ps_encoded_command() {
  local _out
  _out="$(printf '%s' "$1" | iconv -f UTF-8 -t UTF-16LE 2>/dev/null | base64 -w0 2>/dev/null)" || _out=""
  printf '%s' "$_out"
}
