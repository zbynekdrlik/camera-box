#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the shared NDI-runtime install-command emitter, no
# top-level statements) -- matches the sibling scripts/lib/*.sh convention (strih-provision.sh,
# obs-fleet.sh, camera-set.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this
# file executes it in the CALLER's shell, so strict mode here would leak into whichever caller
# sources it. Each caller (setup-imag.sh / setup-strih.sh) sets its own strict mode.
#
# scripts/lib/ndi-runtime.sh -- issue 1317: the ONE place the fleet NDI 6.3.2 runtime install
# recipe lives, so setup-imag.sh (step 10) and setup-strih.sh (its NDI step) share ONE source of
# truth instead of a drifting copy (the #591/#595/#596 "share the recipe, not a copy" lesson).
# It EMITS the idempotent on-box statements as text (the scripts/lib/strih-provision.sh
# `strih_lx_chrome_sandbox_fix_cmd` pattern) so a caller runs `eval "$(ndi_runtime_install_cmds ...)"`
# and tests/ndi_runtime_lib.rs can assert the emitted recipe rustc-free (Tier-0).
#
# WHY DistroAV needs all of it (live-proven on imag-nb + strih-lx): DistroAV's Linux loader
# (vendor/distroav src/plugin-main.cpp load_ndilib) scans ONLY /usr/lib, /usr/lib64 and
# /usr/local/lib (non-recursive -- NOT the multiarch dir, NOT the ld.so cache) for libndi.so.<N>.
# Without the /usr/local/lib/libndi.so.6 symlink the plugin loads UI-only with ERR-404; without
# avahi-daemon NDI find() returns nothing (mDNS discovery); and a 0600-perms copy gives the desktop
# obs user `Permission denied` at dlopen (the issue-1236 root:root a+rX perms-normalize).

# ndi_runtime_install_cmds NDI_PEER CAM_PW [SCP_USER] [NDI_DIR] -> print the idempotent on-box bash
# statements that install the fleet-identical NDI 6.3.2 runtime. NDI_PEER = a live cam box to copy
# libndi from; CAM_PW = that box's ssh password (used only when the runtime is not already present);
# SCP_USER = the ssh login (default newlevel); NDI_DIR = the runtime dir (default /usr/lib/ndi).
# The emitted recipe:
#   * copies libndi.so.*.*.* from NDI_PEER into NDI_DIR ONLY when it is not already there (idempotent),
#     then makes the libndi.so.6 + libndi.so symlinks;
#   * normalizes the runtime files root:root a+rX (unconditional, so a re-run also fixes a bad 0600
#     copy) -- the issue-1236 perms shape;
#   * writes /etc/ld.so.conf.d/ndi.conf + ldconfig, with NO `grep -q` on the pipe (`-q`'s early close
#     SIGPIPEs ldconfig under the caller's `set -o pipefail` -- the step-4 ldconfig footgun);
#   * creates the /usr/local/lib/libndi.so.6 symlink DistroAV's Linux loader scans;
#   * installs + enables avahi-daemon (mDNS NDI-source discovery).
# Peer/pw/user are baked in %q-quoted; the glob's `*` are single-quoted so the LOCAL shell never
# expands them (scp's remote shell does). On a copy / linker-cache failure the emitted recipe writes
# a message to stderr and `exit 1`s, so a caller runs it as `( eval "$(...)" ) || fail "..."`.
ndi_runtime_install_cmds() {
  local peer="${1:?ndi peer required}" pw="${2-}" user="${3:-newlevel}" ndir="${4:-/usr/lib/ndi}"
  local q_peer q_pw q_user q_ndir
  q_peer="$(printf '%q' "$peer")"
  q_pw="$(printf '%q' "$pw")"
  q_user="$(printf '%q' "$user")"
  q_ndir="$(printf '%q' "$ndir")"
  # NOTE: pw is allowed to be EMPTY (${2-}, no colon) -- an idempotent re-run with the runtime
  # already present does not need it, so the whole recipe (incl. the unconditional perms/ldconfig/
  # symlink/avahi tail below) must still emit. CAM_PW is required ONLY for the fetch, so the guard
  # lives INSIDE the runtime-absent branch (matching setup-imag's old inline block, which ran the
  # tail unconditionally on a re-run).
  printf '%s\n' \
    "__ndir=${q_ndir}" \
    'if [ ! -e "$__ndir/libndi.so.6" ]; then' \
    "  [ -n ${q_pw} ] || { echo 'ndi-runtime: CAM_PW required to fetch the NDI runtime from the cambox peer' >&2; exit 1; }" \
    '  command -v sshpass >/dev/null 2>&1 || apt-get install -y sshpass >/dev/null' \
    '  mkdir -p "$__ndir"' \
    "  sshpass -p ${q_pw} scp -O -o StrictHostKeyChecking=no ${q_user}@${q_peer}:/usr/lib/ndi/libndi.so.'*.*.*' \"\$__ndir/\" || { echo 'ndi-runtime: NDI runtime copy from the cambox peer failed' >&2; exit 1; }" \
    '  ( cd "$__ndir" && __real="$(ls libndi.so.*.*.* | head -1)" && ln -sf "$__real" libndi.so.6 && ln -sf libndi.so.6 libndi.so )' \
    'fi' \
    'chown root:root "$__ndir"/libndi.so* 2>/dev/null || true' \
    'chmod a+rX "$__ndir"/libndi.so* 2>/dev/null || true' \
    'echo "$__ndir" > /etc/ld.so.conf.d/ndi.conf' \
    'ldconfig' \
    'ldconfig -p | grep libndi >/dev/null || { echo "ndi-runtime: libndi not in linker cache" >&2; exit 1; }' \
    'ln -sf "$(readlink -f "$__ndir"/libndi.so.6)" /usr/local/lib/libndi.so.6' \
    'apt-get install -y avahi-daemon >/dev/null 2>&1 || true' \
    'systemctl enable --now avahi-daemon >/dev/null 2>&1'
}
