#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into it.
#
# scripts/lib/strih-user-home.sh -- issue 1412: directories setup-strih (root) creates in the desktop
# user's home belong to that user, every level of them.
#
# `install -d -o U -g U a/b/c` gives the owner to the LAST component only: every missing parent is
# created root:root. On the fresh PP boxes (11.10.2026) ~/.config (strih-pp), ~/.config/systemd,
# ~/.config/wireplumber and ~/.config/pipewire came out root-owned, and Companion Satellite (Electron)
# crash-looped 183 times on `Failed to get 'userData' path`: it could not create
# ~/.config/companion-satellite. Never a recursive chown of the home: only the levels on the path.

# strih_user_home_dir HOME USER DIR -> create DIR and every missing level between HOME and DIR (0755),
# each chowned to USER (`USER:` = its login group), one level at a time. An existing level on that path
# not owned by USER (an earlier root-run left it root:root) is chowned back, that directory only, and
# named. HOME itself is never touched. A DIR outside HOME, a `.`/`..` component, a symlinked level or a
# non-directory on the path refuses (a HOME that does not exist yet is created as USER's) (root never follows a link in a user-writable tree). rc 1 + a stderr
# line on any refusal or failure.
strih_user_home_dir() {
  local home="${1:?home required}" user="${2:?user required}" dir="${3:?dir required}"
  local uid rel cur part owner
  local -a parts
  home="${home%/}"
  dir="${dir%/}"
  case "$dir" in
    "$home"/?*) ;;
    *) echo "strih-user-home: ${dir} is not under ${home}" >&2; return 1 ;;
  esac
  uid="$(id -u "$user")" || { echo "strih-user-home: unknown user ${user}" >&2; return 1; }
  if [ ! -e "$home" ] && [ ! -L "$home" ]; then
    # a home that does not exist yet (a fresh account, a test root) is created as the user's
    mkdir -p "$home" && chmod 0755 "$home" || { echo "strih-user-home: cannot create ${home}" >&2; return 1; }
    chown "${user}:" "$home" || { echo "strih-user-home: chown ${user} ${home} failed" >&2; return 1; }
  fi
  if [ ! -d "$home" ] || [ -L "$home" ]; then
    echo "strih-user-home: ${home} is not a plain directory" >&2
    return 1
  fi
  rel="${dir#"$home"/}"
  IFS=/ read -r -a parts <<<"$rel"
  cur="$home"
  for part in "${parts[@]}"; do
    [ -n "$part" ] || continue
    case "$part" in
      .|..) echo "strih-user-home: ${dir} has a '${part}' component -- refusing" >&2; return 1 ;;
    esac
    cur="${cur}/${part}"
    if [ -L "$cur" ]; then
      echo "strih-user-home: ${cur} is a symlink -- refusing (remove it and re-run)" >&2
      return 1
    fi
    if [ ! -e "$cur" ]; then
      mkdir -m 0755 "$cur" || { echo "strih-user-home: cannot create ${cur}" >&2; return 1; }
      chown "${user}:" "$cur" || { echo "strih-user-home: chown ${user} ${cur} failed" >&2; return 1; }
    elif [ ! -d "$cur" ]; then
      echo "strih-user-home: ${cur} is not a directory -- refusing" >&2
      return 1
    else
      owner="$(stat -c '%u' "$cur")" || { echo "strih-user-home: cannot stat ${cur}" >&2; return 1; }
      if [ "$owner" != "$uid" ]; then
        chown "${user}:" "$cur" || { echo "strih-user-home: chown ${user} ${cur} failed" >&2; return 1; }
        echo "  ${cur}: owner uid ${owner} -> ${user} (issue 1412)"
      fi
    fi
  done
}

# strih_home_ownership_grade HOME [BAD_UID] -> verify-strih's (home-owner) row through the caller's
# ok / bad: nothing under HOME/.config may be owned by BAD_UID (default 0, root). An absent .config
# passes (nothing created yet). Names at most 5 paths. Always rc 0.
strih_home_ownership_grade() {
  local home="${1:?home required}" bad_uid="${2:-0}" list n
  home="${home%/}"
  if [ ! -d "${home}/.config" ]; then
    ok "(home-owner) ${home}/.config absent -- nothing to grade"
    return 0
  fi
  list="$(find "${home}/.config" -xdev -uid "$bad_uid" -print 2>/dev/null || true)"
  if [ -z "$list" ]; then
    ok "(home-owner) nothing under ${home}/.config is owned by uid ${bad_uid}"
    return 0
  fi
  n="$(printf '%s\n' "$list" | wc -l)"
  bad "(home-owner) ${n} path(s) under ${home}/.config owned by uid ${bad_uid}, e.g. $(printf '%s\n' "$list" | sed -n '1,5p' | tr '\n' ' ')-- the desktop apps cannot write there (Companion Satellite crash-loops); re-run setup-strih.sh, or chown those paths (non-recursively) to the desktop user (issue 1412)"
  return 0
}
