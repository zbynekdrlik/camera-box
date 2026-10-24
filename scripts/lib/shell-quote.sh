#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one function, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak
# into it.
#
# scripts/lib/shell-quote.sh -- issue 1462: the ONE way a dev1 builder quotes a value (a recording
# path, a WAV path) into REMOTE bash text it sends over plain ssh.
#
#   shell_single_quote TEXT -> TEXT as ONE single-quoted shell word: every ' becomes '\'' and the
#   whole value is wrapped in '...'. The remote shell then takes it byte for byte: no $-expansion,
#   no command substitution, no globbing, a space stays inside the one word. Not printf %q: that
#   prints bash-only $'...' for control characters, and the text may run under a POSIX sh. The
#   output always ends with the closing quote, so a $(...) caller never loses a trailing newline
#   of TEXT (command substitution strips trailing newlines only).
#
# Callers lazy-source it from the function that needs it:
#   command -v shell_single_quote >/dev/null 2>&1 || . "${BASH_SOURCE[0]%/*}/shell-quote.sh"

shell_single_quote() {
  local s="${1-}" q="'\\''"
  printf "'%s'" "${s//\'/$q}"
}

# shell_ssh_rm_plan_line INDENT LABEL USER HOST PATH -> the ONE #652 cleanup PLAN line of a Linux OBS
# box's recording (strih-snv issue 1317 part 4, stream-snv issue 1474): `<INDENT><LABEL padded to 22>
# ssh USER@HOST "rm -f -- '<PATH>'"`, an exact-path rm, never a glob or a sweep. The line is pasted
# into a LOCAL shell and ssh hands the joined command to the REMOTE shell, so PATH is quoted for BOTH
# parses: single-quoted for the remote shell, and that remote command is one double-quoted argument
# for the local shell (\ " $ ` escaped). An OBS default filename carries a space; it stays one word.
shell_ssh_rm_plan_line() {
  local indent="${1-}" label="${2-}" user="${3-}" host="${4-}" path="${5-}" remote dq
  remote="rm -f -- $(shell_single_quote "$path")"
  dq="${remote//\\/\\\\}"
  dq="${dq//\"/\\\"}"
  dq="${dq//\$/\\\$}"
  dq="${dq//\`/\\\`}"
  printf '%s%-22s ssh %s@%s "%s"\n' "$indent" "$label" "$user" "$host" "$dq"
}
