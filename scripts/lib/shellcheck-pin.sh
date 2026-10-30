#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + one function, no top-level commands) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak
# into it.
#
# scripts/lib/shellcheck-pin.sh -- issue 1395: the ONE pinned shellcheck the repo lints with, used
# by the CI `shellcheck` job and by any local full run.
#
# Why a pin and not the distro shellcheck: the full run `shellcheck -S warning scripts/*.sh
# scripts/lib/*.sh` follows every `# shellcheck source=` directive whose target is one of its
# input files, so each entry script is analysed together with its whole sourced closure, and the
# dataflow (CFG) analysis grows superlinearly with that program: on 26.10.2026 the full run
# passed 12 GB RSS with the ubuntu 0.9.0 (it froze dev1 twice). The 11 heaviest entry scripts
# carry a file-scoped directive on line 2 that turns that extended analysis off, which only the
# 0.10.0+ releases know (0.9.0 reports it as SC1107 and still blows up). With this pin the full
# run peaks under 1 GB. See .claude/rules/ci-testing-gotchas.md.
#
# Function: shellcheck_pin_install DIR -> installs the pinned release into DIR (download, sha256 check,
#   extract; a no-op when DIR already holds this version) and prints the binary's path. Any
#   failure (no network, wrong checksum) returns non-zero with a message on stderr; never falls
#   back to another shellcheck.

SHELLCHECK_PIN_VERSION="0.11.0"
SHELLCHECK_PIN_SHA256="8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198"
SHELLCHECK_PIN_URL="https://github.com/koalaman/shellcheck/releases/download/v${SHELLCHECK_PIN_VERSION}/shellcheck-v${SHELLCHECK_PIN_VERSION}.linux.x86_64.tar.xz"

shellcheck_pin_install() {
  local dir="${1:?shellcheck_pin_install: DIR required}"
  local bin="$dir/shellcheck-v${SHELLCHECK_PIN_VERSION}/shellcheck"
  if [ -x "$bin" ] && "$bin" --version 2>/dev/null | grep -qx "version: ${SHELLCHECK_PIN_VERSION}"; then
    printf '%s\n' "$bin"
    return 0
  fi
  mkdir -p "$dir" || return 1
  local tarball="$dir/shellcheck-v${SHELLCHECK_PIN_VERSION}.tar.xz"
  if ! curl -fsSL --retry 3 -o "$tarball" "$SHELLCHECK_PIN_URL"; then
    echo "shellcheck-pin: download of $SHELLCHECK_PIN_URL failed" >&2
    return 1
  fi
  if ! printf '%s  %s\n' "$SHELLCHECK_PIN_SHA256" "$tarball" | sha256sum -c --status; then
    echo "shellcheck-pin: sha256 mismatch for $tarball (expected $SHELLCHECK_PIN_SHA256)" >&2
    rm -f "$tarball"
    return 1
  fi
  if ! tar -xJf "$tarball" -C "$dir"; then
    echo "shellcheck-pin: extracting $tarball failed" >&2
    return 1
  fi
  rm -f "$tarball"
  if ! "$bin" --version | grep -qx "version: ${SHELLCHECK_PIN_VERSION}"; then
    echo "shellcheck-pin: $bin does not report version ${SHELLCHECK_PIN_VERSION}" >&2
    return 1
  fi
  printf '%s\n' "$bin"
}
