#!/usr/bin/env bash
#
# update-av-stack.sh — engine for the /update-av-stack slash command (#44).
#
# The genlock monorepo (vendor/) carries fresh upstream OBS + DistroAV releases with our
# genlock patches applied on top, imported via `git subtree --squash` (see vendor/README.md).
# This script automates the "are we behind upstream, and what is the exact subtree-pull to
# catch up" half of the release-bump flow so production boxes never linger on an old release.
#
# It is split into PURE functions (manifest parse, version compare, command generation —
# unit-tested from tests/av_stack_update.rs by sourcing this file) and a network/mutating
# flow (--check fetches upstream tags, --apply runs `git subtree pull`). The source-guard
# below (BASH_SOURCE != $0) lets the tests exercise the pure functions in isolation without
# triggering any network call or git mutation — the same convention as scripts/loopback-e2e.sh.
#
# Usage:
#   scripts/update-av-stack.sh [--check] [--readme PATH]   # default: report drift (read-only)
#   scripts/update-av-stack.sh --apply  [--readme PATH]     # run git subtree pull for behind comps
#   scripts/update-av-stack.sh --help
#
# Exit codes: 0 = every component up to date, 10 = at least one behind, 11 = upstream
# unreachable for at least one component (drift UNKNOWN — never reported as up to date), 1 = error.

set -euo pipefail

DEFAULT_README="vendor/README.md"

# --- PURE functions (no network, no git mutation — unit-tested) ---------------------------

# normalize_url RAW  ->  canonical clonable URL (https://host/owner/repo.git).
# Accepts bare "github.com/o/r", scheme-prefixed, and/or .git-suffixed forms; idempotent.
normalize_url() {
  local u="$1"
  u="${u#https://}"
  u="${u#http://}"
  u="${u%/}"
  u="${u%.git}"
  printf 'https://%s.git' "$u"
}

# parse_manifest README  ->  one TSV line "PREFIX\tURL\tVERSION\tCOMMIT" per subtree component.
# Reads the vendor table in README; only rows whose "imported as" cell says `subtree` and
# whose dir cell names a `vendor/<dir>` prefix are emitted (the NDI-headers row, the header
# row and the |---| separator carry no "subtree" and are skipped). URL is canonicalized.
parse_manifest() {
  local readme="$1" line prefix url ver commit f_dir f_url f_ver
  [ -f "$readme" ] || { echo "parse_manifest: no such file: $readme" >&2; return 1; }
  while IFS= read -r line; do
    case "$line" in
      '|'*) : ;;          # candidate table row
      *) continue ;;
    esac
    grep -q 'subtree' <<<"$line" || continue
    f_dir="$(printf '%s' "$line"  | awk -F'|' '{print $2}')"
    f_url="$(printf '%s' "$line"  | awk -F'|' '{print $3}')"
    f_ver="$(printf '%s' "$line"  | awk -F'|' '{print $4}')"
    prefix="$(printf '%s' "$f_dir" | grep -oE 'vendor/[A-Za-z0-9._/-]+' | head -1)"
    [ -n "$prefix" ] || continue
    url="$(printf '%s' "$f_url" | tr -d ' `')"
    url="$(normalize_url "$url")"
    ver="$(printf '%s' "$f_ver" | sed -n 's/.*\*\*\([^*]*\)\*\*.*/\1/p')"
    # The backticks below are LITERAL markdown delimiters in the sed pattern, not command
    # substitution — single quotes (no expansion) are exactly what is wanted here.
    # shellcheck disable=SC2016
    commit="$(printf '%s' "$f_ver" | sed -n 's/.*commit `\([^`]*\)`.*/\1/p')"
    printf '%s\t%s\t%s\t%s\n' "$prefix" "$url" "$ver" "$commit"
  done < "$readme"
}

# version_status CURRENT LATEST  ->  UP-TO-DATE | BEHIND | AHEAD | UNKNOWN
# Numeric semver comparison (sort -V), tolerant of a leading "v". Empty LATEST => UNKNOWN
# (upstream lookup failed) so the caller never silently treats "couldn't check" as "current".
version_status() {
  local cur="${1#v}" lat="${2#v}" highest
  [ -n "$lat" ] || { echo "UNKNOWN"; return 0; }
  if [ "$cur" = "$lat" ]; then echo "UP-TO-DATE"; return 0; fi
  highest="$(printf '%s\n%s\n' "$cur" "$lat" | sort -V | tail -1)"
  if [ "$highest" = "$lat" ]; then echo "BEHIND"; else echo "AHEAD"; fi
}

# subtree_pull_cmd PREFIX URL TAG  ->  the exact catch-up command for that component.
# The explicit `-m` is required: `git subtree pull --squash` with no message opens $EDITOR
# for the merge commit on a tty, which would hang the interactive/autonomous --apply flow.
subtree_pull_cmd() {
  printf "git subtree pull --prefix=%s %s %s --squash -m 'vendor: update %s to %s'" \
    "$1" "$2" "$3" "$1" "$3"
}

# --- source-guard: when sourced (the unit tests), stop here -------------------------------
# A sourced file has BASH_SOURCE[0] != $0; the executed script has them equal and runs main.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- network + mutating flow (executed only when run directly) ----------------------------

# latest_stable_tag URL  ->  highest stable X.Y.Z tag on the remote ("" if none/unreachable).
# Stable = digits-and-dots only (drops rc/beta/alpha and ^{} deref lines). Never fails the
# script (returns "" so version_status reports UNKNOWN rather than a false UP-TO-DATE).
latest_stable_tag() {
  local url="$1"
  # GIT_TERMINAL_PROMPT=0: never block on a credential prompt for a moved/auth-gated URL —
  # fail fast to "" (-> UNKNOWN) instead of hanging --check.
  GIT_TERMINAL_PROMPT=0 git ls-remote --tags --refs "$url" 2>/dev/null \
    | sed -E 's#.*refs/tags/##' \
    | grep -E '^v?[0-9]+\.[0-9]+\.[0-9]+$' \
    | sed 's/^v//' \
    | sort -V \
    | tail -1 || true
}

usage() {
  cat <<'EOF'
update-av-stack.sh — engine for the /update-av-stack slash command (#44).

Checks each vendored subtree component (vendor/obs-studio, vendor/distroav) against the
latest upstream STABLE release tag and, for anything behind, emits/runs the exact
`git subtree pull --squash` catch-up — re-applying our genlock patches through the subtree
merge and reporting conflicts loudly.

Usage:
  scripts/update-av-stack.sh [--check] [--readme PATH]   # default: report drift (read-only)
  scripts/update-av-stack.sh --apply  [--readme PATH]     # run git subtree pull for behind comps
  scripts/update-av-stack.sh --help

Exit codes: 0 = all up to date, 10 = at least one behind, 11 = upstream unreachable for at
least one component (drift UNKNOWN — never reported as up to date), 1 = error.
EOF
}

print_checklist() {
  cat <<'EOF'

After applying a subtree pull, finish the release bump (per vendor/README.md):
  1. Resolve conflicts patch-by-patch — each `genlock:`-prefixed commit in `git log -- vendor/`
     is one patch; a conflict means upstream touched the same lines our genlock patch did.
     Resolve, `git add`, then commit the merge. Report every conflict loudly — never auto-skip.
  2. Rebuild the vendored stack per vendor/BUILD.md (OBS first, then DistroAV against it).
  3. Run the strict on-device harness (scripts/loopback-e2e.sh) — zero frame loss required (#35).
  4. Update the version table in vendor/README.md to the new tag + squash commit, and commit
     it together with the bump (`vendor:` / `genlock:` prefixed).
EOF
}

main() {
  local mode="check" readme="$DEFAULT_README"
  while [ $# -gt 0 ]; do
    case "$1" in
      --check) mode="check" ;;
      --apply) mode="apply" ;;
      --readme) shift; readme="${1:-}" ;;
      -h|--help) usage; exit 0 ;;
      *) echo "unknown argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  [ -f "$readme" ] || { echo "ERROR: manifest not found: $readme (run from repo root)" >&2; exit 1; }

  local manifest behind=0 unknown=0 prefix url cur latest status
  manifest="$(parse_manifest "$readme")"
  [ -n "$manifest" ] || { echo "ERROR: no subtree components found in $readme" >&2; exit 1; }

  if [ "$mode" = "apply" ]; then
    if [ -n "$(git status --porcelain)" ]; then
      echo "ERROR: working tree is dirty — commit/stash before --apply (subtree pull needs a clean tree)." >&2
      exit 1
    fi
  fi

  echo "== /update-av-stack — vendored AV stack vs upstream =="
  printf '%-22s %-12s %-12s %s\n' "component" "pinned" "upstream" "status"

  # Parallel arrays of the behind components (TSV record per slot) so --check can print the
  # catch-up command and --apply can run it directly (no eval).
  local -a behind_prefix=() behind_url=() behind_tag=()
  while IFS=$'\t' read -r prefix url cur _commit; do
    [ -n "$prefix" ] || continue
    latest="$(latest_stable_tag "$url")"
    status="$(version_status "$cur" "$latest")"
    printf '%-22s %-12s %-12s %s\n' "$prefix" "$cur" "${latest:-?}" "$status"
    case "$status" in
      BEHIND)
        behind=$((behind + 1))
        behind_prefix+=("$prefix"); behind_url+=("$url"); behind_tag+=("$latest")
        ;;
      UNKNOWN) unknown=$((unknown + 1)) ;;
    esac
  done <<< "$manifest"

  # UNKNOWN = upstream lookup failed. NEVER let it masquerade as up-to-date (the whole point
  # of the UNKNOWN signal). Warn loudly; it forces a non-zero exit below.
  if [ "$unknown" -gt 0 ]; then
    echo
    echo "!! Could not reach upstream for $unknown component(s) — drift UNKNOWN, NOT up to date." >&2
    echo "!! Re-run when the network/remote is reachable before trusting this report." >&2
  fi

  if [ "$behind" -eq 0 ]; then
    if [ "$unknown" -eq 0 ]; then
      echo
      echo "All vendored components are up to date with upstream stable releases."
      exit 0
    fi
    exit 11
  fi

  echo
  echo "$behind component(s) BEHIND upstream. Catch-up command(s):"
  local i
  for i in "${!behind_prefix[@]}"; do
    echo "  $(subtree_pull_cmd "${behind_prefix[$i]}" "${behind_url[$i]}" "${behind_tag[$i]}")"
  done

  if [ "$mode" = "check" ]; then
    print_checklist
    [ "$unknown" -gt 0 ] && exit 11
    exit 10
  fi

  # --apply: run each catch-up pull as an argv array (no eval), stopping loudly on conflict.
  # -m makes the squash-merge non-interactive (no $EDITOR) and the commit message deterministic.
  for i in "${!behind_prefix[@]}"; do
    prefix="${behind_prefix[$i]}"; url="${behind_url[$i]}"; latest="${behind_tag[$i]}"
    echo
    echo ">> git subtree pull --prefix=$prefix $url $latest --squash"
    if ! git subtree pull --prefix="$prefix" "$url" "$latest" --squash \
           -m "vendor: update $prefix to $latest"; then
      echo "!! subtree pull FAILED for $prefix -> $latest" >&2
      echo "!! Likely a conflict between an upstream change and a genlock patch. Conflicting files:" >&2
      git diff --name-only --diff-filter=U >&2 || true
      echo "!! Resolve patch-by-patch, \`git add\`, commit the merge, then re-run --apply." >&2
      exit 1
    fi
    if [ -n "$(git diff --name-only --diff-filter=U)" ]; then
      echo "!! Unresolved conflict markers after $prefix -> $latest" >&2
      git diff --name-only --diff-filter=U >&2
      exit 1
    fi
  done
  print_checklist
  [ "$unknown" -gt 0 ] && exit 11
  exit 0
}

main "$@"
