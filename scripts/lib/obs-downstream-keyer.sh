#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: strict mode belongs to the caller (setup-strih.sh / verify-strih.sh).
#
# scripts/lib/obs-downstream-keyer.sh -- issue 1361: the Downstream Keyer OBS frontend plugin
# (exeldro/obs-downstream-keyer) the strih operator's collection uses. On strih-lx it was extracted
# by hand from the upstream .deb (not dpkg-owned; the OBS log shows `[Downstream Keyer] loaded version
# 0.4.4`). This lib installs the SAME file from the SAME pinned release asset, so a fresh
# setup-strih.sh run reproduces it:
#   * the .deb is pinned by URL + sha256 (the GitHub release digest), fail-loud on a mismatch;
#   * it is EXTRACTED (`dpkg-deb -x`), never dpkg-installed: its `Depends: obs-studio` is never met by
#     the genlock bundle OBS, which is installed from a tarball into the /usr prefix;
#   * the extracted plugin must hash to the pinned .so sha256 (== the live strih-lx file, byte for
#     byte), and only the plugin + its locale dir are installed into the /usr prefix OBS loads;
#   * idempotent: a box whose installed plugin already has the pinned hash downloads nothing.
#
#   obs_dsk_install_cmds VERSION DEB_URL DEB_SHA SO_SHA LIBDIR SHAREDIR -> the emitted install block
#   obs_dsk_verdict SO_SHA WANT_SHA LOCALE_PRESENT                      -> ok | FAIL: <reason>

# obs_dsk_version -> the pinned plugin version (the one strih-lx loads, 25.9.2026).
obs_dsk_version() { printf '0.4.4'; }

# obs_dsk_deb_url -> the pinned upstream release asset.
obs_dsk_deb_url() {
    printf 'https://github.com/exeldro/obs-downstream-keyer/releases/download/0.4.4/downstream-keyer-0.4.4-x86_64-linux-gnu.deb'
}

# obs_dsk_deb_sha256 -> the release asset's sha256 (its GitHub release digest).
obs_dsk_deb_sha256() { printf 'ad585eec720c3cf690dd548b6cfc72ea5007b447954559c0861959ee60b0cf67'; }

# obs_dsk_so_sha256 -> the sha256 of the plugin inside that .deb (== the live strih-lx file).
obs_dsk_so_sha256() { printf '9304d665e7fc96ea54faf7ee31ab5ce0462069de668bcb4be996054876508be5'; }

# obs_dsk_so_path LIBDIR -> where OBS loads the plugin from (LIBDIR = /usr/lib/x86_64-linux-gnu).
obs_dsk_so_path() { printf '%s/obs-plugins/downstream-keyer.so' "${1:-/usr/lib/x86_64-linux-gnu}"; }

# obs_dsk_data_dir SHAREDIR -> the plugin's data dir (SHAREDIR = /usr/share).
obs_dsk_data_dir() { printf '%s/obs/obs-plugins/downstream-keyer' "${1:-/usr/share}"; }

# obs_dsk_install_cmds VERSION DEB_URL DEB_SHA SO_SHA LIBDIR SHAREDIR -> print the idempotent bash
# block that installs the plugin (the strih_rustdesk_install_cmds shape). Every value is %q-quoted;
# the block `exit`s on any failure, so the caller runs it as `( eval "$(obs_dsk_install_cmds ...)" )
# || fail "..."`. The last statement ends with `;` (the $(...)-embedding glue gotcha).
obs_dsk_install_cmds() {
    local version="${1:?downstream-keyer version required}" url="${2:?downstream-keyer .deb url required}"
    local deb_sha="${3:?downstream-keyer .deb sha256 required}" so_sha="${4:?downstream-keyer .so sha256 required}"
    local libdir="${5:?libdir required}" sharedir="${6:?sharedir required}"
    local q_so q_data q_url q_deb_sha q_so_sha
    q_so="$(printf '%q' "$(obs_dsk_so_path "$libdir")")"
    q_data="$(printf '%q' "$(obs_dsk_data_dir "$sharedir")")"
    q_url="$(printf '%q' "$url")"
    q_deb_sha="$(printf '%q' "$deb_sha")"
    q_so_sha="$(printf '%q' "$so_sha")"
    cat <<EOF
__dsk_so=${q_so};
__dsk_data=${q_data};
if [ -f "\$__dsk_so" ] && [ "\$(sha256sum "\$__dsk_so" | awk '{print \$1}')" = ${q_so_sha} ] && [ -f "\$__dsk_data/locale/en-US.ini" ]; then
  echo "  downstream-keyer ${version} already installed (\$__dsk_so, pinned sha256)";
else
  command -v curl >/dev/null 2>&1 || DEBIAN_FRONTEND=noninteractive apt-get install -y curl >/dev/null || { echo "downstream-keyer: curl missing and apt-get install curl failed" >&2; exit 1; };
  __dsk_tmp="\$(mktemp -d "\${TMPDIR:-/tmp}/downstream-keyer-${version}.XXXXXX")" || { echo "downstream-keyer: mktemp failed" >&2; exit 1; };
  curl -fsSL -o "\$__dsk_tmp/dsk.deb" ${q_url} || { echo "downstream-keyer: download failed (${url})" >&2; rm -rf "\$__dsk_tmp"; exit 1; };
  __dsk_got="\$(sha256sum "\$__dsk_tmp/dsk.deb" | awk '{print \$1}')";
  if [ "\$__dsk_got" != ${q_deb_sha} ]; then echo "downstream-keyer: .deb sha256 mismatch (want ${deb_sha}, got \$__dsk_got) -- refusing to install" >&2; rm -rf "\$__dsk_tmp"; exit 1; fi;
  dpkg-deb -x "\$__dsk_tmp/dsk.deb" "\$__dsk_tmp/x" || { echo "downstream-keyer: dpkg-deb -x failed" >&2; rm -rf "\$__dsk_tmp"; exit 1; };
  __dsk_new="\$__dsk_tmp/x/usr/lib/x86_64-linux-gnu/obs-plugins/downstream-keyer.so";
  __dsk_loc="\$__dsk_tmp/x/usr/share/obs/obs-plugins/downstream-keyer/locale";
  if [ ! -f "\$__dsk_new" ] || [ ! -d "\$__dsk_loc" ]; then echo "downstream-keyer: the .deb has no plugin/locale at the expected paths" >&2; rm -rf "\$__dsk_tmp"; exit 1; fi;
  __dsk_got="\$(sha256sum "\$__dsk_new" | awk '{print \$1}')";
  if [ "\$__dsk_got" != ${q_so_sha} ]; then echo "downstream-keyer: plugin sha256 mismatch (want ${so_sha}, got \$__dsk_got)" >&2; rm -rf "\$__dsk_tmp"; exit 1; fi;
  mkdir -p "\$(dirname "\$__dsk_so")" "\$__dsk_data/locale" || { rm -rf "\$__dsk_tmp"; exit 1; };
  install -m 0644 "\$__dsk_new" "\$__dsk_so" || { echo "downstream-keyer: installing \$__dsk_so failed" >&2; rm -rf "\$__dsk_tmp"; exit 1; };
  cp -a "\$__dsk_loc/." "\$__dsk_data/locale/" || { echo "downstream-keyer: installing the locale failed" >&2; rm -rf "\$__dsk_tmp"; exit 1; };
  chmod 0755 "\$__dsk_data" "\$__dsk_data/locale";
  chmod 0644 "\$__dsk_data/locale/"*;
  rm -rf "\$__dsk_tmp";
  echo "  downstream-keyer ${version} installed -> \$__dsk_so (+ locale)";
fi;
EOF
}

# obs_dsk_verdict SO_SHA WANT_SHA LOCALE_PRESENT -> `ok` (rc 0), else `FAIL: missing` /
# `FAIL: sha-mismatch` / `FAIL: no-locale` (rc 1). SO_SHA = the installed plugin's sha256 (empty =
# absent); LOCALE_PRESENT = 1 when <data dir>/locale/en-US.ini exists. Pure.
obs_dsk_verdict() {
    local got="${1-}" want="${2-}" loc="${3-}"
    if [ -z "$got" ]; then echo "FAIL: missing"; return 1; fi
    if [ -z "$want" ] || [ "$got" != "$want" ]; then echo "FAIL: sha-mismatch"; return 1; fi
    if [ "$loc" != 1 ]; then echo "FAIL: no-locale"; return 1; fi
    echo "ok"
    return 0
}
