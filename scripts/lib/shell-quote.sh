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
