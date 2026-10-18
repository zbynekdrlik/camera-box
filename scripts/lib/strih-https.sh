#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih HTTPS front-end constants, the setup-strih step
# 16i install and the verify-strih item 45 grader; no top-level statements besides constants and the
# lib sources) -- the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here:
# sourcing runs in the CALLER's shell, so strict mode would leak into it. setup-strih.sh /
# verify-strih.sh set their own. Every function checks its own return codes.
#
# scripts/lib/strih-https.sh -- issue 1399 (owner 1.10.2026, during a production: "MA TO BYT
# SAMOBEZIACE NA STRIH NB NEMA TO BYT ZAVISLE NA INOM PC AKO DEV1"), design 6142127441: every strih box
# serves its OWN intercom phone page and shading panel over HTTPS, with certificates issued and renewed
# ON the box. dev1's nginx front (scripts/dev1-shading-https-install.sh) is retired for these names once
# a box serves them; this lib reuses its pure renderers (scripts/lib/shading-https.sh) with per-box
# parameters.
#
#   * The facts (scripts/strih-boxes/<box>.env, validated by scripts/lib/strih-box-facts.sh):
#     STRIH_HTTPS_INTERKOM_HOSTS = the intercom certificate's first name + its SANs, STRIH_HTTPS_SHADING_HOST
#     = the bkshading panel's name, `none` = no such site (the stream role declares none).
#   * The sites (`strih_https_sites`), with LOCAL upstreams: interkom `/` -> the hub on 127.0.0.1:8790
#     (the box's intercom matrix [http] bind) + `/janus` -> the same hub (issue 1453: its /janus relay
#     forwards only the phone page's own frames to Janus, whose WebSocket listens on loopback only, so no
#     client can send Janus a plain-RTP join); shading `/` -> bkshading on 127.0.0.1:8770. Written to
#     /etc/nginx/sites-available/{interkom,shading} (the hot-fix names, so the hand sites are replaced
#     in place) and enabled by a sites-enabled symlink.
#   * The certificate per site: /etc/letsencrypt/live/<first name>/, issued by `certbot certonly
#     --dns-cloudflare --cert-name <first name>` ONLY when it is missing or its SAN set differs
#     (`strih_https_cert_action`). Certbot reads /etc/letsencrypt/cloudflare.ini (mode 600, root).
#     Until a live certificate exists the site keeps the hand-copied /etc/nginx/ssl pair the installed
#     site already names (`strih_https_cert_paths`), so a box keeps serving during the move.
#   * THE TOKEN: this lib never reads, prints or writes the cloudflare.ini content. A missing ini is ONE
#     WARN naming the supervisor step (the issue-1399 runbook: the zone-scoped DNS token piped from
#     dev1's secret store over ssh stdin, through `airuleset.py secret exec`). A present ini only gets
#     its mode (600) and, as root, its owner (root) fixed.
#   * DNS records are never touched here: the supervisor moves them with its own channel.
#
#   * strih_https_install -- setup-strih step 16i (as root): the packages nginx-light certbot
#     python3-certbot-dns-cloudflare with --no-upgrade, only when missing; the ini mode; the renewal
#     deploy hook (reloads nginx); the certificates; the two sites, each written only on a difference
#     (the old file kept aside); `nginx -t` BEFORE any reload or start -- a failing test restores the
#     old files and reloads nothing; nginx enabled + reloaded (or started); certbot.timer enabled. A
#     failing certbot run is a WARN, never a failed step: the site keeps the certificate it has, every
#     strih genlock deploy re-runs setup-strih, and verify item 45 grades the truth.
#   * strih_https_grade_rows / _report -- verify-strih item 45:
#       (https-nginx)    nginx active, :443 listening, every declared site equal to its render and enabled;
#       (https-cert)     one row per declared NAME: the certificate nginx serves for that SNI on
#                        127.0.0.1:443 covers the name, is valid for more than 14 days, and is the
#                        /etc/letsencrypt/live one (a hand copy is a NOTE until migrated);
#       (https-upstream) one row per site, through nginx: the hub /api/state = 200 and the /janus
#                        WebSocket upgrade = 101 (the hub answers it only once it reached Janus), and
#                        Janus's WebSocket (:8188) listening on loopback only; bkshading /api/version = 200;
#       (https-renewal)  certbot.timer enabled, the deploy hook in place, the ini present (stat only:
#                        mode 600, owner root);
#     a box that declares no site prints ONE NOTE row.
#
# Test seams (tests/python/test_strih_https_1399.py): STRIH_HTTPS_NGINX_DIR (/etc/nginx),
# STRIH_HTTPS_LE_DIR (/etc/letsencrypt -- certbot itself always uses its default), STRIH_HTTPS_INI_OWNER
# (root); apt-get, dpkg-query, systemctl, nginx, certbot, openssl, curl and ss are looked up on PATH.

if ! declare -F shading_https_site_content >/dev/null; then
  # shellcheck source=scripts/lib/shading-https.sh
  . "$(dirname "${BASH_SOURCE[0]}")/shading-https.sh"
fi
# The write-if-changed installer lives in the session-apps lib.
if ! declare -F strih_session_apps_put >/dev/null; then
  # shellcheck source=scripts/lib/strih-session-apps.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-session-apps.sh"
fi

STRIH_HTTPS_PACKAGES=(nginx-light certbot python3-certbot-dns-cloudflare)
STRIH_HTTPS_HUB_UPSTREAM="http://127.0.0.1:8790"
STRIH_HTTPS_SHADING_UPSTREAM="http://127.0.0.1:8770"
STRIH_HTTPS_JANUS_PORT=8188
STRIH_HTTPS_MIN_DAYS=14

strih_https_nginx_dir() { printf '%s' "${STRIH_HTTPS_NGINX_DIR:-/etc/nginx}"; }
strih_https_le_dir() { printf '%s' "${STRIH_HTTPS_LE_DIR:-/etc/letsencrypt}"; }
strih_https_ini() { printf '%s/cloudflare.ini' "$(strih_https_le_dir)"; }
strih_https_hook_path() { printf '%s/renewal-hooks/deploy/nginx-reload.sh' "$(strih_https_le_dir)"; }

# strih_https_hook_text -> the renewal deploy hook (shading_https_deploy_hook_content, this box's text).
strih_https_hook_text() {
  shading_https_deploy_hook_content "intercom + shading certificates of this strih box" \
    "setup-strih.sh step 16i (scripts/lib/strih-https.sh)" 1399
}

# strih_https_sites -> one line per declared site of the loaded box, `site|first|aliases|upstream|janus`
# (interkom first, then shading); nothing when the box declares none. rc 1 when a fact is unreadable.
# The interkom `janus` upstream is the hub itself (issue 1453): its /janus route relays the phone's
# Janus WebSocket through the frame allowlist, and Janus's own WebSocket is never proxied to.
strih_https_sites() {
  local hosts shading first aliases=""
  hosts="$(strih_lx_https_interkom_hosts)" || return 1
  shading="$(strih_lx_https_shading_host)" || return 1
  if [ "$hosts" != none ]; then
    first="${hosts%% *}"
    [ "$hosts" = "$first" ] || aliases="${hosts#* }"
    printf 'interkom|%s|%s|%s|%s\n' "$first" "$aliases" "$STRIH_HTTPS_HUB_UPSTREAM" "$STRIH_HTTPS_HUB_UPSTREAM"
  fi
  if [ "$shading" != none ]; then
    printf 'shading|%s||%s|\n' "$shading" "$STRIH_HTTPS_SHADING_UPSTREAM"
  fi
  return 0
}

# strih_https_site_text SITE FIRST ALIASES UPSTREAM JANUS CERT KEY -> the nginx site of this box: the
# shading-https renderer with the box's header, its local upstreams and the given certificate pair.
strih_https_site_text() {
  local site="$1" first="$2" aliases="$3" up="$4" janus="$5" cert="$6" key="$7"
  local box ip fronted extra="" header
  box="$(strih_lx_hostname)" || return 1
  ip="$(strih_lx_ip)" || return 1
  case "$site" in
    interkom) fronted="the intercom hub + phone PWA"; extra="$(interkom_https_janus_location "$janus")" ;;
    shading) fronted="the bkshading shading panel" ;;
    *) echo "strih-https: unknown site '${site}'" >&2; return 1 ;;
  esac
  header="# /etc/nginx/sites-available/${site} -- the LAN HTTPS front for ${fronted} ON ${box}
# (camera-box issue 1399). Rendered by scripts/lib/strih-https.sh over scripts/lib/shading-https.sh
# (shading_https_site_content); installed by setup-strih.sh step 16i, graded by verify-strih.sh item 45.
# NO secrets in this file.
#
# Phone / PC on the venue LAN -> DNS ${first} = ${ip} (this box, DNS-only / not proxied) -> nginx :443
# (Let's Encrypt cert, DNS-01, renewed on this box) -> ${up}. Nothing here depends on dev1."
  shading_https_site_content "$first" "$up" "$extra" "$fronted" "$fronted" "$aliases" "$cert" "$key" "$header"
}

# strih_https_san_names (stdin: `openssl x509 -ext subjectAltName` output) -> the DNS names, one per line.
strih_https_san_names() {
  grep -o 'DNS:[^, ]*' | sed 's/^DNS://' || true
}

# strih_https_cert_action WANT HAVE -> issue | reissue | current (no newline). WANT / HAVE are
# space-separated name lists; an empty HAVE (no live certificate) = issue, a different SET = reissue.
strih_https_cert_action() {
  local -a w h
  IFS=' ' read -r -a w <<<"${1-}"
  IFS=' ' read -r -a h <<<"${2-}"
  if [ "${#h[@]}" = 0 ]; then
    printf 'issue'
  elif [ "$(printf '%s\n' "${w[@]}" | LC_ALL=C sort -u)" = "$(printf '%s\n' "${h[@]}" | LC_ALL=C sort -u)" ]; then
    printf 'current'
  else
    printf 'reissue'
  fi
}

# strih_https_name_covered NAME SANS -> rc 0 iff SANS (space-separated) covers NAME: the exact name, or a
# `*.<rest>` wildcard over exactly one leading label.
strih_https_name_covered() {
  local name="${1-}" s
  local -a sans
  IFS=' ' read -r -a sans <<<"${2-}"
  for s in "${sans[@]}"; do
    [ "$s" = "$name" ] && return 0
    if [ "${s#\*.}" != "$s" ] && [ "${name#*.}" = "${s#\*.}" ] && [ "${name%%.*}" != "$name" ]; then
      return 0
    fi
  done
  return 1
}

# strih_https_certbot_argv FIRST ALIASES -> the certbot argv, one token per line: shading-https's
# DNS-01 invocation through this box's ini, always keyed --cert-name FIRST.
strih_https_certbot_argv() {
  local first="${1:?first name required}" aliases="${2-}"
  shading_https_certbot_argv "$first" "$(shading_https_email)" "$(strih_https_ini)" \
    "$(shading_https_propagation_seconds)" "$aliases" || return 1
  [ -n "$aliases" ] || printf '%s\n' --cert-name "$first"
}

# strih_https_live_names FIRST -> the SAN names of /etc/letsencrypt/live/FIRST/fullchain.pem, space-
# separated; empty when there is no readable certificate. Always rc 0.
strih_https_live_names() {
  local pem
  pem="$(strih_https_le_dir)/live/${1-}/fullchain.pem"
  [ -f "$pem" ] || return 0
  { openssl x509 -noout -ext subjectAltName -in "$pem" 2>/dev/null || true; } | strih_https_san_names | tr '\n' ' ' | sed 's/ $//'
}

# _strih_https_file_declares FILE NAME -> rc 0 iff an nginx FILE carries NAME in a server_name line.
_strih_https_file_declares() {
  local file="$1" name="$2" line n
  local -a names
  [ -f "$file" ] || return 1
  while IFS= read -r line; do
    IFS=' ' read -r -a names <<<"${line//$'\t'/ }"
    for n in "${names[@]}"; do [ "$n" = "$name" ] && return 0; done
  done < <(sed -n '/^[[:space:]]*#/d; s/.*server_name[[:space:]]\{1,\}\([^;]*\);.*/\1/p' "$file" 2>/dev/null || true)
  return 1
}

# _strih_https_ssl_pair FILE -> `cert|key` of FILE's first ssl_certificate / ssl_certificate_key lines
# when BOTH resolve (realpath -e: no `..`, no symlink out) to files under <nginx dir>/ssl/; nothing
# otherwise. Always rc 0.
_strih_https_ssl_pair() {
  local file="$1" ssl cert key rc rk
  [ -f "$file" ] || return 0
  ssl="$(realpath -e "$(strih_https_nginx_dir)/ssl" 2>/dev/null)" || return 0
  cert="$(sed -n 's/^[[:space:]]*ssl_certificate[[:space:]]\{1,\}\([^;[:space:]]*\);.*/\1/p;T;q' "$file" 2>/dev/null || true)"
  key="$(sed -n 's/^[[:space:]]*ssl_certificate_key[[:space:]]\{1,\}\([^;[:space:]]*\);.*/\1/p;T;q' "$file" 2>/dev/null || true)"
  [ -n "$cert" ] && [ -n "$key" ] || return 0
  rc="$(realpath -e "$cert" 2>/dev/null)" || return 0
  rk="$(realpath -e "$key" 2>/dev/null)" || return 0
  case "$rc" in "$ssl"/*) ;; *) return 0 ;; esac
  case "$rk" in "$ssl"/*) ;; *) return 0 ;; esac
  [ -f "$rc" ] && [ -f "$rk" ] && printf '%s|%s' "$cert" "$key"
  return 0
}

# strih_https_cert_paths SITE FIRST -> `cert|key|origin`: origin `live` = /etc/letsencrypt/live/FIRST;
# `hand` = the /etc/nginx/ssl pair the installed SITE file (else an enabled site declaring FIRST)
# names; `none` = neither (cert and key empty). Always rc 0.
strih_https_cert_paths() {
  local site="$1" first="$2" live nginx f pair=""
  live="$(strih_https_le_dir)/live/${first}"
  if [ -f "${live}/fullchain.pem" ] && [ -f "${live}/privkey.pem" ]; then
    printf '%s|%s|live' "${live}/fullchain.pem" "${live}/privkey.pem"
    return 0
  fi
  nginx="$(strih_https_nginx_dir)"
  pair="$(_strih_https_ssl_pair "${nginx}/sites-available/${site}")"
  if [ -z "$pair" ]; then
    for f in "${nginx}"/sites-enabled/*; do
      [ "$(basename "$f")" = "$site" ] && continue
      _strih_https_file_declares "$f" "$first" || continue
      pair="$(_strih_https_ssl_pair "$f")"
      [ -n "$pair" ] && break
    done
  fi
  if [ -n "$pair" ]; then printf '%s|hand' "$pair"; else printf '||none'; fi
}

# strih_https_missing_packages -> the packages to install, one per line (nginx-light is satisfied by
# an installed nginx too). Always rc 0.
strih_https_missing_packages() {
  local p alt
  for p in "${STRIH_HTTPS_PACKAGES[@]}"; do
    [ "$(dpkg-query -W -f='${Status}' "$p" 2>/dev/null || true)" = "install ok installed" ] && continue
    if [ "$p" = nginx-light ]; then
      alt="$(dpkg-query -W -f='${Status}' nginx 2>/dev/null || true)"
      [ "$alt" = "install ok installed" ] && continue
    fi
    printf '%s\n' "$p"
  done
  return 0
}

# --- setup-strih step 16i -------------------------------------------------------------------------------

# _strih_https_restore TMPDIR -> put back every site file / symlink the install changed (the record in
# TMPDIR/changed: `<path>|<backup or ->`). Always rc 0.
_strih_https_restore() {
  local tmp="$1" path backup
  [ -f "${tmp}/changed" ] || return 0
  while IFS='|' read -r path backup; do
    if [ "$backup" = - ]; then
      rm -f "$path"
    else
      rm -f "$path" && cp -P -p "$backup" "$path"
    fi
    echo "  restored ${path}"
  done < "${tmp}/changed"
  return 0
}

# _strih_https_keep TMPDIR PATH -> record PATH's current state before it is changed (once per path).
_strih_https_keep() {
  local tmp="$1" path="$2" n
  grep -qF -- "${path}|" "${tmp}/changed" 2>/dev/null && return 0
  if [ -e "$path" ] || [ -L "$path" ]; then
    n="$(wc -l < "${tmp}/changed" 2>/dev/null || echo 0)"
    cp -P -p "$path" "${tmp}/keep.${n}" || return 1
    printf '%s|%s\n' "$path" "${tmp}/keep.${n}" >> "${tmp}/changed"
  else
    printf '%s|-\n' "$path" >> "${tmp}/changed"
  fi
}

# strih_https_install -> see the header. One line per action. rc 1 + a stderr line on a package that
# will not install, a failed write, a failing `nginx -t` (the old sites restored) or a failed systemctl
# call; a missing ini / certificate and a failing certbot are WARNs.
strih_https_install() {
  local sites ini hook nginx avail enabled tmp site first aliases up janus want have action w text
  local paths cert key origin dst link f h act en missing mode sites_changed=0 reload=0
  local -a argv hosts
  sites="$(strih_https_sites)" || return 1
  if [ -z "$sites" ]; then
    echo "  this box declares no HTTPS site (STRIH_HTTPS_INTERKOM_HOSTS / STRIH_HTTPS_SHADING_HOST = none) -- nothing to provision"
    return 0
  fi
  ini="$(strih_https_ini)"
  hook="$(strih_https_hook_path)"
  nginx="$(strih_https_nginx_dir)"
  avail="${nginx}/sites-available"
  enabled="${nginx}/sites-enabled"

  missing="$(strih_https_missing_packages)"
  if [ -n "$missing" ]; then
    echo "  installing ${missing//$'\n'/ } (--no-upgrade: nothing OBS uses is upgraded)"
    # shellcheck disable=SC2086  # one word per package name
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-upgrade $missing || {
      echo "strih-https: apt-get install ${missing//$'\n'/ } failed" >&2; return 1; }
  else
    echo "  packages present (${STRIH_HTTPS_PACKAGES[*]})"
  fi

  # The DNS token file: never read here. Only its mode (and, as root, its owner) is fixed.
  if [ -f "$ini" ]; then
    mode="$(stat -c %a "$ini" 2>/dev/null || true)"
    if [ "$mode" != 600 ]; then
      chmod 600 "$ini" || { echo "strih-https: cannot chmod 600 ${ini}" >&2; return 1; }
      echo "  ${ini} mode ${mode:-?} -> 600"
    fi
    if [ "$(id -u)" = 0 ] && [ "$(stat -c %U:%G "$ini" 2>/dev/null || true)" != root:root ]; then
      chown root:root "$ini" || { echo "strih-https: cannot chown root:root ${ini}" >&2; return 1; }
      echo "  ${ini} owner -> root:root"
    fi
    echo "  DNS token file ${ini} present (mode 600; its content is never read here)"
  else
    echo "  WARN: ${ini} missing -- no certificate can be issued or renewed on this box; supervisor step (the issue 1399 runbook): place the zone-scoped DNS token there through the secret exec channel, then re-run setup-strih.sh step 16i" >&2
  fi

  tmp="$(mktemp -d)" || { echo "strih-https: mktemp failed" >&2; return 1; }
  install -d -m 0755 "$(dirname "$hook")" "$avail" "$enabled" || {
    rm -rf "$tmp"; echo "strih-https: cannot create $(dirname "$hook") / ${avail} / ${enabled}" >&2; return 1; }
  strih_https_hook_text > "${tmp}/hook" || { rm -rf "$tmp"; return 1; }
  w="$(strih_session_apps_put "${tmp}/hook" "$hook" 0755)" || { rm -rf "$tmp"; return 1; }
  echo "  ${hook} ${w} (reloads nginx after a renewal)"

  # The certificates: issued or re-issued only when missing or the SAN set differs.
  while IFS='|' read -r site first aliases up janus; do
    want="${first}${aliases:+ ${aliases}}"
    have="$(strih_https_live_names "$first")"
    action="$(strih_https_cert_action "$want" "$have")"
    if [ "$action" = current ]; then
      echo "  certificate ${first}: current (${want})"
    elif [ ! -f "$ini" ]; then
      echo "  certificate ${first}: ${action} needed -- waiting for the DNS token (the WARN above)"
    else
      text="$(strih_https_certbot_argv "$first" "$aliases")" || { rm -rf "$tmp"; return 1; }
      mapfile -t argv <<<"$text"
      echo "  certificate ${first}: ${action} (${want}) -- certbot DNS-01"
      if certbot "${argv[@]}" </dev/null; then
        # a `certonly` fires no renewal deploy hook: nginx must load the new certificate itself, also
        # when the site text is unchanged (it already names the live dir)
        reload=1
        echo "  certificate ${first}: issued"
      else
        echo "  WARN: certbot could not ${action} the ${first} certificate -- the ${site} site keeps the certificate it has; verify-strih item 45 grades it" >&2
      fi
    fi
  done <<<"$sites"

  # The sites: written only on a difference, the old state kept until nginx -t passes.
  : > "${tmp}/changed"
  while IFS='|' read -r site first aliases up janus; do
    paths="$(strih_https_cert_paths "$site" "$first")"
    IFS='|' read -r cert key origin <<<"$paths"
    if [ "$origin" = none ]; then
      echo "  WARN: no certificate for ${first} yet (none in $(strih_https_le_dir)/live/${first}, no hand copy under ${nginx}/ssl) -- the ${site} site is not written" >&2
      continue
    fi
    text="$(strih_https_site_text "$site" "$first" "$aliases" "$up" "$janus" "$cert" "$key")" || { rm -rf "$tmp"; return 1; }
    printf '%s\n' "$text" > "${tmp}/${site}.new" || { rm -rf "$tmp"; return 1; }
    dst="${avail}/${site}"
    if [ -f "$dst" ] && [ ! -L "$dst" ] && cmp -s "${tmp}/${site}.new" "$dst"; then
      w=unchanged
    else
      _strih_https_keep "$tmp" "$dst" || { rm -rf "$tmp"; return 1; }
      { install -m 0644 "${tmp}/${site}.new" "${dst}.new.$$" && mv -Tf "${dst}.new.$$" "$dst"; } || {
        rm -f "${dst}.new.$$"; _strih_https_restore "$tmp"; rm -rf "$tmp"; echo "strih-https: cannot write ${dst}" >&2; return 1; }
      w=written; sites_changed=1
    fi
    echo "  ${dst} ${w} (${origin} certificate ${cert})"
    link="${enabled}/${site}"
    if [ "$(readlink "$link" 2>/dev/null || true)" != "../sites-available/${site}" ]; then
      _strih_https_keep "$tmp" "$link" || { rm -rf "$tmp"; return 1; }
      ln -sfn "../sites-available/${site}" "$link" || {
        _strih_https_restore "$tmp"; rm -rf "$tmp"; echo "strih-https: cannot enable ${link}" >&2; return 1; }
      echo "  ${link} -> ../sites-available/${site}"
      sites_changed=1
    fi
  done <<<"$sites"

  # Another enabled site that declares one of these names (a hand-made site under another name): named,
  # never removed -- nginx serves the first server_name it loads.
  read -r -a hosts <<<"$(cut -d'|' -f2,3 <<<"$sites" | tr '|\n' '  ')"
  for f in "${enabled}"/*; do
    case "$(basename "$f")" in interkom | shading) continue ;; esac
    for h in "${hosts[@]}"; do
      _strih_https_file_declares "$f" "$h" \
        && echo "  WARN: ${f} also declares ${h} -- remove it once this box's site serves the name (a duplicate server_name: nginx serves the first it loads)" >&2
    done
  done

  act="$(systemctl is-active nginx 2>/dev/null || true)"
  en="$(systemctl is-enabled nginx 2>/dev/null || true)"
  if [ "$sites_changed" = 1 ] || [ "$reload" = 1 ] || [ "$act" != active ]; then
    if ! nginx -t; then
      _strih_https_restore "$tmp"
      rm -rf "$tmp"
      echo "strih-https: nginx -t failed -- the previous sites are restored, nginx is not reloaded" >&2
      return 1
    fi
  fi
  rm -rf "$tmp"
  if [ "$act" != active ]; then
    systemctl enable --now nginx || { echo "strih-https: systemctl enable --now nginx failed" >&2; return 1; }
    echo "  enabled + started nginx"
  else
    if [ "$en" != enabled ]; then
      systemctl enable nginx || { echo "strih-https: systemctl enable nginx failed" >&2; return 1; }
    fi
    if [ "$sites_changed" = 1 ] || [ "$reload" = 1 ]; then
      systemctl reload nginx || { echo "strih-https: systemctl reload nginx failed" >&2; return 1; }
      echo "  reloaded nginx (nginx -t passed)"
    fi
  fi
  if [ "$(systemctl is-enabled certbot.timer 2>/dev/null || true)" != enabled ]; then
    systemctl enable --now certbot.timer || { echo "strih-https: systemctl enable --now certbot.timer failed" >&2; return 1; }
    echo "  enabled + started certbot.timer (renewal)"
  fi
  return 0
}

# --- verify-strih item 45 -------------------------------------------------------------------------------

# strih_https_served_pem NAME -> the first certificate nginx serves for SNI NAME on 127.0.0.1:443 (PEM),
# empty when nothing answers. Always rc 0.
strih_https_served_pem() {
  { timeout 10 openssl s_client -connect 127.0.0.1:443 -servername "${1-}" </dev/null 2>/dev/null || true; } \
    | sed -n '/-----BEGIN CERTIFICATE-----/,/-----END CERTIFICATE-----/{p;/-----END CERTIFICATE-----/q}' || true
}

# strih_https_status NAME PATH [CURL ARGS...] -> the HTTP status nginx answers for https://NAME/PATH
# (resolved to 127.0.0.1), read from the FIRST status line curl dumps (`-D -`): a WebSocket upgrade
# answers 101 and curl then holds the connection until --max-time. `000` when nothing answers.
strih_https_status() {
  local name="$1" path="$2" code
  shift 2
  code="$( { curl -sk -D - -o /dev/null --max-time 5 --resolve "${name}:443:127.0.0.1" "$@" "https://${name}${path}" 2>/dev/null || true; } \
    | sed -n 's/^HTTP\/[0-9.]* \([0-9][0-9][0-9]\).*/\1/p;T;q' || true)"
  printf '%s' "${code:-000}"
}

# strih_https_janus_ws_listeners -> where Janus's WebSocket (:STRIH_HTTPS_JANUS_PORT) listens, read
# from `ss -ltnH`: `loopback <addrs>` when every listener is on 127.0.0.0/8 or [::1], `exposed <addrs>`
# when any is not (the LAN IP, 0.0.0.0, [::], *), `not listening at all` when nothing does. Issue 1453:
# Janus on loopback is what makes the hub's /janus relay the only way to Janus. Always rc 0.
strih_https_janus_ws_listeners() {
  local listen addr all="" exposed=0 _s _r _q
  listen="$(ss -ltnH "sport = :${STRIH_HTTPS_JANUS_PORT}" 2>/dev/null || true)"
  while read -r _s _r _q addr _; do
    [ -n "${addr:-}" ] || continue
    all="${all:+${all},}${addr}"
    case "$addr" in
      127.*:"${STRIH_HTTPS_JANUS_PORT}" | "[::1]"*":${STRIH_HTTPS_JANUS_PORT}") ;;
      *) exposed=1 ;;
    esac
  done <<<"$listen"
  if [ -z "$all" ]; then
    printf 'not listening at all\n'
  elif [ "$exposed" = 1 ]; then
    printf 'exposed on %s\n' "$all"
  else
    printf 'loopback %s\n' "$all"
  fi
  return 0
}

strih_https_row_count() {
  local sites n=0 site first aliases up janus
  local -a names
  sites="$(strih_https_sites 2>/dev/null)" || { printf '1'; return 0; }
  if [ -z "$sites" ]; then printf '1'; return 0; fi
  while IFS='|' read -r site first aliases up janus; do
    IFS=' ' read -r -a names <<<"${first} ${aliases}"
    n=$((n + 1 + ${#names[@]}))
  done <<<"$sites"
  printf '%s' "$((n + 2))"
}

# strih_https_grade_rows -> verify-strih rows `OK|text`, `NOTE|text` or `FAIL|text`
# (strih_https_row_count of them). Always rc 0.
strih_https_grade_rows() {
  local sites nginx avail enabled tmp site first aliases up janus paths cert key origin st act listen
  local problems="" names_ok="" pem sans end name h1 h2 jl te ini hook stat_line owner live fp_s fp_l
  local -a names
  sites="$(strih_https_sites 2>/dev/null)" || { printf 'FAIL|(https) the box facts are unreadable\n'; return 0; }
  if [ -z "$sites" ]; then
    printf 'NOTE|(https) this box declares no HTTPS site (STRIH_HTTPS_INTERKOM_HOSTS / STRIH_HTTPS_SHADING_HOST = none) -- not graded\n'
    return 0
  fi
  nginx="$(strih_https_nginx_dir)"
  avail="${nginx}/sites-available"
  enabled="${nginx}/sites-enabled"

  act="$(systemctl is-active nginx 2>/dev/null | head -n 1 || true)"
  [ "$act" = active ] || problems="${problems} nginx=${act:-unreadable}"
  listen="$(ss -ltnH 'sport = :443' 2>/dev/null || true)"
  [ -n "$listen" ] || problems="${problems} :443-not-listening"
  live="$(strih_https_le_dir)/live"
  if [ -e "$live" ] && { [ ! -r "$live" ] || [ ! -x "$live" ]; }; then
    problems="${problems} ${live}-unreadable-as-$(id -un 2>/dev/null || echo ?)(run verify-strih.sh as root)"
  fi
  tmp="$(mktemp -d)" || { problems="${problems} mktemp-failed"; tmp=""; }
  while IFS='|' read -r site first aliases up janus; do
    paths="$(strih_https_cert_paths "$site" "$first")"
    IFS='|' read -r cert key origin <<<"$paths"
    if [ "$origin" = none ]; then
      problems="${problems} ${site}=no-certificate"
    elif [ -n "$tmp" ] && strih_https_site_text "$site" "$first" "$aliases" "$up" "$janus" "$cert" "$key" > "${tmp}/t" 2>/dev/null \
      && printf '%s\n' "$(cat "${tmp}/t")" > "${tmp}/site"; then
      st="$(strih_session_app_file_state "${avail}/${site}" "${tmp}/site")"
      [ "$st" = ok ] || problems="${problems} ${site}-site=${st}"
    else
      problems="${problems} ${site}-site=render-failed"
    fi
    [ "$(readlink "${enabled}/${site}" 2>/dev/null || true)" = "../sites-available/${site}" ] \
      || problems="${problems} ${site}-not-enabled"
    names_ok="${names_ok} ${site}"
  done <<<"$sites"
  [ -z "$tmp" ] || rm -rf "$tmp"
  if [ -z "$problems" ]; then
    printf 'OK|(https-nginx) nginx active, :443 listening, the%s site(s) equal to their render and enabled\n' "$names_ok"
  else
    printf 'FAIL|(https-nginx)%s -- re-run setup-strih.sh step 16i (issue 1399); journalctl -u nginx\n' "$problems"
  fi

  while IFS='|' read -r site first aliases up janus; do
    paths="$(strih_https_cert_paths "$site" "$first")"
    IFS='|' read -r cert key origin <<<"$paths"
    IFS=' ' read -r -a names <<<"${first} ${aliases}"
    for name in "${names[@]}"; do
      pem="$(strih_https_served_pem "$name")"
      if [ -z "$pem" ]; then
        printf 'FAIL|(https-cert) %s: nothing serves a certificate for this name on 127.0.0.1:443 -- is nginx running the %s site?\n' "$name" "$site"
        continue
      fi
      sans="$( { openssl x509 -noout -ext subjectAltName <<<"$pem" 2>/dev/null || true; } | strih_https_san_names | tr '\n' ' ' | sed 's/ $//')"
      end="$( { openssl x509 -noout -enddate <<<"$pem" 2>/dev/null || true; } | sed -n 's/^notAfter=//p' || true)"
      if ! strih_https_name_covered "$name" "$sans"; then
        printf 'FAIL|(https-cert) %s: the served certificate (SAN %s) does not cover this name -- re-run setup-strih.sh step 16i\n' "$name" "${sans:-none}"
      elif ! openssl x509 -noout -checkend "$((STRIH_HTTPS_MIN_DAYS * 86400))" <<<"$pem" >/dev/null 2>&1; then
        printf 'FAIL|(https-cert) %s: the served certificate expires within %s days (notAfter %s) -- renewal is not working: journalctl -u certbot, the (https-renewal) row\n' \
          "$name" "$STRIH_HTTPS_MIN_DAYS" "${end:-?}"
      elif [ "$origin" = live ]; then
        fp_s="$( { openssl x509 -noout -fingerprint -sha256 <<<"$pem" 2>/dev/null || true; } | sed -n 's/.*Fingerprint=//p' || true)"
        fp_l="$( { openssl x509 -noout -fingerprint -sha256 -in "$cert" 2>/dev/null || true; } | sed -n 's/.*Fingerprint=//p' || true)"
        if [ -n "$fp_s" ] && [ "$fp_s" = "$fp_l" ]; then
          printf 'OK|(https-cert) %s: served, valid past %s days (notAfter %s), the certificate in %s\n' "$name" "$STRIH_HTTPS_MIN_DAYS" "${end:-?}" "$(dirname "$cert")"
        else
          printf 'FAIL|(https-cert) %s: nginx serves a certificate other than the live one in %s (fingerprint %s vs %s) -- reload nginx (systemctl reload nginx) and re-run\n' \
            "$name" "$(dirname "$cert")" "${fp_s:-none}" "${fp_l:-unreadable}"
        fi
      elif [ "$origin" = hand ]; then
        printf 'NOTE|(https-cert) %s: served and valid (notAfter %s), but from the hand copy %s until migrated -- the supervisor places the DNS token, then setup-strih.sh step 16i issues it here (issue 1399)\n' \
          "$name" "${end:-?}" "$cert"
      else
        printf 'FAIL|(https-cert) %s: served, but the %s site names no certificate of this box -- re-run setup-strih.sh step 16i\n' "$name" "$site"
      fi
    done
  done <<<"$sites"

  while IFS='|' read -r site first aliases up janus; do
    if [ "$site" = interkom ]; then
      h1="$(strih_https_status "$first" /api/state)"
      h2="$(strih_https_status "$first" /janus --http1.1 -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
        -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
        -H 'Sec-WebSocket-Protocol: janus-protocol')"
      jl="$(strih_https_janus_ws_listeners)"
      if [ "$h1" != 200 ] || [ "$h2" != 101 ]; then
        printf 'FAIL|(https-upstream) interkom through nginx: the hub /api/state %s (want 200), the /janus upgrade through the hub relay %s (want 101) -- systemctl status intercom-hub janus\n' "$h1" "$h2"
      elif [ "${jl%% *}" = loopback ]; then
        printf 'OK|(https-upstream) interkom through nginx: the hub /api/state 200, the /janus upgrade 101 through the hub relay (Janus reached), the Janus WebSocket on loopback only (%s)\n' "${jl#* }"
      else
        printf 'FAIL|(https-upstream) interkom: the Janus WebSocket :%s is %s, not loopback only -- a client could go around the hub /janus relay (the plain-RTP join); re-run setup-strih.sh step 14 and restart janus (issue 1453)\n' "$STRIH_HTTPS_JANUS_PORT" "$jl"
      fi
    else
      h1="$(strih_https_status "$first" /api/version)"
      if [ "$h1" = 200 ]; then
        printf 'OK|(https-upstream) shading through nginx: bkshading /api/version 200\n'
      else
        printf 'FAIL|(https-upstream) shading through nginx: bkshading /api/version %s (want 200) -- systemctl status bkshading-service\n' "$h1"
      fi
    fi
  done <<<"$sites"

  problems=""
  te="$(systemctl is-enabled certbot.timer 2>/dev/null | head -n 1 || true)"
  [ "$te" = enabled ] || problems="${problems} certbot.timer=${te:-unreadable}"
  hook="$(strih_https_hook_path)"
  tmp="$(mktemp -d)"
  if [ -n "$tmp" ] && strih_https_hook_text > "${tmp}/hook" 2>/dev/null; then
    st="$(strih_session_app_file_state "$hook" "${tmp}/hook")"
  else
    st=render-failed
  fi
  [ -z "$tmp" ] || rm -rf "$tmp"
  if [ "$st" != ok ]; then
    problems="${problems} deploy-hook=${st}"
  elif [ ! -x "$hook" ]; then
    problems="${problems} deploy-hook=not-executable"
  fi
  ini="$(strih_https_ini)"
  owner="${STRIH_HTTPS_INI_OWNER:-root}"
  if [ ! -f "$ini" ]; then
    problems="${problems} no-${ini}(the supervisor places the zone-scoped DNS token, issue 1399 runbook)"
  else
    stat_line="$(stat -c '%U %a' "$ini" 2>/dev/null || true)"
    [ "$stat_line" = "${owner} 600" ] || problems="${problems} ${ini}=${stat_line:-unreadable}(want ${owner} 600)"
  fi
  if [ -z "$problems" ]; then
    printf 'OK|(https-renewal) certbot.timer enabled, the nginx deploy hook in place, %s present (%s 600) -- the certificates renew on this box\n' "$ini" "$owner"
  else
    printf 'FAIL|(https-renewal)%s -- re-run setup-strih.sh step 16i (issue 1399)\n' "$problems"
  fi
  return 0
}

# strih_https_grade_report -> print the rows through the CALLER's ok / note / bad functions
# (verify-strih). rc 1 unless the grader printed every row.
strih_https_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_https_grade_rows)
  [ "$rows" = "$(strih_https_row_count)" ]
}
