#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/obs-release.sh -- issue 1487: the Linux OBS boxes (every obs-fleet linux-genlock box with
# a fact file: strih-snv, stream-snv, strih-pp, stream-pp) run a RELEASE of the forked OBS (repo
# zbynekdrlik/obs, its release workflow + docs/HANDOVER.md), never this repo's vendor/ build. ONE pin,
# scripts/obs-release.pin: the tag, the commit, the sha256 of the strih tarball and of its
# BUNDLE_MANIFEST.json (the release publishes no .sha256 for the manifest, so the pin carries it).
#
# The tarball `obs-strih-linux-x86_64-<tag>.tar.zst` is the strih stage dir with its unix modes
# (`zstd -dc ... | tar -C <dest> -xf -`): bin/obs, the /usr-prefix lib + share trees, RUNTIME_PACKAGES.txt,
# STRIH_BUILD_FLAGS.txt, GENLOCK_BUILD_SHA.txt (= the obs commit), OBS_RELEASE.txt (tag= commit=
# version=), lib/x86_64-linux-gnu/obs-plugins/dante-input.so. obs_release_bundle proves it four ways:
# the tarball's sha256 = the pin AND the release's own .sha256; the manifest's sha256 = the pin; every
# file the manifest lists has its manifest sha256 in the extracted tree; OBS_RELEASE.txt, the manifest's
# release block and GENLOCK_BUILD_SHA.txt all name the pinned tag + commit. No fallback, ever: a
# failure is rc 1 + a named line on stderr and nothing half-extracted is left.
#
# Seams: OBS_RELEASE_URL = the download base (tests: file://); OBS_RELEASE_PIN_FILE = another pin;
# OBS_RELEASE_CACHE = another cache dir (default <tree>/obs-release, git-ignored).

OBS_RELEASE_REPO_URL="https://github.com/zbynekdrlik/obs/releases/download"
OBS_RELEASE_MANIFEST="BUNDLE_MANIFEST.json"
# The files a strih bundle must carry beyond the manifest's own list (the setup-strih step 4 contract).
OBS_RELEASE_REQUIRED=(bin/obs lib/x86_64-linux-gnu/libobs.so.30 RUNTIME_PACKAGES.txt STRIH_BUILD_FLAGS.txt
  GENLOCK_BUILD_SHA.txt OBS_RELEASE.txt lib/x86_64-linux-gnu/obs-plugins/dante-input.so)

# _obs_pin_field ROOT KEY -> the value of KEY in ROOT's pin file (nothing when unreadable).
_obs_pin_field() {
  sed -n "s/^${2}=//p" "${OBS_RELEASE_PIN_FILE:-${1}/scripts/obs-release.pin}" 2>/dev/null | head -n 1
}

# obs_pin_tag ROOT / obs_pin_commit ROOT / obs_pin_sha256 ROOT / obs_pin_manifest_sha256 ROOT -> the pin;
# nothing + rc 1 (a named line) when the field is missing or malformed.
obs_pin_tag() {
  local v
  v="$(_obs_pin_field "${1:?tree required}" OBS_RELEASE_TAG)"
  [[ "$v" =~ ^v[0123456789]+\.[0123456789]+\.[0123456789]+$ ]] \
    || { echo "obs-release: no vX.Y.Z OBS_RELEASE_TAG in ${1}/scripts/obs-release.pin" >&2; return 1; }
  printf '%s\n' "$v"
}
_obs_pin_hex() {
  local v
  v="$(_obs_pin_field "$1" "$2")"
  [ "${#v}" = "$3" ] && [[ "$v" =~ ^[0123456789abcdef]+$ ]] \
    || { echo "obs-release: no ${2} (${3} hex) in ${1}/scripts/obs-release.pin" >&2; return 1; }
  printf '%s\n' "$v"
}
# the commit: a full 40-hex sha in the real pin; 7..40 hex is read (a test's short sha)
obs_pin_commit() {
  local v
  v="$(_obs_pin_field "${1:?tree required}" OBS_RELEASE_COMMIT)"
  [ "${#v}" -ge 7 ] && [ "${#v}" -le 40 ] && [[ "$v" =~ ^[0123456789abcdef]+$ ]] \
    || { echo "obs-release: no OBS_RELEASE_COMMIT (7..40 hex) in ${1}/scripts/obs-release.pin" >&2; return 1; }
  printf '%s\n' "$v"
}
obs_pin_sha256() { _obs_pin_hex "${1:?tree required}" OBS_RELEASE_STRIH_SHA256 64; }
obs_pin_manifest_sha256() { _obs_pin_hex "${1:?tree required}" OBS_RELEASE_MANIFEST_SHA256 64; }

# obs_release_tarball_name TAG -> the strih tarball's asset name.
obs_release_tarball_name() { printf 'obs-strih-linux-x86_64-%s.tar.zst\n' "${1:?tag required}"; }

# obs_pin_tag_of_text PIN_TEXT -> the tag in a pin file's TEXT (`git show` of another ref's pin), or nothing.
obs_pin_tag_of_text() {
  local line
  while IFS= read -r line; do
    case "$line" in OBS_RELEASE_TAG=v*) printf '%s\n' "${line#OBS_RELEASE_TAG=}"; return 0 ;; esac
  done <<<"${1:-}"
  return 0
}

_obs_release_sha() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }

# _obs_release_get ROOT ASSET WANT_SHA SIDE -> prints the cached path of ASSET, proved against WANT_SHA
# (and, SIDE=1, the release's own <asset>.sha256). An asset already cached with the pinned bytes needs no
# network. rc 1 + a named line otherwise; nothing unproved stays at that path.
_obs_release_get() {
  local root="$1" asset="$2" want="$3" side="$4" tag dir file part url said got
  tag="$(obs_pin_tag "$root")" || return 1
  dir="${OBS_RELEASE_CACHE:-${root%/}/obs-release}/${tag}"
  file="${dir}/${asset}"
  if [ "$(_obs_release_sha "$file")" = "$want" ]; then printf '%s\n' "$file"; return 0; fi
  mkdir -p "$dir" || { echo "obs-release: cannot create ${dir}" >&2; return 1; }
  part="$(mktemp "${file}.XXXXXX")" || { echo "obs-release: no temp file in ${dir}" >&2; return 1; }
  url="${OBS_RELEASE_URL:-$OBS_RELEASE_REPO_URL}/${tag}/${asset}"
  if ! curl -fsSL --max-time 1800 -o "$part" "$url"; then
    rm -f "$part"; echo "obs-release: cannot fetch ${url}: never another build" >&2; return 1
  fi
  said="$want"
  if [ "$side" = 1 ]; then
    said="$(curl -fsSL --max-time 30 "${url}.sha256" 2>/dev/null || true)"; said="${said%% *}"
  fi
  got="$(_obs_release_sha "$part")"
  if [ "$got" != "$want" ] || [ "$said" != "$want" ]; then
    rm -f "$part"
    echo "obs-release: ${tag} ${asset} sha256 ${got:-unreadable}, its .sha256 says ${said:-nothing}, the pin says ${want}" >&2
    return 1
  fi
  mv -f "$part" "$file" || { rm -f "$part"; echo "obs-release: cannot keep ${file}" >&2; return 1; }
  printf '%s\n' "$file"
}

# obs_release_verify_tree DIR MANIFEST TAG COMMIT -> rc 0 when DIR is the release the manifest describes:
# every listed file's sha256, the required files, OBS_RELEASE.txt + GENLOCK_BUILD_SHA.txt + the manifest's
# release block naming TAG and COMMIT. rc 1 + every problem on stderr otherwise. Pure over the files.
obs_release_verify_tree() {
  local dir="${1:?dir required}" man="${2:?manifest required}" tag="${3:?tag required}" commit="${4:?commit required}" f bad=0
  for f in "${OBS_RELEASE_REQUIRED[@]}"; do
    [ -f "${dir}/${f}" ] || { echo "obs-release: ${tag} has no ${f}" >&2; bad=1; }
  done
  [ "$(tr -d '[:space:]' < "${dir}/GENLOCK_BUILD_SHA.txt" 2>/dev/null)" = "$commit" ] \
    || { echo "obs-release: GENLOCK_BUILD_SHA.txt reads $(tr -d '[:space:]' < "${dir}/GENLOCK_BUILD_SHA.txt" 2>/dev/null || echo '<none>'), not the pinned commit ${commit}" >&2; bad=1; }
  grep -qx "tag=${tag}" "${dir}/OBS_RELEASE.txt" 2>/dev/null && grep -qx "commit=${commit}" "${dir}/OBS_RELEASE.txt" 2>/dev/null \
    || { echo "obs-release: OBS_RELEASE.txt does not name tag=${tag} commit=${commit}" >&2; bad=1; }
  python3 -I - "$dir" "$man" "$tag" "$commit" <<'PY' || bad=1
import hashlib, json, os, sys
root, man, tag, commit = sys.argv[1:5]
try:
    d = json.load(open(man, encoding="utf-8"))
except (OSError, ValueError) as e:
    print("obs-release: the manifest is not readable JSON: %s" % e, file=sys.stderr); sys.exit(1)
rel = d.get("release") if isinstance(d, dict) else None
problems = []
if not isinstance(rel, dict) or rel.get("obs_tag") != tag or rel.get("obs_commit") != commit:
    problems.append("the manifest's release block does not name %s %s" % (tag, commit))
files = d.get("files") if isinstance(d, dict) else None
if not isinstance(files, list) or not files:
    problems.append("the manifest lists no files")
    files = []
for f in files:
    p = f.get("path") if isinstance(f, dict) else None
    if not isinstance(p, str) or p.startswith("/") or ".." in p.split("/"):
        problems.append("a manifest entry has no safe path: %r" % (p,)); continue
    full = os.path.join(root, p)
    try:
        h = hashlib.sha256()
        with open(full, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
    except OSError:
        problems.append("%s is missing" % p); continue
    if h.hexdigest() != f.get("sha256"):
        problems.append("%s sha256 %s != the manifest's %s" % (p, h.hexdigest(), f.get("sha256")))
for p in problems[:20]:
    print("obs-release: " + p, file=sys.stderr)
if len(problems) > 20:
    print("obs-release: ... %d more" % (len(problems) - 20), file=sys.stderr)
sys.exit(1 if problems else 0)
PY
  [ "$bad" = 0 ]
}

# obs_release_bundle ROOT DEST -> extract the pinned, proved strih bundle into DEST (absent or empty) with
# BUNDLE_MANIFEST.json beside it, the tree checked file by file. rc 0, or rc 1 + named lines and DEST
# removed (a half bundle never stays where a deploy would stage it).
obs_release_bundle() {
  local root="${1:?tree required}" dest="${2:?destination required}" tag commit tb man
  tag="$(obs_pin_tag "$root")" || return 1
  commit="$(obs_pin_commit "$root")" || return 1
  if [ -e "$dest" ] && [ -n "$(ls -A "$dest" 2>/dev/null)" ]; then
    echo "obs-release: ${dest} is not empty -- refusing to extract over it" >&2; return 1
  fi
  command -v zstd >/dev/null 2>&1 || { echo "obs-release: zstd is required to unpack the ${tag} bundle" >&2; return 1; }
  tb="$(_obs_release_get "$root" "$(obs_release_tarball_name "$tag")" "$(obs_pin_sha256 "$root")" 1)" || return 1
  man="$(_obs_release_get "$root" "$OBS_RELEASE_MANIFEST" "$(obs_pin_manifest_sha256 "$root")" 0)" || return 1
  mkdir -p "$dest" || { echo "obs-release: cannot create ${dest}" >&2; return 1; }
  if ! { zstd -dc -- "$tb" | tar -C "$dest" -xf - --no-same-owner; }; then
    rm -rf "$dest"; echo "obs-release: cannot unpack ${tb}" >&2; return 1
  fi
  # The tarball carries its own manifest: it must be the pinned bytes (never a second, unproved list).
  if [ -e "${dest}/${OBS_RELEASE_MANIFEST}" ]; then
    cmp -s -- "$man" "${dest}/${OBS_RELEASE_MANIFEST}" \
      || { rm -rf "$dest"; echo "obs-release: the tarball's ${OBS_RELEASE_MANIFEST} is not the pinned manifest" >&2; return 1; }
  else
    cp -- "$man" "${dest}/${OBS_RELEASE_MANIFEST}" || { rm -rf "$dest"; echo "obs-release: cannot copy the manifest" >&2; return 1; }
  fi
  if ! obs_release_verify_tree "$dest" "${dest}/${OBS_RELEASE_MANIFEST}" "$tag" "$commit"; then
    rm -rf "$dest"; return 1
  fi
  echo "obs-release: ${tag} (${commit}) unpacked into ${dest}, every manifest file verified" >&2
}

# obs_release_manifest_field DIR KEY -> the manifest's release.KEY (genlock_stats_version, ...), or nothing.
obs_release_manifest_field() {
  python3 -I -c 'import json,sys
try:
    v = json.load(open(sys.argv[1], encoding="utf-8"))["release"][sys.argv[2]]
except (OSError, ValueError, KeyError, TypeError):
    sys.exit(1)
print(v)' "${1:?dir required}/${OBS_RELEASE_MANIFEST}" "${2:?key required}" 2>/dev/null
}

# obs_release_stats_abi DIR -> the frontend's genlock stats ABI pair `<stats>:<output_stats>` the manifest
# records (the pair genlock_stats_abi_stage writes into GENLOCK_STATS_ABI.txt); nothing + rc 1 when either
# is unreadable (the marker is then removed by the stage, never guessed).
obs_release_stats_abi() {
  local s o
  s="$(obs_release_manifest_field "$1" genlock_stats_version)" || return 1
  o="$(obs_release_manifest_field "$1" genlock_output_stats_version)" || return 1
  [[ "$s" =~ ^[123456789][0123456789]{0,8}$ ]] && [[ "$o" =~ ^[123456789][0123456789]{0,8}$ ]] || return 1
  printf '%s:%s\n' "$s" "$o"
}

# obs_release_marker_grade INSTALL_DIR ROOT -> `ok <tag> <commit>` (rc 0) when INSTALL_DIR's markers
# (GENLOCK_BUILD_SHA.txt + OBS_RELEASE.txt) name the pin, else `drift <why>` (rc 1). Read-only; verify-strih
# and the deploy read-back both grade a Linux box against the pin with it.
obs_release_marker_grade() {
  local dir="${1:?install dir required}" root="${2:?tree required}" tag commit have
  tag="$(obs_pin_tag "$root" 2>/dev/null)" || { echo "drift the pin is unreadable"; return 1; }
  commit="$(obs_pin_commit "$root" 2>/dev/null)" || { echo "drift the pin is unreadable"; return 1; }
  have="$(tr -d '[:space:]' < "${dir}/GENLOCK_BUILD_SHA.txt" 2>/dev/null || true)"
  [ "$have" = "$commit" ] || { echo "drift GENLOCK_BUILD_SHA.txt=${have:-<unreadable>}, the pin says ${commit} (${tag})"; return 1; }
  grep -qx "tag=${tag}" "${dir}/OBS_RELEASE.txt" 2>/dev/null \
    || { echo "drift OBS_RELEASE.txt does not say tag=${tag}"; return 1; }
  echo "ok ${tag} ${commit}"
}
