#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/strih-bundle-install.sh -- issue 1412: how setup-strih step 4 puts the genlock bundle
# into its /usr prefix (strih_install_bundle_prefix in strih-provision.sh calls these).
#
# Why (stream-pp, 11.10.2026): the prefix install used `cp -a` into /usr/lib/x86_64-linux-gnu. cp
# rewrites an existing file on the SAME inode, so the libraries a running OBS and its CEF children had
# mapped changed under them: every obs-browser-page dumped core and OBS died with status 139. One run
# also failed outright with `Text file busy`, because the kernel refuses a write to an executing ELF.
# So a file is never written in place: an unchanged one is left alone, a changed one goes through a
# temp sibling + rename (a NEW inode; the running process keeps the one it mapped).
#
# A second live finding the same day: a bundle unpacked without its unix modes left the CEF helper
# obs-browser-page at 0664, and `chmod a+rX` never adds an x bit that is not already there, so CEF
# crash-looped OBS (exit 133). The bundle's executables are therefore set 0755 by NAME.

# strih_bundle_lib_executables -> the bundle's executables under its lib dir (lib/x86_64-linux-gnu),
# relative paths, one per line. obs-browser-page is the CEF helper OBS execs from its plugin dir
# (vendor/obs-studio/plugins/obs-browser/cmake/os-linux.cmake installs it there). chrome-sandbox is
# not listed: setup-strih step 4 sets it root:root 4755 itself (the CEF SUID sandbox, issue 1317 F6).
# The bin/ helpers (obs, obs-ffmpeg-mux, obs-nvenc-test) are installed 0755 by strih_install_bundle_prefix.
strih_bundle_lib_executables() {
  printf '%s\n' "obs-plugins/obs-browser-page"
}

# strih_bundle_tree_install SRC DST -> mirror SRC's tree into DST without ever writing into an existing
# file. An unchanged file or symlink is left alone (same inode, same mtime). A changed or new file is
# copied to `.<name>.cb-new.<pid>` in its own directory, made a+rX and renamed over the name (rename(2)
# is one step: a reader sees the old file or the new one, never a half). A changed symlink goes the same
# way; a directory is created 0755. A path whose type differs from SRC's (a file where a directory goes,
# a symlinked directory in DST) is refused, nothing guessed. Prints one summary line; sets
# STRIH_BUNDLE_TREE_CHANGED to the number of names it wrote. rc 1 + a stderr line on a failure.
strih_bundle_tree_install() {
  local src="${1:?source dir required}" dst="${2:?destination dir required}"
  local rel s d tmp tgt same=0 changed=0
  # shellcheck disable=SC2034  # read by the caller (strih_install_bundle_prefix in strih-provision.sh)
  STRIH_BUNDLE_TREE_CHANGED=0
  [ -d "$src" ] || { printf 'strih_bundle_tree_install: %s is not a directory\n' "$src" >&2; return 1; }
  mkdir -p "$dst" || { printf 'strih_bundle_tree_install: cannot create %s\n' "$dst" >&2; return 1; }
  while IFS= read -r -d '' rel; do
    s="${src}/${rel}"
    d="${dst}/${rel}"
    tmp="${d%/*}/.${d##*/}.cb-new.$$"
    if [ -L "$s" ]; then
      tgt="$(readlink "$s")"
      if [ -L "$d" ] && [ "$(readlink "$d")" = "$tgt" ]; then
        same=$((same + 1))
        continue
      fi
      if [ -d "$d" ] && [ ! -L "$d" ]; then
        printf 'strih_bundle_tree_install: %s is a directory, the bundle has a symlink there -- refusing\n' "$d" >&2
        return 1
      fi
      rm -f "$tmp"
      if ! { ln -s "$tgt" "$tmp" && mv -fT "$tmp" "$d"; }; then
        rm -f "$tmp"
        printf 'strih_bundle_tree_install: could not install the symlink %s\n' "$d" >&2
        return 1
      fi
      changed=$((changed + 1))
    elif [ -d "$s" ]; then
      if [ -L "$d" ] || { [ -e "$d" ] && [ ! -d "$d" ]; }; then
        printf 'strih_bundle_tree_install: %s is not a plain directory -- refusing\n' "$d" >&2
        return 1
      fi
      if [ ! -d "$d" ]; then
        mkdir -m 0755 "$d" || { printf 'strih_bundle_tree_install: cannot create %s\n' "$d" >&2; return 1; }
      fi
    elif [ -f "$s" ]; then
      if [ -f "$d" ] && [ ! -L "$d" ] && cmp -s -- "$s" "$d"; then
        same=$((same + 1))
        continue
      fi
      if [ -d "$d" ] && [ ! -L "$d" ]; then
        printf 'strih_bundle_tree_install: %s is a directory, the bundle has a file there -- refusing\n' "$d" >&2
        return 1
      fi
      rm -f "$tmp"
      if ! { cp --preserve=mode,timestamps -- "$s" "$tmp" && chmod a+rX "$tmp" && mv -fT "$tmp" "$d"; }; then
        rm -f "$tmp"
        printf 'strih_bundle_tree_install: could not install %s\n' "$d" >&2
        return 1
      fi
      changed=$((changed + 1))
    fi
  done < <(cd "$src" && find . -mindepth 1 -printf '%P\0')
  # shellcheck disable=SC2034  # read by the caller (strih_install_bundle_prefix in strih-provision.sh)
  STRIH_BUNDLE_TREE_CHANGED="$changed"
  printf '  %s: %d unchanged, %d written (temp sibling + rename, never in place)\n' "$dst" "$same" "$changed"
}

# strih_bundle_exec_fix LIBDIR -> chmod 0755 every strih_bundle_lib_executables file present under
# LIBDIR (an existing inode's mode only; a running process is unaffected). rc 1 on a failed chmod.
strih_bundle_exec_fix() {
  local libdir="${1:?libdir required}" rel
  while IFS= read -r rel; do
    [ -f "${libdir}/${rel}" ] || continue
    chmod 0755 "${libdir}/${rel}" || { printf 'strih_bundle_exec_fix: chmod 0755 %s failed\n' "${libdir}/${rel}" >&2; return 1; }
  done < <(strih_bundle_lib_executables)
}

# strih_bundle_exec_grade LIBDIR BROWSER_ON -> verify-strih item 20's executables row, through the
# caller's ok / bad. Every listed executable present under LIBDIR must be executable; with BROWSER_ON=1
# (the installed STRIH_BUILD_FLAGS.txt says BROWSER-ON) obs-browser-page must also be present.
# Always rc 0.
strih_bundle_exec_grade() {
  local libdir="${1:?libdir required}" browser="${2:-0}" rel bad_list=""
  while IFS= read -r rel; do
    if [ -f "${libdir}/${rel}" ]; then
      [ -x "${libdir}/${rel}" ] || bad_list="${bad_list:+$bad_list, }${rel} not executable ($(stat -c '%a' "${libdir}/${rel}" 2>/dev/null || echo '?'))"
    elif [ "$browser" = 1 ]; then
      bad_list="${bad_list:+$bad_list, }${rel} missing"
    fi
  done < <(strih_bundle_lib_executables)
  if [ -z "$bad_list" ]; then
    ok "(bundle-exec) the bundle's executables under ${libdir} are executable ($(strih_bundle_lib_executables | tr '\n' ' ' | sed 's/ $//'))"
  else
    bad "(bundle-exec) ${bad_list} under ${libdir} -- CEF cannot launch it and OBS crash-loops (exit 133); re-run setup-strih.sh step 4 (issue 1412)"
  fi
  return 0
}
