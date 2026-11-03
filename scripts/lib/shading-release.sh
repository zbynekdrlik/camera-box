#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/shading-release.sh -- issue 1487: camera-box pins a RELEASE of bkshading (repo
# zbynekdrlik/shading, its docs/CONTRACT.md) and deploys it; the shading project owns the code, its
# tests and its releases. ONE pin, scripts/shading-release.pin: the tag + the sha256 of each asset.
#
# Every asset is fetched by the EXACT tag into <cache>/<tag>/ (default cache <tree>/shading-release,
# git-ignored), proved against the pin AND the release's own <asset>.sha256; an asset already there
# that matches the pin needs no network. The relay (amd64) and the service must also print the tag's
# version. No fallback to any other build, ever: a failure is rc 1 + a named line on stderr.
#
# A file handed in by hand (`--binary`, `--relay-binary`) goes through shading_release_file_ok: only
# the pinned bytes deploy. Another relay version on the boxes means another pin.
#
# Seams: SHADING_RELEASE_URL = the download base (tests: file://); SHADING_RELEASE_PIN_FILE = another
# pin; SHADING_RELEASE_CACHE = another cache dir.

SHADING_RELEASE_REPO_URL="https://github.com/zbynekdrlik/shading/releases/download"
SHADING_ASSET_RELAY_AMD64="bkshading-relay-linux-amd64"
SHADING_ASSET_RELAY_ARM64="bkshading-relay-linux-arm64"
SHADING_ASSET_SERVICE_LINUX="bkshading-service-linux-amd64.tar.gz"
SHADING_ASSET_SERVICE_WINDOWS="bkshading-service-windows-amd64.exe"

# _shading_pin_field ROOT KEY -> the value of KEY in ROOT's pin file (nothing when unreadable).
_shading_pin_field() {
  sed -n "s/^${2}=//p" "${SHADING_RELEASE_PIN_FILE:-${1}/scripts/shading-release.pin}" 2>/dev/null | head -n 1
}

# shading_pin_tag ROOT -> the pinned tag; nothing + rc 1 when unreadable.
shading_pin_tag() {
  local v
  v="$(_shading_pin_field "${1:?tree required}" SHADING_RELEASE_TAG)"
  [ -n "$v" ] || { echo "shading-release: no SHADING_RELEASE_TAG in ${1}/scripts/shading-release.pin" >&2; return 1; }
  printf '%s\n' "$v"
}

# shading_pin_version ROOT -> the pinned tag without its v, what --version prints.
shading_pin_version() {
  local tag
  tag="$(shading_pin_tag "${1:?tree required}")" || return 1
  printf '%s\n' "${tag#v}"
}

# shading_pin_version_of_text PIN_TEXT -> the version in a pin file's TEXT (`git show` of another
# ref's pin), or nothing.
shading_pin_version_of_text() {
  local line
  while IFS= read -r line; do
    case "$line" in
      SHADING_RELEASE_TAG=v*) printf '%s\n' "${line#SHADING_RELEASE_TAG=v}"; return 0 ;;
    esac
  done <<<"${1:-}"
  return 0
}

# _shading_asset_key ASSET -> the pin file key of ASSET; nothing + rc 1 for an unknown asset.
_shading_asset_key() {
  case "${1:-}" in
    "$SHADING_ASSET_RELAY_AMD64") printf '%s\n' SHADING_RELAY_LINUX_AMD64_SHA256 ;;
    "$SHADING_ASSET_RELAY_ARM64") printf '%s\n' SHADING_RELAY_LINUX_ARM64_SHA256 ;;
    "$SHADING_ASSET_SERVICE_LINUX") printf '%s\n' SHADING_SERVICE_LINUX_AMD64_SHA256 ;;
    "$SHADING_ASSET_SERVICE_WINDOWS") printf '%s\n' SHADING_SERVICE_WINDOWS_AMD64_SHA256 ;;
    *) echo "shading-release: unknown asset '${1:-}'" >&2; return 1 ;;
  esac
}

# shading_pin_sha256 ROOT ASSET -> the pinned sha256 of ASSET; nothing + rc 1 when unreadable.
shading_pin_sha256() {
  local key v
  key="$(_shading_asset_key "${2:-}")" || return 1
  v="$(_shading_pin_field "${1:?tree required}" "$key")"
  [ "${#v}" = 64 ] || { echo "shading-release: no ${key} in ${1}/scripts/shading-release.pin" >&2; return 1; }
  printf '%s\n' "$v"
}

# shading_relay_asset_for_arch ARCH -> the relay asset of amd64 | arm64; nothing + rc 1 otherwise.
shading_relay_asset_for_arch() {
  case "${1:-amd64}" in
    amd64) printf '%s\n' "$SHADING_ASSET_RELAY_AMD64" ;;
    arm64) printf '%s\n' "$SHADING_ASSET_RELAY_ARM64" ;;
    *) echo "shading-release: no relay asset for arch '${1:-}'" >&2; return 1 ;;
  esac
}

# _shading_sha FILE -> its sha256 ('' when unreadable).
_shading_sha() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }

# shading_release_file_ok ROOT ASSET FILE -> rc 0 when FILE carries the pinned bytes of ASSET; rc 1 + a
# named line otherwise. Every hand-supplied binary goes through this.
shading_release_file_ok() {
  local want got
  want="$(shading_pin_sha256 "${1:?tree required}" "${2:-}")" || return 1
  got="$(_shading_sha "${3:?file required}")"
  [ "$got" = "$want" ] && return 0
  echo "shading-release: ${3} is not the pinned ${2} (sha256 ${got:-unreadable}, the pin says ${want}, $(shading_pin_tag "$1" 2>/dev/null)): only the pinned release deploys" >&2
  return 1
}

# _shading_release_dir ROOT -> <cache>/<tag>.
_shading_release_dir() {
  local tag
  tag="$(shading_pin_tag "$1")" || return 1
  printf '%s/%s\n' "${SHADING_RELEASE_CACHE:-${1%/}/shading-release}" "$tag"
}

# shading_release_fetch ROOT ASSET -> prints the path of ASSET, proved against the pin and the
# release's .sha256 (rc 0); rc 1 + a named line otherwise, and nothing unproved stays at that path.
shading_release_fetch() {
  local root="${1:?tree required}" asset="${2:?asset required}" tag want dir file url part said got
  tag="$(shading_pin_tag "$root")" || return 1
  want="$(shading_pin_sha256 "$root" "$asset")" || return 1
  dir="$(_shading_release_dir "$root")" || return 1
  file="${dir}/${asset}"
  if [ "$(_shading_sha "$file")" = "$want" ]; then
    printf '%s\n' "$file"
    return 0
  fi
  mkdir -p "$dir" || { echo "shading-release: cannot create ${dir}" >&2; return 1; }
  part="$(mktemp "${file}.XXXXXX")" || { echo "shading-release: no temp file in ${dir}" >&2; return 1; }
  url="${SHADING_RELEASE_URL:-$SHADING_RELEASE_REPO_URL}/${tag}/${asset}"
  if ! curl -fsSL --max-time 180 -o "$part" "$url" || ! said="$(curl -fsSL --max-time 30 "${url}.sha256")"; then
    rm -f "$part"
    echo "shading-release: cannot fetch ${url} (+ .sha256): never another build" >&2
    return 1
  fi
  said="${said%% *}"
  got="$(_shading_sha "$part")"
  if [ "$said" != "$want" ] || [ "$got" != "$want" ]; then
    rm -f "$part"
    echo "shading-release: ${tag} ${asset} sha256 ${got:-unreadable}, its .sha256 says ${said:-nothing}, the pin says ${want}" >&2
    return 1
  fi
  mv -f "$part" "$file" || { rm -f "$part"; echo "shading-release: cannot keep ${file}" >&2; return 1; }
  printf '%s\n' "$file"
}

# _shading_version_ok BIN NAME TAG -> rc 0 when BIN prints exactly `NAME <tag without v>`.
_shading_version_ok() {
  local have want="${2} ${3#v}"
  have="$(timeout 10 "$1" --version 2>/dev/null | head -n 1 || true)"
  [ "$have" = "$want" ] && return 0
  echo "shading-release: the ${3} binary says '${have:-nothing}', not '${want}'" >&2
  return 1
}

# shading_release_relay ROOT ARCH -> prints the path of the verified relay binary of ARCH. amd64 is
# also run for its --version; dev1 cannot run arm64, there the two sha256 proofs stand.
shading_release_relay() {
  local root="${1:?tree required}" arch="${2:-amd64}" asset bin tag
  asset="$(shading_relay_asset_for_arch "$arch")" || return 1
  bin="$(shading_release_fetch "$root" "$asset")" || return 1
  chmod +x "$bin" || { echo "shading-release: cannot chmod ${bin}" >&2; return 1; }
  if [ "$arch" = amd64 ]; then
    tag="$(shading_pin_tag "$root")" || return 1
    _shading_version_ok "$bin" bkshading-relay "$tag" || return 1
  fi
  printf '%s\n' "$bin"
}

# shading_release_service ROOT -> prints the directory holding the verified Linux service (`bkshading`
# + `web/`, extracted from the proved tarball, the binary's --version being the tag).
shading_release_service() {
  local root="${1:?tree required}" tb tag dir x
  tb="$(shading_release_fetch "$root" "$SHADING_ASSET_SERVICE_LINUX")" || return 1
  tag="$(shading_pin_tag "$root")" || return 1
  dir="$(dirname "$tb")"
  x="$(mktemp -d "${dir}/.extract.XXXXXX")" || { echo "shading-release: no temp dir in ${dir}" >&2; return 1; }
  if ! tar -xzf "$tb" -C "$x" --no-same-owner --no-same-permissions --no-overwrite-dir 2>/dev/null; then
    rm -rf "$x"; echo "shading-release: cannot extract ${tb}" >&2; return 1
  fi
  if [ ! -f "${x}/bkshading-service/bkshading" ] || [ ! -d "${x}/bkshading-service/web" ]; then
    rm -rf "$x"; echo "shading-release: ${tag} has no bkshading-service/bkshading + web/" >&2; return 1
  fi
  chmod 0755 "${x}/bkshading-service/bkshading"
  if ! _shading_version_ok "${x}/bkshading-service/bkshading" bkshading "$tag"; then
    rm -rf "$x"; return 1
  fi
  rm -rf "${dir}/bkshading-service"
  mv -f "${x}/bkshading-service" "${dir}/bkshading-service" \
    || { rm -rf "$x"; echo "shading-release: cannot place the service in ${dir}" >&2; return 1; }
  rm -rf "$x"
  # setup-strih runs this as root inside the operator's staged tree: the next deploy's rsync and sweep
  # (the operator) must be able to replace it, so it takes the owner of the directory it lands in.
  chown -R --reference="$dir" "${dir}/bkshading-service" \
    || { echo "shading-release: cannot give ${dir}/bkshading-service the owner of ${dir}" >&2; return 1; }
  printf '%s\n' "${dir}/bkshading-service"
}

# strih_bkshading_service_install DIR BIN -> setup-strih step 16c: DIR's binary at BIN (a NEW inode, a
# running service keeps its old one; restart it to load a new pin) + DIR's web/ beside it, + BIN.version.
strih_bkshading_service_install() {
  local dir="${1:?service dir required}" bin="${2:?binary path required}" dest
  dest="$(dirname "$bin")"
  install -d -m 0755 -o root -g root "$dest" || return 1
  install -m 0755 -o root -g root "${dir}/bkshading" "${bin}.new" && mv -f "${bin}.new" "$bin" \
    || { rm -f "${bin}.new"; echo "shading-release: could not install ${bin}" >&2; return 1; }
  rm -rf "${dest}/web"
  cp -a "${dir}/web" "${dest}/web" || { echo "shading-release: could not copy web/ to ${dest}" >&2; return 1; }
  chown -R root:root "${dest}/web" || return 1
  find "${dest}/web" -type d -exec chmod 0755 {} + || return 1
  find "${dest}/web" -type f -exec chmod 0644 {} + || return 1
}

# strih_bkshading_version_grade BASE_URL -> verify item 31's version row through the caller's ok / bad:
# the running service (BASE_URL/api/version), else the installed binary, against the pin. A difference
# is a FAIL with the reinstall line (step 16c restarts a running service, so a difference is a real
# defect, unlike an installed-only unit). Always rc 0. STRIH_BKSHADING_VERSION_JSON_FILE reads the JSON from a file, STRIH_BKSHADING_BIN names the binary.
strih_bkshading_version_grade() {
  local base="${1:?service base url required}" bin="${STRIH_BKSHADING_BIN:-/opt/bkshading/bkshading}"
  local root want json have
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  want="$(shading_pin_version "$root" 2>/dev/null || true)"
  if [ -n "${STRIH_BKSHADING_VERSION_JSON_FILE:-}" ]; then
    json="$(cat -- "$STRIH_BKSHADING_VERSION_JSON_FILE" 2>/dev/null || true)"
  else
    json="$(curl -fsS --max-time 3 "${base}/api/version" 2>/dev/null || true)"
  fi
  have="$(printf '%s' "$json" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("version", ""))
except ValueError:
    pass' 2>/dev/null || true)"
  [ -n "$have" ] || have="$(timeout 10 "$bin" --version 2>/dev/null | sed -n 's/^bkshading \([^ ]*\)$/\1/p' | head -n 1 || true)"
  if [ -n "$want" ] && [ "$have" = "$want" ]; then
    ok "(bkshading-service) version ${have}, the shading pin"
  else
    bad "(bkshading-service) version '${have:-unreadable}', the shading pin says '${want:-unreadable}' -- re-run setup-strih.sh step 16c"
  fi
}
