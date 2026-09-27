#!/usr/bin/env bash
# Issue 1372: emit the dantesync tray-only program and RUN it against the stubbed node in
# tests/pwsh/dantesync_tray_swap_1372.ps1 (see its header). Also parses the full upgrade program.
set -euo pipefail

# Needs pwsh: $PWSH, else `pwsh` on PATH. dev1 has none; unpack the powershell-7.x-linux-x64
# release tarball into a scratch dir and point PWSH at it. A missing pwsh FAILS, never skips.
# Exit 0 = every case ok, 1 = a case failed or the full program has parse errors, 2 = no pwsh.
TRAY_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TRAY_TEST_ROOT="$(cd "$TRAY_TEST_DIR/../.." && pwd)"
PWSH="${PWSH:-$(command -v pwsh || true)}"
if [ -z "$PWSH" ] || [ ! -x "$PWSH" ]; then
  echo "FAIL: no pwsh (set PWSH=/path/to/pwsh; dev1 has none -- unpack the powershell-7.x-linux-x64 release tarball)" >&2
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# the emitters are pure bash functions; sourcing the upgrade script runs nothing (source guard)
# shellcheck source=scripts/dantesync-fleet-upgrade.sh
. "$TRAY_TEST_ROOT/scripts/dantesync-fleet-upgrade.sh"
dantesync_windows_tray_only_ps 1.12.0 >"$WORK/tray-only.ps1"
dantesync_windows_upgrade_ps 1.12.0 >"$WORK/full.ps1"

"$PWSH" -NoProfile -NonInteractive -File "$TRAY_TEST_DIR/dantesync_tray_swap_1372.ps1" -Program "$WORK/tray-only.ps1"

errors="$("$PWSH" -NoProfile -NonInteractive -Command \
  "\$e = \$null; [void][System.Management.Automation.Language.Parser]::ParseFile('$WORK/full.ps1', [ref]\$null, [ref]\$e); \$e.Count")"
echo "full upgrade program parse errors: $errors"
[ "$errors" = "0" ]
