#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure decision helpers, no top-level statements) -- the
# sibling scripts/lib/*.sh convention (strih-provision.sh, obs-fleet.sh) of deliberately NOT setting
# `set -euo pipefail` here: sourcing executes this file in the CALLER's shell, and each caller
# (verify-strih.sh, the tests) sets its own strict mode. Every function returns cleanly under the
# caller's `set -euo pipefail` (no grep pipelines, stdin drained by a bash read loop).
#
# scripts/lib/strih-cef-keyring.sh -- issue 1359: the verify-strih REPORT-ONLY item that grades
# whether the OBS CEF (obs-browser) on strih-lx runs with Chromium's `--password-store=basic`.
#
# Why: GNOME auto-login does not unlock the login keyring. Without the switch, Chromium's os_crypt
# inside the OBS CEF asks libsecret for its storage key at every login, the keyring is locked, and
# GNOME raises an unlock dialog on the operator screen. The vendored
# vendor/obs-studio/plugins/obs-browser/browser-app.cpp appends the switch in
# OnBeforeCommandLineProcessing (the Linux sibling of the macOS use-mock-keychain switch).
#
# Two signals, strongest first:
#   1. a running obs-browser-page command line carries `--password-store=basic` (ok-live);
#   2. the LOADED obs-browser.so carries the `password-store` switch literal (ok-built). On Linux
#      the CEF browser process is OBS itself and the switch is applied to CEF's in-process command
#      line, which Chromium does not necessarily copy onto the child argv, so a correct deploy can
#      show no page with the switch; the compiled-in literal then proves the fix is deployed.
# A plugin without the literal = a bundle that predates issue 1359 (missing); no plugin = unknown.
# Separate from strih-provision.sh (already ~1870 lines) so that file does not grow further.

# strih_cef_so_password_store_state SO_PATH -> prints `carries` when the file contains the
# `password-store` switch literal, `missing` when it is readable but does not, `absent` when the
# path is empty / not a readable regular file. Always rc 0 (safe as a bare assignment under set -e).
strih_cef_so_password_store_state() {
  local so="${1-}"
  if [ -z "$so" ] || [ ! -f "$so" ] || [ ! -r "$so" ]; then
    printf 'absent'
    return 0
  fi
  if LC_ALL=C grep -qaF -- 'password-store' "$so" 2>/dev/null; then
    printf 'carries'
  else
    printf 'missing'
  fi
  return 0
}

# strih_cef_password_store_verdict SO_STATE (stdin: `pgrep -af obs-browser-page` lines, may be
# empty) -> ONE token, rc 0 iff ok-*:
#   ok-live   a running obs-browser-page command line carries `--password-store=basic`
#   ok-built  no page shows it, but SO_STATE=carries (the loaded plugin has the switch compiled in)
#   missing   SO_STATE=missing -- the deployed obs-browser.so predates issue 1359
#   unknown   anything else (plugin absent / unreadable)
# Only the exact `--password-store=basic` value counts as live; `--password-store=gnome-libsecret`
# is precisely the keyring behaviour this removes. stdin is fully drained (no SIGPIPE upstream).
strih_cef_password_store_verdict() {
  local so_state="${1-}" line live=0
  while IFS= read -r line || [ -n "$line" ]; do
    case " $line " in
      *" --password-store=basic "*) live=1 ;;
    esac
  done
  if [ "$live" = 1 ]; then printf 'ok-live'; return 0; fi
  case "$so_state" in
    carries) printf 'ok-built'; return 0 ;;
    missing) printf 'missing'; return 1 ;;
    *) printf 'unknown'; return 1 ;;
  esac
}
