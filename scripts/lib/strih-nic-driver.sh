#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih rig-NIC driver planner, apply step and grader;
# no top-level statements besides constants) -- the sibling scripts/lib/*.sh convention of NOT setting
# `set -euo pipefail` here: sourcing runs in the CALLER's shell, so strict mode would leak into it.
# setup-strih.sh / verify-strih.sh set their own. Every function checks its own return codes, because a
# caller runs it in an `||` list, where errexit is off.
#
# scripts/lib/strih-nic-driver.sh -- issue 1391: the strih rig NIC's out-of-tree kernel driver, and the
# two link facts that decide whether its 5 GbE link loses packets.
#
# The rig NIC of strih-lx since 29.9.2026 is a Ubiquiti UACC-Adapter-RJ45-USBC-5GE = Realtek RTL8157 (USB
# 0bda:8157). The in-tree r8152 of the 7.0 kernels has no 0bda:8157 id, so the Realtek out-of-tree driver
# is vendored at vendor/realtek-r8152 (the pinned v2.21.4 source + camera-box's dkms.conf + SHA256SUMS)
# and built through DKMS, which rebuilds it for every kernel installed later. Measured live 29.9.2026:
# plugged one way round, the laptop's USB-C port trained the adapter at Gen 1 (5000 Mb/s) and the NIC
# lost ~240 packets/s at 5 GbE; flipped 180 degrees it trained at 10000 Mb/s and lost none.
#
#   * strih_nic_driver_plan (PURE) -- the installed state vs the box fact -> SKIP | NOOP | INSTALL | UPGRADE.
#   * strih_nic_driver_apply -- setup-strih step 1b: the vendored source -> /usr/src, dkms add/build/install
#     for the running kernel and every other installed kernel with headers, the hand-copied plain module
#     moved aside, another DKMS version removed per kernel once the vendored one is installed there, the
#     udev rule. It NEVER reloads the module live (`modprobe -r r8152` drops the rig NIC, the ssh session
#     running this and dantesync's PTP): the DKMS module loads at the next boot.
#   * strih_nic_grade_rows / strih_nic_grade_report -- verify-strih item 36: the loaded module version, DKMS
#     for the running kernel, the USB device speed, the Ethernet link speed and the NetworkManager profile
#     pinned to the rig NIC, each graded against the box facts. An unreadable value is never a pass.
#
# Test seams (tests/python/test_strih_nic_driver_1391.py): STRIH_NIC_DRV_KERNEL (the running kernel),
# STRIH_NIC_DRV_MODULES_ROOT (/lib/modules), STRIH_NIC_DRV_SRC_ROOT (/usr/src), STRIH_NIC_DRV_UDEV_DIR
# (/etc/udev/rules.d), STRIH_NIC_DRV_BACKUP_DIR (/var/lib/camera-box/strih-nic-driver), STRIH_NIC_DRV_SYSROOT
# (/sys, the apply's closing read of the loaded version); `dkms`, `modinfo`,
# `depmod`, `udevadm`, `apt-get` and `nmcli` are looked up on PATH.

# The vendored driver. The names are pinned against vendor/realtek-r8152/dkms.conf by a test.
STRIH_NIC_DRV_PACKAGE=realtek-r8152
STRIH_NIC_DRV_VERSION=2.21.4
STRIH_NIC_DRV_MODULE=r8152
STRIH_NIC_DRV_VENDOR_DIR=vendor/realtek-r8152
STRIH_NIC_DRV_UDEV_RULE=50-usb-realtek-net.rules

# The physical fix a USB link below the box fact needs (the 29.9.2026 finding above).
STRIH_NIC_USB_FIX="the USB-C connector trained at Gen 1 -- flip it 180 degrees / use the USB-C port, USB-A is 5 Gb/s"

# --- constants + path seams ------------------------------------------------------------------------

# strih_nic_driver_vendored_spec -> the box-fact spelling (STRIH_NIC_OOT_DRIVER) of the vendored driver.
strih_nic_driver_vendored_spec() { printf '%s-%s' "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION"; }

strih_nic_driver_kernel() {
  if [ -n "${STRIH_NIC_DRV_KERNEL:-}" ]; then printf '%s' "$STRIH_NIC_DRV_KERNEL"; else uname -r; fi
}
strih_nic_driver_modules_root() { printf '%s' "${STRIH_NIC_DRV_MODULES_ROOT:-/lib/modules}"; }
strih_nic_driver_src_dir() {
  printf '%s/%s-%s' "${STRIH_NIC_DRV_SRC_ROOT:-/usr/src}" "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION"
}
strih_nic_driver_udev_dir() { printf '%s' "${STRIH_NIC_DRV_UDEV_DIR:-/etc/udev/rules.d}"; }
strih_nic_driver_backup_dir() { printf '%s' "${STRIH_NIC_DRV_BACKUP_DIR:-/var/lib/camera-box/strih-nic-driver}"; }

# strih_nic_driver_spec_check SPEC NIC_DRIVER -> rc 0 iff SPEC (fact STRIH_NIC_OOT_DRIVER) names the
# vendored driver AND NIC_DRIVER (fact STRIH_NIC_DRIVER, the rig-NIC selection rule) is the module it
# builds; else one stderr line, rc 1. A `none` box never calls this.
strih_nic_driver_spec_check() {
  local spec="${1-}" drv="${2-}" want
  want="$(strih_nic_driver_vendored_spec)"
  if [ "$spec" != "$want" ]; then
    echo "strih-nic-driver: STRIH_NIC_OOT_DRIVER is '${spec}' but this tree vendors ${want} (${STRIH_NIC_DRV_VENDOR_DIR}) -- refusing a driver it does not carry" >&2
    return 1
  fi
  if [ "$drv" != "$STRIH_NIC_DRV_MODULE" ]; then
    echo "strih-nic-driver: STRIH_NIC_DRIVER is '${drv}' but ${want} builds the module ${STRIH_NIC_DRV_MODULE} -- the rig-NIC resolver would look for another driver" >&2
    return 1
  fi
}

# strih_nic_driver_version_norm TEXT -> the bare version of a module version string (`v2.21.4 (2025/10/28)`
# -> `2.21.4`); nothing + rc 1 for an empty or non-version value.
strih_nic_driver_version_norm() {
  local v="${1-}"
  v="${v#"${v%%[![:space:]]*}"}"
  v="${v%%[[:space:]]*}"
  v="${v#v}"
  [[ "$v" =~ ^[0-9]+(\.[0-9]+)+$ ]] || return 1
  printf '%s' "$v"
}

# --- `dkms status` parsing (PURE) ------------------------------------------------------------------

# strih_nic_dkms_rows PACKAGE  (stdin: `dkms status`) -> one `VERSION|KERNEL|STATE` row per PACKAGE line
# (KERNEL empty for `added` / `broken`). Reads the DKMS 3.x shape `pkg/ver, kernel, arch: state` and the
# 2.x shape `pkg, ver, kernel, arch: state`; a trailing note such as `(WARNING! ...)` is dropped, so the
# state is one word (installed, installed-weak, built, added, broken).
strih_nic_dkms_rows() {
  local pkg="${1:?package required}" line head state rest ver kern
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in *': '*) ;; *) continue ;; esac
    head="${line%%: *}"
    state="${line#*: }"
    state="${state%%[[:space:]]*}"
    case "$head" in "${pkg}/"*) head="${pkg}, ${head#"${pkg}/"}" ;; esac
    case "$head" in "${pkg}, "*) ;; *) continue ;; esac
    rest="${head#"${pkg}, "}"
    ver="${rest%%,*}"
    kern=""
    case "$rest" in *,*) rest="${rest#*,}"; rest="${rest# }"; kern="${rest%%,*}" ;; esac
    printf '%s|%s|%s\n' "$ver" "$kern" "$state"
  done
}

# strih_nic_dkms_state PACKAGE VERSION KERNEL  (stdin: `dkms status`) -> PACKAGE/VERSION's state for KERNEL:
# installed | built | added (known to DKMS, not built for KERNEL) | broken (its source is gone) | absent.
strih_nic_dkms_state() {
  local pkg="${1:?package required}" ver="${2:?version required}" kern="${3:?kernel required}"
  local v k s rank=0 r
  while IFS='|' read -r v k s; do
    [ -n "$v" ] && [ "$v" = "$ver" ] || continue
    case "$s" in
      installed) if [ "$k" = "$kern" ]; then r=5; else r=2; fi ;;
      built) if [ "$k" = "$kern" ]; then r=4; else r=2; fi ;;
      broken) r=3 ;;
      *) r=2 ;;
    esac
    [ "$r" -gt "$rank" ] && rank="$r"
  done < <(strih_nic_dkms_rows "$pkg")
  case "$rank" in
    5) printf 'installed' ;;
    4) printf 'built' ;;
    3) printf 'broken' ;;
    2) printf 'added' ;;
    *) printf 'absent' ;;
  esac
}

# strih_nic_dkms_other_versions PACKAGE VERSION  (stdin: `dkms status`) -> every OTHER version of PACKAGE
# that DKMS knows, one per line, deduplicated.
strih_nic_dkms_other_versions() {
  local pkg="${1:?package required}" ver="${2:?version required}" v k s seen=" "
  while IFS='|' read -r v k s; do
    [ -n "$v" ] && [ "$v" != "$ver" ] || continue
    case "$seen" in *" ${v} "*) continue ;; esac
    seen="${seen}${v} "
    printf '%s\n' "$v"
  done < <(strih_nic_dkms_rows "$pkg")
}

# strih_nic_dkms_other_active PACKAGE VERSION KERNEL  (stdin: `dkms status`) -> the OTHER version of
# PACKAGE that is installed (= active, DKMS keeps one per kernel) for KERNEL, or nothing.
strih_nic_dkms_other_active() {
  local pkg="${1:?package required}" ver="${2:?version required}" kern="${3:?kernel required}" v k s
  while IFS='|' read -r v k s; do
    [ -n "$v" ] && [ "$v" != "$ver" ] && [ "$k" = "$kern" ] && [ "$s" = installed ] || continue
    printf '%s' "$v"
    return 0
  done < <(strih_nic_dkms_rows "$pkg")
  return 0
}

# --- the planner (PURE) ----------------------------------------------------------------------------

# strih_nic_driver_plan SPEC KERNEL MODINFO_VERSION PLAIN_VERSIONS  (stdin: `dkms status`) -> ONE token:
#   SKIP     SPEC is `none`: the box has no out-of-tree NIC driver.
#   NOOP     SPEC's package is installed in DKMS for KERNEL, the module KERNEL would load (MODINFO_VERSION,
#            `modinfo -k KERNEL -F version <module>`) is SPEC's version, and no hand-copied plain module is
#            left in /lib/modules/KERNEL/updates (PLAIN_VERSIONS empty).
#   UPGRADE  an earlier install has to be replaced on KERNEL: another version of the package is the one
#            DKMS has installed for KERNEL, or a plain copy is present (PLAIN_VERSIONS = the version of
#            each one, `unknown` when unreadable). Another version left on OTHER kernels only (one without
#            headers keeps it, see _strih_nic_driver_remove_other_versions) does not change KERNEL's plan.
#   INSTALL  anything else: nothing yet, or SPEC only partly in place.
# rc 1, nothing on stdout, one stderr line: an empty KERNEL or a SPEC this tree does not vendor.
strih_nic_driver_plan() {
  local spec="${1-}" kern="${2-}" mi="${3-}" plain="${4-}" status state others mv
  if [ "$spec" = none ]; then printf 'SKIP'; return 0; fi
  if [ "$spec" != "$(strih_nic_driver_vendored_spec)" ]; then
    echo "strih_nic_driver_plan: '${spec}' is not the vendored $(strih_nic_driver_vendored_spec)" >&2
    return 1
  fi
  if [ -z "$kern" ]; then
    echo "strih_nic_driver_plan: no kernel release" >&2
    return 1
  fi
  status="$(cat)"
  state="$(strih_nic_dkms_state "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$kern" <<<"$status")"
  others="$(strih_nic_dkms_other_active "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$kern" <<<"$status")"
  mv="$(strih_nic_driver_version_norm "$mi" || true)"
  plain="${plain//[[:space:]]/}"
  if [ "$state" = installed ] && [ "$mv" = "$STRIH_NIC_DRV_VERSION" ] && [ -z "$plain" ]; then
    printf 'NOOP'
  elif [ -n "$others" ] || [ -n "$plain" ]; then
    printf 'UPGRADE'
  else
    printf 'INSTALL'
  fi
}

# --- the live reads behind the planner -------------------------------------------------------------

# strih_nic_driver_plain_copies KERNEL -> every hand-copied plain module of the driver for KERNEL, one
# path per line: <modules root>/KERNEL/updates/<module>.ko[.zst|.xz|.gz] -- never updates/dkms/.
strih_nic_driver_plain_copies() {
  local k="${1:?kernel required}" d f
  d="$(strih_nic_driver_modules_root)/${k}/updates"
  for f in "$d/${STRIH_NIC_DRV_MODULE}.ko" "$d/${STRIH_NIC_DRV_MODULE}.ko.zst" \
    "$d/${STRIH_NIC_DRV_MODULE}.ko.xz" "$d/${STRIH_NIC_DRV_MODULE}.ko.gz"; do
    [ -f "$f" ] && printf '%s\n' "$f"
  done
  return 0
}

# strih_nic_driver_file_version PATH -> the bare version `modinfo -F version PATH` reports, else `unknown`.
strih_nic_driver_file_version() {
  strih_nic_driver_version_norm "$(modinfo -F version "$1" 2>/dev/null || true)" || printf 'unknown'
}

# strih_nic_driver_dkms_status -> `dkms status` for the package (empty when dkms is absent).
strih_nic_driver_dkms_status() {
  command -v dkms >/dev/null 2>&1 || return 0
  dkms status -m "$STRIH_NIC_DRV_PACKAGE" 2>/dev/null || true
}

# strih_nic_driver_live_plan SPEC KERNEL -> strih_nic_driver_plan over the live state of KERNEL.
strih_nic_driver_live_plan() {
  local spec="$1" k="$2" mi plain="" f
  mi="$(modinfo -k "$k" -F version "$STRIH_NIC_DRV_MODULE" 2>/dev/null || true)"
  while IFS= read -r f; do
    [ -n "$f" ] && plain="${plain:+$plain }$(strih_nic_driver_file_version "$f")"
  done < <(strih_nic_driver_plain_copies "$k")
  strih_nic_driver_dkms_status | strih_nic_driver_plan "$spec" "$k" "$mi" "$plain"
}

# --- the apply step (setup-strih step 1b) ----------------------------------------------------------

# _strih_nic_driver_prereqs KERNEL -> dkms + KERNEL's headers installed (apt-get only when missing).
_strih_nic_driver_prereqs() {
  local k="$1" pkgs=""
  command -v dkms >/dev/null 2>&1 || pkgs="dkms"
  [ -e "$(strih_nic_driver_modules_root)/${k}/build/Makefile" ] || pkgs="${pkgs:+$pkgs }linux-headers-${k}"
  [ -n "$pkgs" ] || return 0
  echo "  installing ${pkgs} (DKMS builds the driver against the running kernel's headers)"
  if declare -F obs_box_apt_update >/dev/null; then obs_box_apt_update || return 1; fi
  # shellcheck disable=SC2086  # word-split the package list on purpose
  DEBIAN_FRONTEND=noninteractive apt-get install -y $pkgs || {
    echo "strih-nic-driver: apt-get install ${pkgs} failed" >&2; return 1; }
  command -v dkms >/dev/null 2>&1 || { echo "strih-nic-driver: dkms still missing after the install" >&2; return 1; }
  [ -e "$(strih_nic_driver_modules_root)/${k}/build/Makefile" ] || {
    echo "strih-nic-driver: the headers of ${k} are still missing after the install -- DKMS cannot build" >&2; return 1; }
}

# _strih_nic_driver_verify_vendored VENDOR_DIR -> rc 0 iff the staged vendored tree matches its SHA256SUMS;
# else one stderr line (not staged / changed), rc 1. Nothing from the tree is used before this passes.
_strih_nic_driver_verify_vendored() {
  local vdir="$1"
  if [ ! -f "${vdir}/SHA256SUMS" ]; then
    echo "strih-nic-driver: ${vdir} is not staged -- run setup-strih.sh from a tree that carries ${STRIH_NIC_DRV_VENDOR_DIR} (the genlock deploy archives it)" >&2
    return 1
  fi
  (cd "$vdir" && sha256sum --quiet --strict -c SHA256SUMS) || {
    echo "strih-nic-driver: ${vdir} does not match its SHA256SUMS -- refusing a changed driver source" >&2; return 1; }
}

# _strih_nic_driver_sync_source REPO_ROOT -> the vendored tree verified against its SHA256SUMS and copied
# to /usr/src/<package>-<version> (the DKMS source tree) unless that copy already verifies.
_strih_nic_driver_sync_source() {
  local vdir="${1%/}/${STRIH_NIC_DRV_VENDOR_DIR}" dst sum f
  dst="$(strih_nic_driver_src_dir)"
  _strih_nic_driver_verify_vendored "$vdir" || return 1
  if [ -f "${dst}/SHA256SUMS" ] && cmp -s "${vdir}/SHA256SUMS" "${dst}/SHA256SUMS" \
    && (cd "$dst" && sha256sum --quiet --strict -c SHA256SUMS >/dev/null 2>&1); then
    return 0
  fi
  mkdir -p "$dst" || return 1
  while read -r sum f; do
    [ -n "$sum" ] && [ -n "$f" ] || continue
    cp -f "${vdir}/${f}" "${dst}/${f}" || return 1
  done < "${vdir}/SHA256SUMS"
  cp -f "${vdir}/SHA256SUMS" "${dst}/SHA256SUMS" || return 1
  (cd "$dst" && sha256sum --quiet --strict -c SHA256SUMS) || {
    echo "strih-nic-driver: the copy in ${dst} does not verify" >&2; return 1; }
  echo "  driver source ${STRIH_NIC_DRV_PACKAGE} ${STRIH_NIC_DRV_VERSION} -> ${dst} (SHA256SUMS verified)"
}

# _strih_nic_driver_restore_plain KERNEL FROM... -- TO... : put moved-aside plain copies back + depmod.
_strih_nic_driver_restore_plain() {
  local k="$1" i
  shift
  local -a from=() to=()
  while [ "$#" -gt 0 ] && [ "$1" != -- ]; do from+=("$1"); shift; done
  [ "$#" -gt 0 ] && shift
  to=("$@")
  for i in "${!from[@]}"; do mv -f "${to[$i]}" "${from[$i]}" || true; done
  depmod -a "$k" || true
}

# _strih_nic_driver_install_kernel KERNEL -> the vendored version built + installed in DKMS for KERNEL.
# Why the order: DKMS 3.x `dkms install` compares the new module with the FIRST same-named module `find`
# returns in the kernel's module tree (a same-version hand copy makes it refuse "already installed"), and
# moves the one it replaces into its own original_module/ store, restored on a later `dkms remove`. So:
# a plain copy of ANOTHER version refuses before anything changes; the module is built; the known plain
# copy is moved to the backup dir; `dkms install` (over an older DKMS version, which stays until the
# caller removes it once every kernel is installed); a failed move or install puts every moved copy
# back (+ depmod). `dkms install` copies the new module over the active old one before its own depmod,
# and on a depmod failure DKMS uninstalls the new one, so the old one is gone from disk: a failed install
# therefore re-installs the previously active version (still built) and reads it back.
_strih_nic_driver_install_kernel() {
  local k="$1" state f pv prev now
  local -a from=() to=()
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    pv="$(strih_nic_driver_file_version "$f")"
    if [ "$pv" != "$STRIH_NIC_DRV_VERSION" ]; then
      echo "strih-nic-driver: ${f} is a hand-installed ${STRIH_NIC_DRV_MODULE} of version ${pv}, not the known ${STRIH_NIC_DRV_VERSION} copy -- remove it by hand, then re-run" >&2
      return 1
    fi
  done < <(strih_nic_driver_plain_copies "$k")
  state="$(strih_nic_driver_dkms_status | strih_nic_dkms_state "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$k")"
  prev="$(strih_nic_driver_dkms_status | strih_nic_dkms_other_active "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$k")"
  if [ "$state" != installed ] && [ "$state" != built ]; then
    echo "  dkms build ${STRIH_NIC_DRV_PACKAGE}/${STRIH_NIC_DRV_VERSION} for ${k}"
    dkms build -m "$STRIH_NIC_DRV_PACKAGE" -v "$STRIH_NIC_DRV_VERSION" -k "$k" || {
      echo "strih-nic-driver: dkms build for ${k} failed (the running module and the files on disk are unchanged)" >&2; return 1; }
  fi
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    if ! mkdir -p "$(strih_nic_driver_backup_dir)/${k}" || ! mv -f "$f" "$(strih_nic_driver_backup_dir)/${k}/"; then
      _strih_nic_driver_restore_plain "$k" "${from[@]}" -- "${to[@]}"
      echo "strih-nic-driver: could not move ${f} aside -- every moved copy is back in place" >&2
      return 1
    fi
    from+=("$f"); to+=("$(strih_nic_driver_backup_dir)/${k}/${f##*/}")
    echo "  moved the hand-copied ${f} aside -> ${to[-1]}"
  done < <(strih_nic_driver_plain_copies "$k")
  if [ "$state" != installed ]; then
    echo "  dkms install ${STRIH_NIC_DRV_PACKAGE}/${STRIH_NIC_DRV_VERSION} for ${k}"
    if ! dkms install -m "$STRIH_NIC_DRV_PACKAGE" -v "$STRIH_NIC_DRV_VERSION" -k "$k"; then
      _strih_nic_driver_restore_plain "$k" "${from[@]}" -- "${to[@]}"
      if [ -n "$prev" ] && [ "$(strih_nic_driver_dkms_status | strih_nic_dkms_state "$STRIH_NIC_DRV_PACKAGE" "$prev" "$k")" != installed ]; then
        dkms install -m "$STRIH_NIC_DRV_PACKAGE" -v "$prev" -k "$k" >&2 || true
        now="$(strih_nic_driver_version_norm "$(modinfo -k "$k" -F version "$STRIH_NIC_DRV_MODULE" 2>/dev/null || true)" || true)"
        echo "strih-nic-driver: dkms install for ${k} failed -- re-installed the previous ${STRIH_NIC_DRV_PACKAGE}/${prev}, ${k} now loads version '${now:-none}'" >&2
      else
        echo "strih-nic-driver: dkms install for ${k} failed -- any hand-copied module is back in place${prev:+, the previous ${prev} is still installed}" >&2
      fi
      return 1
    fi
  fi
  depmod -a "$k" || { echo "strih-nic-driver: depmod -a ${k} failed" >&2; return 1; }
}

# _strih_nic_driver_remove_other_versions -> every OTHER version of the package removed PER KERNEL, run
# after the installs: `dkms remove -m <pkg> -v <old> -k <kernel>` only on a kernel where the vendored
# version is now installed (the old one is no longer active there, so DKMS only unbuilds it; the version
# goes away with its last kernel). On a kernel the vendored version is NOT installed on (no headers, so
# nothing built it), the old version is kept -- a `--all` would delete the only driver that kernel has --
# with a WARNING naming it. A version DKMS knows only as `added` (never built) is removed with --all.
_strih_nic_driver_remove_other_versions() {
  local status old v k s built
  status="$(strih_nic_driver_dkms_status)"
  while IFS= read -r old; do
    [ -n "$old" ] || continue
    built=0
    while IFS='|' read -r v k s; do
      [ "$v" = "$old" ] && [ -n "$k" ] || continue
      built=1
      if [ "$(strih_nic_dkms_state "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$k" <<<"$status")" = installed ]; then
        echo "  dkms remove ${STRIH_NIC_DRV_PACKAGE}/${old} for ${k} (${STRIH_NIC_DRV_VERSION} is installed there)"
        dkms remove -m "$STRIH_NIC_DRV_PACKAGE" -v "$old" -k "$k" || {
          echo "strih-nic-driver: dkms remove of ${STRIH_NIC_DRV_PACKAGE}/${old} for ${k} failed" >&2; return 1; }
      else
        echo "  WARNING: ${STRIH_NIC_DRV_PACKAGE}/${old} is kept for ${k} (${s}): ${STRIH_NIC_DRV_VERSION} is not installed there -- install linux-headers-${k} (DKMS then builds it) or remove that kernel, then re-run" >&2
      fi
    done < <(strih_nic_dkms_rows "$STRIH_NIC_DRV_PACKAGE" <<<"$status")
    if [ "$built" = 0 ]; then
      echo "  dkms remove ${STRIH_NIC_DRV_PACKAGE}/${old} (added, never built)"
      dkms remove -m "$STRIH_NIC_DRV_PACKAGE" -v "$old" --all || {
        echo "strih-nic-driver: dkms remove of ${STRIH_NIC_DRV_PACKAGE}/${old} failed" >&2; return 1; }
    fi
  done < <(strih_nic_dkms_other_versions "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" <<<"$status")
}

# _strih_nic_driver_other_kernels RUNNING -> every other installed kernel whose headers are present (DKMS
# AUTOINSTALL only covers kernels installed AFTER the package was added), one per line. A kernel without
# headers is skipped: nothing can build for it until its headers are installed, and then the DKMS headers
# hook builds this package for it.
_strih_nic_driver_other_kernels() {
  local run="$1" d k
  for d in "$(strih_nic_driver_modules_root)"/*/; do
    k="$(basename "$d")"
    [ "$k" != "$run" ] && [ -e "${d}build/Makefile" ] && printf '%s\n' "$k"
  done
  return 0
}

# _strih_nic_driver_udev_rule REPO_ROOT -> the vendored udev rule (it makes the adapter come up in its
# vendor configuration, so r8152 binds instead of cdc_ncm) installed when it differs; the rules are
# reloaded, never re-triggered (a trigger re-applies bConfigurationValue on the live NIC).
_strih_nic_driver_udev_rule() {
  local src="${1%/}/${STRIH_NIC_DRV_VENDOR_DIR}/${STRIH_NIC_DRV_UDEV_RULE}" dst
  dst="$(strih_nic_driver_udev_dir)/${STRIH_NIC_DRV_UDEV_RULE}"
  if [ ! -f "$src" ]; then
    [ -f "$dst" ] && { echo "  udev rule ${dst} present (the vendored copy is not staged -- not compared)"; return 0; }
    echo "strih-nic-driver: neither ${src} nor ${dst} exists" >&2
    return 1
  fi
  _strih_nic_driver_verify_vendored "${1%/}/${STRIH_NIC_DRV_VENDOR_DIR}" || return 1
  if cmp -s "$src" "$dst"; then
    echo "  udev rule ${dst} current"
    return 0
  fi
  mkdir -p "$(dirname "$dst")" || return 1
  install -m 0644 "$src" "$dst" || return 1
  udevadm control --reload-rules || { echo "strih-nic-driver: udevadm control --reload-rules failed" >&2; return 1; }
  echo "  udev rule installed -> ${dst} (rules reloaded; applies at the next plug / boot)"
}

# strih_nic_driver_apply REPO_ROOT SPEC NIC_DRIVER -> setup-strih step 1b for the box facts SPEC
# (STRIH_NIC_OOT_DRIVER) + NIC_DRIVER (STRIH_NIC_DRIVER). Idempotent: a NOOP plan changes no driver
# state. Ends with the planner re-run, which must read NOOP. rc 1 + a stderr line on any failure.
strih_nic_driver_apply() {
  local repo="${1:?repo root required}" spec="${2-}" nicdrv="${3-}" k plan loaded ok_k
  strih_nic_driver_spec_check "$spec" "$nicdrv" || return 1
  k="$(strih_nic_driver_kernel)" || return 1
  [ -n "$k" ] || { echo "strih-nic-driver: no running kernel release" >&2; return 1; }
  _strih_nic_driver_prereqs "$k" || return 1
  plan="$(strih_nic_driver_live_plan "$spec" "$k")" || return 1
  echo "  ${spec} on ${k}: ${plan}"
  if [ "$plan" != NOOP ]; then
    _strih_nic_driver_sync_source "$repo" || return 1
    if [ "$(strih_nic_driver_dkms_status | strih_nic_dkms_state "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$k")" = absent ]; then
      dkms add -m "$STRIH_NIC_DRV_PACKAGE" -v "$STRIH_NIC_DRV_VERSION" || {
        echo "strih-nic-driver: dkms add ${STRIH_NIC_DRV_PACKAGE}/${STRIH_NIC_DRV_VERSION} failed" >&2; return 1; }
    fi
    _strih_nic_driver_install_kernel "$k" || return 1
  fi
  while IFS= read -r ok_k; do
    [ -n "$ok_k" ] || continue
    [ "$(strih_nic_driver_dkms_status | strih_nic_dkms_state "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$ok_k")" = installed ] && continue
    echo "  also for the installed kernel ${ok_k} (the next boot may choose it)"
    _strih_nic_driver_sync_source "$repo" || return 1
    _strih_nic_driver_install_kernel "$ok_k" || return 1
  done < <(_strih_nic_driver_other_kernels "$k")
  _strih_nic_driver_remove_other_versions || return 1
  _strih_nic_driver_udev_rule "$repo" || return 1
  plan="$(strih_nic_driver_live_plan "$spec" "$k")" || return 1
  if [ "$plan" != NOOP ]; then
    echo "strih-nic-driver: after the install the plan for ${k} still reads ${plan}, not NOOP" >&2
    return 1
  fi
  loaded="$(cat "${STRIH_NIC_DRV_SYSROOT:-/sys}/module/${STRIH_NIC_DRV_MODULE}/version" 2>/dev/null || true)"
  echo "  ${spec} installed in DKMS for ${k}; reload pending: next boot (the loaded ${STRIH_NIC_DRV_MODULE} is '${loaded:-none}' -- modprobe -r would drop the rig NIC, this ssh session and dantesync's PTP, so it is never reloaded live)"
}

# --- the grader (verify-strih item 36) -------------------------------------------------------------

# strih_nic_speed_verdict KIND VALUE MIN NIC -> `OK|<detail>` or `FAIL|<detail>` (rc 0 / 1). KIND is `usb`
# (the USB device speed) or `link` (the Ethernet speed). VALUE is the raw sysfs text in Mb/s; an empty,
# `-1` (link down) or non-numeric value is unreadable and FAILs -- never a pass.
strih_nic_speed_verdict() {
  local kind="${1-}" val="${2-}" min="${3-}" nic="${4-}" v label fix
  case "$kind" in
    usb) label="USB link"; fix=" -- ${STRIH_NIC_USB_FIX}" ;;
    link) label="Ethernet link"; fix=" -- check the switch port, its module and the cable (5GBASE-T needs Cat6)" ;;
    *) printf 'FAIL|(nic) unknown speed kind %s\n' "$kind"; return 1 ;;
  esac
  v="${val//[[:space:]]/}"
  if [[ ! "$min" =~ ^[1-9][0-9]*$ ]]; then
    printf 'FAIL|(nic-%s) %s: no valid minimum speed (%s)\n' "$kind" "$nic" "${min:-<empty>}"
    return 1
  fi
  if [[ ! "$v" =~ ^[0-9]+$ ]]; then
    printf 'FAIL|(nic-%s) %s: %s speed unreadable (%s) -- never graded as a pass\n' "$kind" "$nic" "$label" "${v:-<empty>}"
    return 1
  fi
  if [ "$((10#$v))" -ge "$((10#$min))" ]; then
    printf 'OK|(nic-%s) %s: %s %s Mb/s >= %s\n' "$kind" "$nic" "$label" "$((10#$v))" "$min"
    return 0
  fi
  printf 'FAIL|(nic-%s) %s: %s %s Mb/s < %s%s\n' "$kind" "$nic" "$label" "$((10#$v))" "$min" "$fix"
  return 1
}

# strih_nic_usb_speed_file SYSROOT NIC -> the speed file of NIC's USB DEVICE: the parent of NIC's
# interface node (<SYSROOT>/class/net/NIC/device/..), never a hard-coded bus path. rc 1 when that parent
# is not a USB device (no idVendor + speed).
strih_nic_usb_speed_file() {
  local sysroot="${1:?sysroot required}" nic="${2:?nic required}" dev usbdev
  dev="$(readlink -f "${sysroot}/class/net/${nic}/device" 2>/dev/null || true)"
  [ -n "$dev" ] && [ -d "$dev" ] || return 1
  usbdev="$(dirname "$dev")"
  [ -f "${usbdev}/speed" ] && [ -f "${usbdev}/idVendor" ] || return 1
  printf '%s' "${usbdev}/speed"
}

# strih_nic_module_verdict SPEC LOADED_VERSION -> `OK|...` / `FAIL|...`: the loaded module's version
# (/sys/module/<module>/version) is SPEC's version.
strih_nic_module_verdict() {
  local spec="${1-}" raw="${2-}" v
  if [ "$spec" != "$(strih_nic_driver_vendored_spec)" ]; then
    printf 'FAIL|(nic-driver) STRIH_NIC_OOT_DRIVER is %s but this tree vendors %s\n' "$spec" "$(strih_nic_driver_vendored_spec)"
    return 1
  fi
  if ! v="$(strih_nic_driver_version_norm "$raw")"; then
    printf 'FAIL|(nic-driver) %s is not loaded or has no version (the in-tree driver has none and cannot bind the RTL8157) -- setup-strih step 1b + a reboot\n' "$STRIH_NIC_DRV_MODULE"
    return 1
  fi
  if [ "$v" = "$STRIH_NIC_DRV_VERSION" ]; then
    printf 'OK|(nic-driver) loaded %s %s = %s\n' "$STRIH_NIC_DRV_MODULE" "$v" "$spec"
    return 0
  fi
  printf 'FAIL|(nic-driver) loaded %s is %s, want %s -- the DKMS module loads at the next boot (never reloaded live)\n' "$STRIH_NIC_DRV_MODULE" "$v" "$STRIH_NIC_DRV_VERSION"
  return 1
}

# strih_nic_dkms_verdict SPEC KERNEL  (stdin: `dkms status`) -> `OK|...` / `FAIL|...`: SPEC's package is
# installed in DKMS for KERNEL.
strih_nic_dkms_verdict() {
  local spec="${1-}" k="${2-}" st
  st="$(strih_nic_dkms_state "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "${k:-<none>}")"
  if [ "$spec" = "$(strih_nic_driver_vendored_spec)" ] && [ -n "$k" ] && [ "$st" = installed ]; then
    printf 'OK|(nic-dkms) %s/%s installed for %s\n' "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$k"
    return 0
  fi
  printf 'FAIL|(nic-dkms) %s/%s is %s for %s (want installed) -- re-run setup-strih.sh step 1b\n' "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$st" "${k:-<no kernel>}"
  return 1
}

# strih_nic_nm_pin_verdict IP NIC PRESENT_IFACES  (stdin: `UUID|NAME|INTERFACE_NAME|IPV4_ADDRESSES` rows,
# one per NetworkManager connection) -> `OK|...` / `FAIL|...`. PASS needs a connection carrying IP whose
# connection.interface-name is NIC, and no connection carrying IP that is unpinned or pinned to ANOTHER
# present interface (it could come up there with the same address). A connection pinned to an interface
# that is not present (the replaced 2.5 GbE adapter) is dormant and ignored.
strih_nic_nm_pin_verdict() {
  local ip="${1-}" nic="${2-}" present=" ${3-} " u n ifn addrs a hit carrying=0 pinned="" bad=""
  while IFS='|' read -r u n ifn addrs; do
    [ -n "$u" ] || continue
    hit=0
    for a in ${addrs//,/ }; do [ "${a%%/*}" = "$ip" ] && hit=1; done
    [ "$hit" = 1 ] || continue
    carrying=$((carrying + 1))
    if [ "$ifn" = "$nic" ]; then
      pinned="${pinned:-$n}"
    elif [ -z "$ifn" ]; then
      bad="${bad}${bad:+; }'${n}' carries ${ip} unpinned (connection.interface-name empty)"
    else
      case "$present" in *" ${ifn} "*) bad="${bad}${bad:+; }'${n}' carries ${ip} on the present interface ${ifn}" ;; esac
    fi
  done
  if [ "$carrying" = 0 ]; then
    printf 'FAIL|(nic-nm) no NetworkManager connection carries %s -- setup-strih step 1 (operator): assign it to the rig NIC %s\n' "$ip" "${nic:-<unresolved>}"
    return 1
  fi
  if [ -n "$bad" ]; then
    printf 'FAIL|(nic-nm) %s -- pin it: nmcli connection modify <name> connection.interface-name %s\n' "$bad" "${nic:-<rig NIC>}"
    return 1
  fi
  if [ -z "$pinned" ]; then
    printf 'FAIL|(nic-nm) no connection carrying %s is pinned to the rig NIC %s (connection.interface-name)\n' "$ip" "${nic:-<unresolved>}"
    return 1
  fi
  printf 'OK|(nic-nm) NetworkManager connection %s carrying %s is pinned to %s\n' "$pinned" "$ip" "$nic"
}

# _strih_nic_nm_rows -> `UUID|NAME|INTERFACE_NAME|IPV4_ADDRESSES` per NetworkManager connection (read-only
# nmcli, one property per call so no value is ever split on a separator); rc 1 when nmcli is absent.
_strih_nic_nm_rows() {
  local u
  command -v nmcli >/dev/null 2>&1 || return 1
  while IFS= read -r u; do
    [ -n "$u" ] || continue
    printf '%s|%s|%s|%s\n' "$u" \
      "$(nmcli -g connection.id connection show "$u" 2>/dev/null || true)" \
      "$(nmcli -g connection.interface-name connection show "$u" 2>/dev/null || true)" \
      "$(nmcli -g ipv4.addresses connection show "$u" 2>/dev/null || true)"
  done < <(nmcli -g UUID connection show 2>/dev/null || true)
}

# strih_nic_grade_rows SYSROOT IP_ADDR_TEXT -> verify-strih item 36, one `OK|` / `FAIL|` / `NOTE|` row per
# check, read live (sysfs under SYSROOT, `dkms status`, `nmcli`) and graded against the loaded box facts.
# IP_ADDR_TEXT is `ip -o -4 addr show` output, the fallback of the rig-NIC resolver strih_lx_rig_nic
# (scripts/lib/strih-provision.sh, which the caller sources). Read-only; always rc 0 (the rows are the verdict).
strih_nic_grade_rows() {
  local sysroot="${1:-/sys}" addrs="${2-}" spec nicdrv ip nic k minusb minlink usbf present n nmrows
  if ! spec="$(strih_lx_nic_oot_driver)" || ! nicdrv="$(strih_lx_nic_driver)" || ! ip="$(strih_lx_ip)" \
    || ! minusb="$(strih_lx_nic_min_usb_mbps)" || ! minlink="$(strih_lx_nic_min_link_mbps)"; then
    printf 'FAIL|(nic) the box facts are not readable\n'
    return 0
  fi
  if [ "$spec" = none ]; then
    printf 'NOTE|(nic-driver) no out-of-tree NIC driver declared (STRIH_NIC_OOT_DRIVER=none)\n'
  else
    strih_nic_driver_spec_check "$spec" "$nicdrv" 2>/dev/null \
      || printf 'FAIL|(nic-driver) STRIH_NIC_OOT_DRIVER %s / STRIH_NIC_DRIVER %s do not match the vendored %s\n' "$spec" "$nicdrv" "$(strih_nic_driver_vendored_spec)"
    strih_nic_module_verdict "$spec" "$(cat "${sysroot}/module/${STRIH_NIC_DRV_MODULE}/version" 2>/dev/null || true)" || true
    k="$(strih_nic_driver_kernel 2>/dev/null || true)"
    if command -v dkms >/dev/null 2>&1; then
      strih_nic_driver_dkms_status | strih_nic_dkms_verdict "$spec" "$k" || true
    else
      printf 'FAIL|(nic-dkms) dkms is not installed -- re-run setup-strih.sh step 1b\n'
    fi
  fi
  nic="$(strih_lx_rig_nic "$sysroot" "$addrs" "$ip" 2>/dev/null || true)"
  if [ -z "$nic" ]; then
    printf 'FAIL|(nic) cannot resolve the rig NIC (%s driver / %s) -- USB, link and NM pinning not graded\n' "$nicdrv" "$ip"
    return 0
  fi
  if [ "$minusb" = none ]; then
    printf 'NOTE|(nic-usb) %s: not graded (STRIH_NIC_MIN_USB_MBPS=none, not a USB NIC)\n' "$nic"
  elif usbf="$(strih_nic_usb_speed_file "$sysroot" "$nic")"; then
    strih_nic_speed_verdict usb "$(cat "$usbf" 2>/dev/null || true)" "$minusb" "$nic" || true
  else
    printf 'FAIL|(nic-usb) %s: no USB device above its interface node (%s/class/net/%s/device/..) -- speed unreadable\n' "$nic" "$sysroot" "$nic"
  fi
  strih_nic_speed_verdict link "$(cat "${sysroot}/class/net/${nic}/speed" 2>/dev/null || true)" "$minlink" "$nic" || true
  present=""
  for n in "${sysroot}"/class/net/*; do [ -e "$n" ] && present="${present} ${n##*/}"; done
  if nmrows="$(_strih_nic_nm_rows)"; then
    strih_nic_nm_pin_verdict "$ip" "$nic" "$present" <<<"$nmrows" || true
  else
    printf 'FAIL|(nic-nm) nmcli is not installed -- the NetworkManager pinning cannot be read\n'
  fi
  return 0
}

# strih_nic_grade_report SYSROOT IP_ADDR_TEXT -> print verify-strih item 36 through the CALLER's ok / bad /
# note functions (one per row). rc 1 when the grader printed no row at all -- a silent grader is a FAIL.
strih_nic_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_nic_grade_rows "$@")
  [ "$rows" -gt 0 ]
}
