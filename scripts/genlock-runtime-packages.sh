#!/usr/bin/env bash
# genlock-runtime-packages.sh (issue 1317): record the apt packages the built genlock bundle links
# against -- see the extended header below the strict-mode line.
set -euo pipefail
#
# WHY: the strih-lx genlock bundle links release-specific Qt6 / ffmpeg 8 / libOpenGL runtime
# libraries. A fresh Ubuntu 26.04 box has none of them, so `obs` dies at exec with
# `libavcodec.so.62: cannot open shared object file`. A hand-curated 26.04 package list rots on
# every soname bump -- the runner that BUILT the binary is the only authority on what it links.
#
# WHAT: the CI Stage step runs this AFTER staging the bundle and BEFORE genlock-manifest.sh (so the
# file is in the bundle manifest). It walks `ldd` over the staged bin/obs + every *.so (with the
# stage lib dirs on LD_LIBRARY_PATH so bundle-internal sonames like libobs.so.30 resolve INSIDE the
# tree and are dropped), maps each remaining resolved SYSTEM library path to its owning apt package
# via `dpkg -S`, and writes the sorted-unique package list to RUNTIME_PACKAGES.txt. setup-strih.sh
# then `apt-get install`s that list before installing the bundle. FAILS LOUD on any `=> not found`.
#
# Pure half `runtime_packages_from_ldd STAGE` (reads ldd output on stdin, maps via `dpkg -S`) is
# sourceable + guarded so tests/genlock_runtime_packages_1317.rs can drive it with fake ldd/dpkg on
# PATH (Tier-0, no cargo).
#
# Usage:  genlock-runtime-packages.sh --stage <dir> --out <file>

# runtime_packages_from_ldd STAGE  (stdin: concatenated `ldd` output) -> print the sorted-unique apt
# package names owning the resolved SYSTEM libraries the bundle links, one per line. Drops sonames
# whose resolved path is INSIDE STAGE (bundle-internal libs). FAILS LOUD (returns 3) on any
# `=> not found` line. `dpkg -S <path>` maps a path to its package (its `pkg:arch: /path` first field).
runtime_packages_from_ldd() {
  local stage="${1:?stage dir required}" line path pkg soname
  local stage_norm="${stage%/}"
  local out=""
  while IFS= read -r line; do
    case "$line" in
      *'=> not found'*)
        soname="${line%%=>*}"
        soname="${soname#"${soname%%[![:space:]]*}"}"   # ltrim
        soname="${soname%"${soname##*[![:space:]]}"}"     # rtrim
        printf 'runtime_packages_from_ldd: UNRESOLVED soname %s (ldd => not found) -- the bundle links a library the build runner cannot resolve\n' "$soname" >&2
        return 3
        ;;
      *'=>'*)
        path="${line#*=> }"
        path="${path%% (0x*}"                             # strip trailing " (0xADDR)"
        path="${path%"${path##*[![:space:]]}"}"           # rtrim
        [ -n "$path" ] || continue
        case "$path" in
          "$stage_norm"/*) continue ;;                    # bundle-internal -> drop
        esac
        pkg="$(dpkg -S "$path" 2>/dev/null | head -n1 | cut -d: -f1)" || true
        [ -n "$pkg" ] && out+="${pkg}"$'\n'
        ;;
      *) : ;;                                              # vdso / loader lines (no '=>') -> skip
    esac
  done
  [ -n "$out" ] && printf '%s' "$out" | sort -u
  return 0
}

# --- source-guard: sourced (the tests) -> define runtime_packages_from_ldd only, never run main -----
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0 2>/dev/null || true
fi

STAGE=""; OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --stage) STAGE="${2:?--stage needs a dir}"; shift 2 ;;
    --out)   OUT="${2:?--out needs a file}"; shift 2 ;;
    *) echo "genlock-runtime-packages.sh: unknown arg '$1' (usage: --stage <dir> --out <file>)" >&2; exit 2 ;;
  esac
done
[ -n "$STAGE" ] || { echo "genlock-runtime-packages.sh: --stage <dir> required" >&2; exit 2; }
[ -n "$OUT" ]   || { echo "genlock-runtime-packages.sh: --out <file> required" >&2; exit 2; }
[ -d "$STAGE" ] || { echo "genlock-runtime-packages.sh: stage dir '$STAGE' not found" >&2; exit 2; }
command -v ldd  >/dev/null 2>&1 || { echo "genlock-runtime-packages.sh: ldd not found" >&2; exit 2; }
command -v dpkg >/dev/null 2>&1 || { echo "genlock-runtime-packages.sh: dpkg not found" >&2; exit 2; }

STAGE_ABS="$(cd "$STAGE" && pwd)"

# LD_LIBRARY_PATH = every dir under the stage that holds a shared object, so the bundle's OWN libs
# (libobs.so.30, libobs-frontend-api.so.30, libcef.so, ...) resolve INSIDE the tree (and are dropped)
# instead of reporting `=> not found`.
LLP=""
while IFS= read -r _d; do
  [ -n "$_d" ] || continue
  LLP="${LLP:+$LLP:}$_d"
done < <(find "$STAGE_ABS" -type f -name '*.so*' -printf '%h\n' | sort -u)

# Collect ldd output over bin/obs + every staged *.so, then map to packages (fail-loud on not-found).
PKGS="$(
  {
    [ -f "$STAGE_ABS/bin/obs" ] && LD_LIBRARY_PATH="$LLP" ldd "$STAGE_ABS/bin/obs" 2>/dev/null
    while IFS= read -r _so; do
      LD_LIBRARY_PATH="$LLP" ldd "$_so" 2>/dev/null
    done < <(find "$STAGE_ABS" -type f -name '*.so*')
  } | runtime_packages_from_ldd "$STAGE_ABS"
)" || { echo "genlock-runtime-packages.sh: FAILED -- the bundle has an unresolved runtime dependency (see above)" >&2; exit 1; }

{
  echo "# issue 1317: apt packages the built genlock bundle links against (Qt6 / ffmpeg / libOpenGL /"
  echo "# ...), derived by ldd+dpkg on the build runner. setup-strih.sh installs these before the bundle."
  printf '%s\n' "$PKGS" | grep -v '^[[:space:]]*$' || true
} > "$OUT"

echo "genlock-runtime-packages: wrote $(printf '%s\n' "$PKGS" | grep -cv '^[[:space:]]*$') package(s) -> $OUT"
