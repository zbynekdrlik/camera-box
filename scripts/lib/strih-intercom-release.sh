#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-intercom-release.sh -- issue 1487: camera-box pins a RELEASE of the intercom
# (repo zbynekdrlik/intercom) and installs it; the intercom project owns the hub, its unit and its
# routing files. ONE pin, scripts/intercom-release.pin: the tag + the sha256 of the release tarball.
# Root Cargo.toml's `intercom-vban` git tag must equal it (tests/intercom_pin_1487.rs).
#
# The bundle `intercom-hub-linux-amd64.tar.gz` carries the binary, the unit and the four routing TOMLs.
# `--check-config` makes TOML and binary a pair, so everything step 13 installs comes from one verified
# directory: <tree>/intercom-release/<tag>/. The strih deploy fills it in its staged tree on dev1 (the box
# then needs no network for it); a setup-strih run from a checkout fills it there (git-ignored).
#
# Fetched by the EXACT tag, verified against the pin AND the release's own .sha256, the binary's
# --version against the tag. Never a fallback to another build: a stale hub is issue 1467's silent FOH.
# Seams: STRIH_INTERCOM_RELEASE_URL = the base URL (tests: file://); STRIH_INTERCOM_PIN_FILE = another pin;
# STRIH_INTERCOM_TARBALL_SRC = a local tarball instead of the download, still proved against the pin.

STRIH_INTERCOM_REPO_URL="https://github.com/zbynekdrlik/intercom/releases/download"
STRIH_INTERCOM_TARBALL="intercom-hub-linux-amd64.tar.gz"
STRIH_INTERCOM_FILES=(intercom-hub intercom-hub.service intercom.strih-snv.toml intercom.strih-pp.toml
  intercom.stream-snv.toml intercom.stream-pp.toml)

# _strih_intercom_pin_field ROOT KEY -> the value of KEY in ROOT's scripts/intercom-release.pin.
_strih_intercom_pin_field() {
  sed -n "s/^${2}=//p" "${STRIH_INTERCOM_PIN_FILE:-${1}/scripts/intercom-release.pin}" 2>/dev/null | head -n 1
}

# strih_intercom_pin_tag ROOT / strih_intercom_pin_sha256 ROOT -> the pin; nothing + rc 1 when unreadable.
strih_intercom_pin_tag() {
  local v
  v="$(_strih_intercom_pin_field "${1:?tree required}" INTERCOM_RELEASE_TAG)"
  [ -n "$v" ] || { echo "strih-intercom-release: no INTERCOM_RELEASE_TAG in ${1}/scripts/intercom-release.pin" >&2; return 1; }
  printf '%s\n' "$v"
}
strih_intercom_pin_sha256() {
  local v
  v="$(_strih_intercom_pin_field "${1:?tree required}" INTERCOM_RELEASE_SHA256)"
  [ "${#v}" = 64 ] || { echo "strih-intercom-release: no INTERCOM_RELEASE_SHA256 in ${1}/scripts/intercom-release.pin" >&2; return 1; }
  printf '%s\n' "$v"
}

# strih_intercom_release_dir ROOT -> <ROOT>/intercom-release/<tag>, the bundle's directory (reads only).
strih_intercom_release_dir() {
  local tag
  tag="$(strih_intercom_pin_tag "$1")" || return 1
  printf '%s/intercom-release/%s\n' "${1%/}" "$tag"
}

# _strih_intercom_sha FILE -> its sha256 ('' when unreadable).
_strih_intercom_sha() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }

# _strih_intercom_download TAG SHA DIR -> the tarball into DIR, proved against the pin and the release's
# .sha256. rc 1 + a named line otherwise; nothing but a proved tarball stays.
_strih_intercom_download() {
  local tag="$1" sha="$2" dir="$3" url tb said got
  url="${STRIH_INTERCOM_RELEASE_URL:-$STRIH_INTERCOM_REPO_URL}/${tag}/${STRIH_INTERCOM_TARBALL}"
  tb="${dir}/${STRIH_INTERCOM_TARBALL}"
  if [ -n "${STRIH_INTERCOM_TARBALL_SRC:-}" ]; then
    cp "$STRIH_INTERCOM_TARBALL_SRC" "${tb}.part" || { echo "strih-intercom-release: cannot read ${STRIH_INTERCOM_TARBALL_SRC}" >&2; return 1; }
    printf '%s\n' "$sha" > "${tb}.sha256.part"
  elif ! curl -fsSL --max-time 180 -o "${tb}.part" "$url" || ! curl -fsSL --max-time 30 -o "${tb}.sha256.part" "${url}.sha256"; then
    rm -f "${tb}.part" "${tb}.sha256.part"
    echo "strih-intercom-release: cannot fetch ${url} (+ .sha256): never another build" >&2; return 1
  fi
  said="$(cut -d' ' -f1 "${tb}.sha256.part" 2>/dev/null || true)"
  got="$(_strih_intercom_sha "${tb}.part")"
  rm -f "${tb}.sha256.part"
  if [ "$said" != "$sha" ] || [ "$got" != "$sha" ]; then
    rm -f "${tb}.part"
    echo "strih-intercom-release: ${tag} tarball sha256 ${got:-unreadable}, its .sha256 says ${said:-nothing}, the pin says ${sha}" >&2
    return 1
  fi
  mv -f "${tb}.part" "$tb" || { echo "strih-intercom-release: cannot keep ${tb}" >&2; return 1; }
}

# strih_intercom_release_ensure ROOT -> prints the bundle directory (rc 0) once its tarball matches the
# pin and its files are extracted from THAT tarball, the binary's --version being the tag; a tarball that
# is missing or does not match is fetched. rc 1 + a named line on stderr otherwise, nothing half-installed.
strih_intercom_release_ensure() {
  local root="${1:?tree required}" tag sha dir tb x f name have
  tag="$(strih_intercom_pin_tag "$root")" || return 1
  sha="$(strih_intercom_pin_sha256 "$root")" || return 1
  dir="${root%/}/intercom-release/${tag}"
  tb="${dir}/${STRIH_INTERCOM_TARBALL}"
  mkdir -p "$dir" || { echo "strih-intercom-release: cannot create ${dir}" >&2; return 1; }
  if [ "$(_strih_intercom_sha "$tb")" != "$sha" ]; then
    rm -f "$tb"
    _strih_intercom_download "$tag" "$sha" "$dir" || return 1
  fi
  x="$(mktemp -d "${dir}/.extract.XXXXXX")" || { echo "strih-intercom-release: no temp dir in ${dir}" >&2; return 1; }
  if ! tar -xzf "$tb" -C "$x" --no-same-owner --no-same-permissions --no-overwrite-dir 2>/dev/null; then
    rm -rf "$x"; echo "strih-intercom-release: cannot extract ${tb}" >&2; return 1
  fi
  for f in "${STRIH_INTERCOM_FILES[@]}"; do
    if [ ! -f "${x}/${f}" ]; then rm -rf "$x"; echo "strih-intercom-release: ${tag} has no ${f}" >&2; return 1; fi
  done
  chmod 0755 "${x}/intercom-hub"
  name="intercom-hub ${tag#v}"
  have="$(timeout 10 "${x}/intercom-hub" --version 2>/dev/null | head -n 1 || true)"
  if [ "$have" != "$name" ]; then
    rm -rf "$x"; echo "strih-intercom-release: the ${tag} binary says '${have:-nothing}', not '${name}'" >&2; return 1
  fi
  for f in "${STRIH_INTERCOM_FILES[@]}"; do
    mv -f "${x}/${f}" "${dir}/${f}" || { rm -rf "$x"; echo "strih-intercom-release: cannot place ${f} in ${dir}" >&2; return 1; }
  done
  rm -rf "$x"
  # Run as root by setup-strih inside the operator's staged tree: the files take the directory's owner,
  # so the next deploy's rsync (the operator) can replace them (3.11.2026, rsync rc 23).
  for f in "${STRIH_INTERCOM_FILES[@]}"; do
    chown --reference="$dir" "${dir}/${f}" \
      || { echo "strih-intercom-release: cannot give ${dir}/${f} the owner of ${dir}" >&2; return 1; }
  done
  printf '%s\n' "$dir"
}

# _strih_hub_version BIN -> the version BIN reports ('' when it does not run or says something else).
_strih_hub_version() {
  timeout 10 "$1" --version 2>/dev/null | sed -n 's/^intercom-hub \([^ ]*\)$/\1/p' | head -n 1 || true
}

# _strih_hub_record_version BIN TAG SHA -> write BIN.version (version / release_tag / tarball_sha256), so
# verify item 16 can grade a hub that does not answer and an operator can read where it came from.
_strih_hub_record_version() {
  local bin="$1" tag="$2" sha="$3" ver
  ver="$(_strih_hub_version "$bin")"
  if ! printf 'version=%s\nrelease_tag=%s\ntarball_sha256=%s\n' "${ver:-unknown}" "$tag" "$sha" > "${bin}.version" 2>/dev/null; then
    echo "  WARNING: could not record the intercom-hub version at ${bin}.version" >&2
  fi
}

# strih_intercom_hub_binary_install BIN DIR ROOT -> BIN is the pinned release's binary from the bundle DIR
# (setup-strih step 13, before the routing file). An installed binary of another version is kept as
# BIN.pre-<version> and the new one renamed over it, so a running hub keeps its old inode: it is never
# restarted here and a crash restart loads the pair. rc 1 + a named line on failure.
strih_intercom_hub_binary_install() {
  local bin="${1:?hub binary path required}" dir="${2:?bundle dir required}" root="${3:?tree required}"
  local tag sha want have
  tag="$(strih_intercom_pin_tag "$root")" || return 1
  sha="$(strih_intercom_pin_sha256 "$root")" || return 1
  want="${tag#v}"
  have=""
  if [ -e "$bin" ]; then have="$(_strih_hub_version "$bin")"; fi
  if [ "$have" = "$want" ]; then
    echo "  intercom-hub ${want} present (${bin}), the pinned ${tag}"
  else
    if [ -e "$bin" ]; then
      cp -p "$bin" "${bin}.pre-${have:-unknown}" || { echo "strih-intercom-release: cannot keep ${bin}" >&2; return 1; }
    fi
    install -D -m 0755 "${dir}/intercom-hub" "${bin}.new" && mv -f "${bin}.new" "$bin" \
      || { rm -f "${bin}.new"; echo "strih-intercom-release: could not install ${bin}" >&2; return 1; }
    echo "  installed intercom-hub ${want} (${tag}) -> ${bin}${have:+, was ${have}: restart the hub to load it}"
  fi
  _strih_hub_record_version "$bin" "$tag" "$sha"
}
