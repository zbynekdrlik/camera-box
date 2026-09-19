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

# (Sibling parser: scripts/lib/strih-provision.sh's `strih_ldd_unresolved` LISTS the unresolved
# sonames on the target box for verify-strih.sh; this one FAILS at build time + maps the resolved
# ones to packages. Two contracts, two files -- the standalone CI recorder cannot source the
# provision lib.)
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
        if [ -n "$pkg" ]; then
          out+="${pkg}"$'\n'
        else
          # A resolved SYSTEM path that no apt package owns (an aliased / non-apt lib) -- NOTE it so
          # the gap surfaces at record time rather than only on the box (verify-strih.sh's own
          # `strih_ldd_unresolved` re-check still fails loud there). Not a hard fail here: only an
          # unresolvable soname (`=> not found`, above) is a build-blocking error.
          printf 'runtime_packages_from_ldd: NOTE no apt package owns resolved lib %s (skipped)\n' "$path" >&2
        fi
        ;;
      *) : ;;                                              # vdso / loader lines (no '=>') -> skip
    esac
  done
  [ -n "$out" ] && printf '%s' "$out" | sort -u
  return 0
}

# runtime_packages_with_always_include  (stdin: the ldd-derived package list, one per line) + args:
# always-include apt package names -> print the sorted-unique UNION of stdin and the args, blank
# lines dropped. issue 1317: the strih (Wayland) bundle DLOPENS its Qt platform plugin at runtime
# (`qt6-wayland`), so `ldd` over bin/obs + the *.so files never sees it and it must be FORCED in --
# without it OBS aborts on 26.04 GNOME with `Could not find the Qt platform plugin "wayland"`. The
# strih CI job passes `--extra qt6-wayland`; the non-Wayland imag bundle passes none.
runtime_packages_with_always_include() {
  # returns 0 regardless (sort always succeeds); the `if` avoids a no-args `&&` short-circuit exiting
  # non-zero, and grep's no-match (exit 1 on an all-blank union) is swallowed -- so a caller's
  # `set -e`/pipefail never aborts on an empty or extras-less union.
  { cat; if [ "$#" -gt 0 ]; then printf '%s\n' "$@"; fi; } \
    | { grep -v '^[[:space:]]*$' || true; } | sort -u
}

# --- source-guard: sourced (the tests) -> define the pure helpers only, never run main -------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0 2>/dev/null || true
fi

STAGE=""; OUT=""; EXTRAS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --stage) STAGE="${2:?--stage needs a dir}"; shift 2 ;;
    --out)   OUT="${2:?--out needs a file}"; shift 2 ;;
    --extra) EXTRAS+=("${2:?--extra needs a package name}"); shift 2 ;;   # always-include (dlopen'd at runtime, invisible to ldd)
    *) echo "genlock-runtime-packages.sh: unknown arg '$1' (usage: --stage <dir> --out <file> [--extra <pkg>]...)" >&2; exit 2 ;;
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
# Each `ldd` is `|| true`'d so ONLY runtime_packages_from_ldd's return code governs the pipeline
# (an `ldd` on a non-ELF staged file -- a linker script -- exits 1 and prints nothing useful; under
# `pipefail` that LEFT-side non-zero would otherwise abort the step with a misleading "unresolved
# dependency" error even though no `=> not found` line was ever emitted).
PKGS="$(
  {
    [ -f "$STAGE_ABS/bin/obs" ] && { LD_LIBRARY_PATH="$LLP" ldd "$STAGE_ABS/bin/obs" 2>/dev/null || true; }
    while IFS= read -r _so; do
      LD_LIBRARY_PATH="$LLP" ldd "$_so" 2>/dev/null || true
    done < <(find "$STAGE_ABS" -type f -name '*.so*')
  } | runtime_packages_from_ldd "$STAGE_ABS"
)" || { echo "genlock-runtime-packages.sh: FAILED -- the bundle has an unresolved runtime dependency (see above)" >&2; exit 1; }

# issue 1317: union in the always-include extras (packages the bundle DLOPENS at runtime, invisible
# to ldd -- qt6-wayland on the Wayland strih box). No `--extra` -> unchanged.
if [ "${#EXTRAS[@]}" -gt 0 ]; then
  PKGS="$(printf '%s\n' "$PKGS" | runtime_packages_with_always_include "${EXTRAS[@]}")"
fi

{
  echo "# issue 1317: apt packages the built genlock bundle links against (Qt6 / ffmpeg / libOpenGL /"
  echo "# ...), derived by ldd+dpkg on the build runner. setup-strih.sh installs these before the bundle."
  printf '%s\n' "$PKGS" | grep -v '^[[:space:]]*$' || true
} > "$OUT"

echo "genlock-runtime-packages: wrote $(printf '%s\n' "$PKGS" | grep -cv '^[[:space:]]*$') package(s) -> $OUT"
