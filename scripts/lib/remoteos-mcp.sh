#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak
# into it. Each caller (setup-strih.sh / setup-imag.sh / setup-device.sh / verify-strih.sh) owns
# its own strict mode; every function here returns a status instead of exiting.
#
# scripts/lib/remoteos-mcp.sh -- issue 1361: the ONE install of the RemoteOS MCP control-channel
# agent (remoteos-mcp.service on :8092, the separate zbynekdrlik/remoteos-mcp project) for every
# managed Linux box: setup-strih.sh step 10, setup-imag.sh step 23, setup-device.sh STEP 17b.
#
# WHY it replaces the upstream install-linux.sh: that installer pip-installs into the SYSTEM python
# (`--break-system-packages`, the Debian RECORD conflicts of the cam1 re-provision) and writes the
# bearer key into the world-readable unit's ExecStart. strih-lx runs a hand-made venv instead
# (read 25.9.2026: /opt/remoteos-mcp-venv, `-m remoteos --transport streamable-http --enable-all
# --host 0.0.0.0 --port 8092`, User=newlevel + DISPLAY=:0 + XDG_RUNTIME_DIR). This lib makes a fresh
# run produce that box by construction:
#   * the source is the project's GitHub API tarball of a PINNED commit (`remoteos_mcp_pinned_ref`,
#     the one strih-lx runs; `REMOTEOS_MCP_REF` overrides for a deliberate bump -- pin-not-latest, so a
#     deploy never silently installs whatever upstream `main` is), fetched with the
#     `Authorization: token` header read from STDIN when GH_TOKEN is set (never argv), anonymously
#     otherwise (the repo is public);
#   * pip installs it into the venv with the project's own constraints.txt, never into /usr, and runs
#     with GH_TOKEN / REMOTEOS_MCP_AUTH_KEY removed from its environment (it executes build code);
#   * the key comes from REMOTEOS_MCP_AUTH_KEY, else a legacy unit's `--auth-key` (what the RUNNING
#     service accepts), else the existing EnvironmentFile, else config.json, else a fresh one -- a
#     re-run keeps the key dev1's .mcp.json holds. It lands in 0600 files only (config.json + the
#     unit's EnvironmentFile, read by remoteos as REMOTEOS_AUTH_KEY), never in the unit text or an argv;
#   * idempotent: the installed source id (the tarball's top dir, which carries the commit) is
#     recorded in the venv; an unchanged source skips pip, unchanged files are not rewritten, and a
#     restart-policy box restarts the service only when something changed;
#   * a failed fetch or pip keeps a working installed venv (WARNING); the restart policy proves the
#     service answers /health AND refuses an unauthenticated /mcp request (401).
#
#   remoteos_mcp_install USER MODE POLICY   (root) MODE desktop|headless, POLICY restart|enable-only
#   remoteos_mcp_verdict UNIT_TEXT ENV_STAT IMPORT_OK ENABLED ACTIVE UNAUTH_CODE -> `ok ...` | `FAIL: ...`

remoteos_mcp_repo() { printf 'zbynekdrlik/remoteos-mcp'; }
remoteos_mcp_port() { printf '8092'; }
# The pinned source commit: the remoteos-mcp commit the live strih-lx venv runs (pip direct_url
# commit_id, read 25.9.2026). Bump it on purpose, together with a live check of one box.
remoteos_mcp_pinned_ref() { printf '8b4ce58dbed08c366ecbd50a32ff66213b77a9ba'; }
remoteos_mcp_ref() { printf '%s' "${REMOTEOS_MCP_REF:-$(remoteos_mcp_pinned_ref)}"; }
remoteos_mcp_venv_dir() { printf '%s' "${REMOTEOS_MCP_VENV:-/opt/remoteos-mcp-venv}"; }
remoteos_mcp_config_dir() { printf '%s' "${REMOTEOS_MCP_CONFIG_DIR:-/etc/remoteos-mcp}"; }
remoteos_mcp_config_json_path() { printf '%s/config.json' "$(remoteos_mcp_config_dir)"; }
remoteos_mcp_env_file() { printf '%s/remoteos-mcp.env' "$(remoteos_mcp_config_dir)"; }
remoteos_mcp_unit_path() { printf '%s' "${REMOTEOS_MCP_UNIT_PATH:-/etc/systemd/system/remoteos-mcp.service}"; }
# The file inside the venv that records which source tree is installed (the idempotency key).
remoteos_mcp_source_marker() { printf '%s/.camera-box-source' "$(remoteos_mcp_venv_dir)"; }

# remoteos_mcp_source_url REF -> the GitHub API tarball URL of REF.
remoteos_mcp_source_url() {
    printf 'https://api.github.com/repos/%s/tarball/%s' "$(remoteos_mcp_repo)" "${1:-$(remoteos_mcp_ref)}"
}

# remoteos_mcp_desktop_packages -> the desktop-control tools the upstream installer adds on a box with
# a graphical session (screenshots, input, clipboard, OCR, AT-SPI). One per line.
remoteos_mcp_desktop_packages() {
    printf '%s\n' xdotool scrot xclip wmctrl tesseract-ocr ffmpeg python3-pyatspi gir1.2-atspi-2.0 python3-gi
}

# remoteos_mcp_key_ok KEY -> 0 iff KEY is 1-128 ASCII letters/digits (the upstream installer's key
# charset). Anything else would break the JSON / EnvironmentFile line or smuggle shell syntax.
remoteos_mcp_key_ok() {
    local k="${1-}"
    [ -n "$k" ] && [ "${#k}" -le 128 ] || return 1
    case "$k" in
        *[!A-Za-z0-9]*) return 1 ;;
    esac
    return 0
}

# remoteos_mcp_key_from_config_text TEXT -> the `auth_key` of an upstream config.json, or nothing
# (not JSON, no key, not a string). Always rc 0.
remoteos_mcp_key_from_config_text() {
    printf '%s' "${1-}" | python3 -c '
import json, sys
try:
    v = json.load(sys.stdin).get("auth_key", "")
except Exception:
    v = ""
if isinstance(v, str):
    print(v, end="")
' 2>/dev/null || true
}

# remoteos_mcp_key_from_unit_text TEXT -> the value after `--auth-key` on the unit's ExecStart line
# (the shape the upstream installer and the hand-made strih-lx unit wrote), or nothing. Always rc 0.
remoteos_mcp_key_from_unit_text() {
    local line rest
    while IFS= read -r line; do
        case "$line" in
            ExecStart=*--auth-key\ *)
                rest="${line#*--auth-key }"
                printf '%s' "${rest%% *}"
                return 0
                ;;
        esac
    done <<<"${1-}"
    return 0
}

# remoteos_mcp_key_from_env_text TEXT -> the REMOTEOS_AUTH_KEY value of an EnvironmentFile, or nothing.
remoteos_mcp_key_from_env_text() {
    local line
    while IFS= read -r line; do
        case "$line" in
            REMOTEOS_AUTH_KEY=*) printf '%s' "${line#REMOTEOS_AUTH_KEY=}"; return 0 ;;
        esac
    done <<<"${1-}"
    return 0
}

# remoteos_mcp_resolve_key ENV_KEY UNIT_KEY ENVFILE_KEY CONFIG_KEY -> the key to install: ENV_KEY when
# set (it must be valid: rc 2 + stderr otherwise, never silently replaced), else the first VALID of a
# legacy unit's --auth-key (the key the RUNNING service accepts, so dev1's .mcp.json holds it), the
# EnvironmentFile and config.json, else nothing (rc 0 -- the caller generates one).
remoteos_mcp_resolve_key() {
    local env_key="${1-}" k
    if [ -n "$env_key" ]; then
        if remoteos_mcp_key_ok "$env_key"; then
            printf '%s' "$env_key"
            return 0
        fi
        echo "remoteos-mcp: REMOTEOS_MCP_AUTH_KEY must be 1-128 letters/digits [A-Za-z0-9]; refusing to install it" >&2
        return 2
    fi
    for k in "${2-}" "${3-}" "${4-}"; do
        if remoteos_mcp_key_ok "$k"; then printf '%s' "$k"; return 0; fi
    done
    return 0
}

# remoteos_mcp_generate_key -> 32 random letters/digits (the upstream installer's generator).
remoteos_mcp_generate_key() {
    python3 -c "import secrets, string; print(''.join(secrets.choice(string.ascii_letters + string.digits) for _ in range(32)), end='')"
}

# remoteos_mcp_config_json KEY -> the upstream installer's config.json text (port, key, bind host).
remoteos_mcp_config_json() {
    printf '{\n  "port": %s,\n  "auth_key": "%s",\n  "host": "0.0.0.0"\n}\n' "$(remoteos_mcp_port)" "${1-}"
}

# remoteos_mcp_env_text KEY -> the unit's EnvironmentFile (remoteos reads REMOTEOS_AUTH_KEY).
remoteos_mcp_env_text() {
    printf 'REMOTEOS_AUTH_KEY=%s\n' "${1-}"
}

# remoteos_mcp_unit_text USER UID MODE -> the remoteos-mcp.service text. MODE `desktop` = the live
# strih-lx shape (runs as the desktop USER on its X display, ordered after the graphical session);
# `headless` = a cam box (no display). The key is never in this text: EnvironmentFile carries it.
# rc 2 on a bad USER / UID / MODE.
remoteos_mcp_unit_text() {
    local user="${1-}" uid="${2-}" mode="${3-}"
    case "$user" in
        ''|*[!A-Za-z0-9._-]*) echo "remoteos_mcp_unit_text: bad user '${user}'" >&2; return 2 ;;
    esac
    case "$uid" in
        ''|*[!0-9]*) echo "remoteos_mcp_unit_text: bad uid '${uid}'" >&2; return 2 ;;
    esac
    case "$mode" in
        desktop|headless) ;;
        *) echo "remoteos_mcp_unit_text: mode must be desktop|headless (got '${mode}')" >&2; return 2 ;;
    esac
    printf '[Unit]\n'
    printf 'Description=RemoteOS MCP control agent (camera-box scripts/lib/remoteos-mcp.sh, venv)\n'
    # graphical-session.target is a USER-manager target, so in this system unit it orders nothing; it
    # is kept only because the live strih-lx unit carries it (the agent needs no display to start).
    if [ "$mode" = desktop ]; then
        printf 'After=network.target graphical-session.target\n'
    else
        printf 'After=network.target\n'
    fi
    printf 'Wants=network.target\n'
    printf '[Service]\n'
    printf 'Type=simple\n'
    printf 'User=%s\n' "$user"
    if [ "$mode" = desktop ]; then
        printf 'Environment=DISPLAY=:0\n'
        printf 'Environment=XDG_RUNTIME_DIR=/run/user/%s\n' "$uid"
    fi
    printf 'EnvironmentFile=%s\n' "$(remoteos_mcp_env_file)"
    printf 'ExecStart=%s/bin/python -m remoteos --transport streamable-http --enable-all --host 0.0.0.0 --port %s\n' \
        "$(remoteos_mcp_venv_dir)" "$(remoteos_mcp_port)"
    printf 'Restart=always\n'
    printf 'RestartSec=5\n'
    printf 'StandardOutput=journal\n'
    printf 'StandardError=journal\n'
    printf '[Install]\n'
    printf 'WantedBy=multi-user.target\n'
}

# remoteos_mcp_fetch_source DEST_DIR [REF] -> download + unpack the source of REF into DEST_DIR (which
# must not exist yet) and print its id (the tarball's top dir, e.g. zbynekdrlik-remoteos-mcp-8b4ce58).
# GH_TOKEN, when set, is sent as the Authorization header read from curl's STDIN -- never an argv, so
# it never shows in `ps`. rc 1 + stderr on any failure.
remoteos_mcp_fetch_source() {
    local dest="${1-}" ref="${2:-$(remoteos_mcp_ref)}" url tgz id rc=0
    [ -n "$dest" ] || { echo "remoteos_mcp_fetch_source: DEST_DIR required" >&2; return 1; }
    url="$(remoteos_mcp_source_url "$ref")"
    tgz="$(mktemp "${TMPDIR:-/tmp}/remoteos-mcp-src.XXXXXX.tgz")" || return 1
    if [ -n "${GH_TOKEN:-}" ]; then
        printf 'Authorization: token %s\n' "$GH_TOKEN" \
            | curl -fsSL --max-time 120 -H @- -H 'Accept: application/vnd.github+json' -o "$tgz" "$url" || rc=$?
    else
        curl -fsSL --max-time 120 -H 'Accept: application/vnd.github+json' -o "$tgz" "$url" || rc=$?
    fi
    if [ "$rc" -ne 0 ]; then
        echo "remoteos-mcp: could not fetch ${url} (curl rc ${rc}; a private fork needs GH_TOKEN)" >&2
        rm -f "$tgz"
        return 1
    fi
    id="$(tar -tzf "$tgz" 2>/dev/null | awk -F/ 'NR == 1 { print $1 }')"
    case "$id" in
        ''|*[!A-Za-z0-9._-]*)
            echo "remoteos-mcp: ${url} is not a source tarball (top entry '${id}')" >&2
            rm -f "$tgz"
            return 1
            ;;
    esac
    mkdir -p "$dest" && tar -xzf "$tgz" -C "$dest" --strip-components=1 || {
        echo "remoteos-mcp: could not unpack the ${ref} source" >&2
        rm -f "$tgz"
        return 1
    }
    rm -f "$tgz"
    [ -f "${dest}/pyproject.toml" ] && [ -f "${dest}/constraints.txt" ] \
        || { echo "remoteos-mcp: the ${ref} source has no pyproject.toml + constraints.txt" >&2; return 1; }
    printf '%s' "$id"
}

# _remoteos_mcp_write PATH MODE LABEL (stdin: content) -> install the content at PATH with MODE when it
# (or the mode) differs; logs `written`/`unchanged`; sets _REMOTEOS_MCP_CHANGED=1 on a write, so it
# must run in the caller's shell (feed it a here-string, never a pipe). The file is owned by whoever
# runs this (root on a box). rc 1 on a failed write.
_remoteos_mcp_write() {
    local path="$1" mode="$2" label="$3" tmp
    tmp="$(mktemp "${path}.XXXXXX")" || return 1
    cat > "$tmp" || { rm -f "$tmp"; return 1; }
    # Content, mode AND owner (the obs_box_write_if_changed rule): a key file some other account owns
    # is rewritten, never logged `unchanged` while verify item 8 keeps failing it.
    if [ -f "$path" ] && cmp -s "$tmp" "$path" \
        && [ "$(stat -c '%a %U:%G' "$path" 2>/dev/null)" = "${mode} $(id -un):$(id -gn)" ]; then
        rm -f "$tmp"
        echo "  remoteos-mcp: ${label} unchanged"
        return 0
    fi
    chmod "$mode" "$tmp" && mv -f "$tmp" "$path" || { rm -f "$tmp"; return 1; }
    _REMOTEOS_MCP_CHANGED=1
    echo "  remoteos-mcp: ${label} written (${path})"
}

# _remoteos_mcp_healthy -> 0 once the service is active AND /health answers on :8092, polling
# REMOTEOS_MCP_HEALTH_TRIES (12) times REMOTEOS_MCP_HEALTH_SLEEP (5) s apart.
_remoteos_mcp_healthy() {
    local tries="${REMOTEOS_MCP_HEALTH_TRIES:-12}" pause="${REMOTEOS_MCP_HEALTH_SLEEP:-5}" i=0
    while [ "$i" -lt "$tries" ]; do
        if systemctl is-active --quiet remoteos-mcp \
            && curl -fsS --max-time 3 -o /dev/null "http://127.0.0.1:$(remoteos_mcp_port)/health"; then
            return 0
        fi
        i=$((i + 1))
        [ "$i" -lt "$tries" ] && sleep "$pause"
    done
    return 1
}

# remoteos_mcp_unauth_code -> the HTTP status an UNAUTHENTICATED POST /mcp gets on the local agent
# (401 = authentication is on; `000` = no answer). An empty REMOTEOS_AUTH_KEY would turn remoteos's
# authentication OFF on a 0.0.0.0 full-shell agent, so both the install and verify item 8 probe it.
remoteos_mcp_unauth_code() {
    curl -s -o /dev/null -w '%{http_code}' --max-time 5 -X POST "http://127.0.0.1:$(remoteos_mcp_port)/mcp" 2>/dev/null || true
}

# remoteos_mcp_install USER MODE POLICY -> install / refresh the agent (run as root).
#   USER    the account the service runs as (the desktop user, or root on a headless cam box)
#   MODE    desktop | headless  (see remoteos_mcp_unit_text; desktop also installs the desktop tools)
#   POLICY  restart     = (re)start now and require /health (strih, imag)
#           enable-only = enable for the next boot, never start (the cam-box convention)
# Env: REMOTEOS_MCP_AUTH_KEY (the key to install), GH_TOKEN (optional), REMOTEOS_MCP_REF.
# A failed source fetch with a working venv already installed keeps that install (WARNING) and still
# refreshes the unit + key files, so a re-run never breaks a working box on a network hiccup.
# rc 0 = installed and gated; rc 1 = a named failure on stderr.
remoteos_mcp_install() {
    local user="${1-}" mode="${2-}" policy="${3-}"
    local venv uid unit_text cfg_key unit_key ef_key key work src_id="" marker pkg_changed=0 en code
    case "$policy" in
        restart|enable-only) ;;
        *) echo "remoteos_mcp_install: POLICY must be restart|enable-only (got '${policy}')" >&2; return 1 ;;
    esac
    uid="$(id -u "$user" 2>/dev/null)" || { echo "remoteos_mcp_install: no such user '${user}'" >&2; return 1; }
    unit_text="$(remoteos_mcp_unit_text "$user" "$uid" "$mode")" || return 1
    venv="$(remoteos_mcp_venv_dir)"
    marker="$(remoteos_mcp_source_marker)"
    _REMOTEOS_MCP_CHANGED=0

    command -v curl >/dev/null 2>&1 \
        || DEBIAN_FRONTEND=noninteractive apt-get install -y curl >/dev/null \
        || { echo "remoteos-mcp: curl is missing and apt-get install curl failed" >&2; return 1; }
    if [ "$mode" = desktop ]; then
        # shellcheck disable=SC2046  # one package per word, by design
        DEBIAN_FRONTEND=noninteractive apt-get install -y $(remoteos_mcp_desktop_packages) >/dev/null \
            || echo "WARNING: remoteos-mcp: some desktop tools failed to install -- screenshots/input may be limited" >&2
    fi

    # The key: env, else a legacy unit's --auth-key (what the running service accepts), else the
    # EnvironmentFile, else config.json, else new.
    unit_key="$(remoteos_mcp_key_from_unit_text "$(cat "$(remoteos_mcp_unit_path)" 2>/dev/null || true)")"
    ef_key="$(remoteos_mcp_key_from_env_text "$(cat "$(remoteos_mcp_env_file)" 2>/dev/null || true)")"
    cfg_key="$(remoteos_mcp_key_from_config_text "$(cat "$(remoteos_mcp_config_json_path)" 2>/dev/null || true)")"
    key="$(remoteos_mcp_resolve_key "${REMOTEOS_MCP_AUTH_KEY:-}" "$unit_key" "$ef_key" "$cfg_key")" || return 1
    if [ -z "$key" ]; then
        key="$(remoteos_mcp_generate_key)" || { echo "remoteos-mcp: key generation failed" >&2; return 1; }
        echo "  remoteos-mcp: generated a new auth key -- update dev1's .mcp.json entry for this box to match"
    elif remoteos_mcp_key_ok "$cfg_key" && [ "$cfg_key" != "$key" ]; then
        echo "WARNING: remoteos-mcp: the config.json key differs from the key being installed -- config.json follows it" >&2
    fi

    # The source + the venv.
    work="$(mktemp -d "${TMPDIR:-/tmp}/remoteos-mcp.XXXXXX")" || return 1
    if src_id="$(remoteos_mcp_fetch_source "${work}/src")"; then
        if [ "$(cat "$marker" 2>/dev/null || true)" = "$src_id" ] \
            && "${venv}/bin/python" -c 'import remoteos' >/dev/null 2>&1; then
            echo "  remoteos-mcp: ${src_id} already installed in ${venv}"
        else
            if ! "${venv}/bin/python" -c 'import sys' >/dev/null 2>&1; then
                # Ubuntu's python3 ships venv without ensurepip; python3-venv adds it (no-op when present).
                DEBIAN_FRONTEND=noninteractive apt-get install -y python3-venv >/dev/null \
                    || { echo "remoteos-mcp: apt-get install python3-venv failed" >&2; rm -rf "$work"; return 1; }
                python3 -m venv "$venv" || { echo "remoteos-mcp: python3 -m venv ${venv} failed" >&2; rm -rf "$work"; return 1; }
            fi
            # pip executes the build code of the source and of every sdist dependency: never hand it
            # the GitHub token or the agent key.
            if env -u GH_TOKEN -u REMOTEOS_MCP_AUTH_KEY \
                PIP_CONSTRAINT="${work}/src/constraints.txt" PIP_DISABLE_PIP_VERSION_CHECK=1 \
                "${venv}/bin/python" -m pip install --no-cache-dir --upgrade "${work}/src" >/dev/null; then
                "${venv}/bin/python" -c 'import remoteos' >/dev/null 2>&1 \
                    || { echo "remoteos-mcp: ${venv} cannot import remoteos after the install" >&2; rm -rf "$work"; return 1; }
                printf '%s\n' "$src_id" > "$marker" || { rm -rf "$work"; return 1; }
                pkg_changed=1
                echo "  remoteos-mcp: installed ${src_id} into ${venv}"
            elif "${venv}/bin/python" -c 'import remoteos' >/dev/null 2>&1; then
                echo "WARNING: remoteos-mcp: pip install of ${src_id} failed -- keeping the installed $(cat "$marker" 2>/dev/null || echo 'venv') (the next run retries)" >&2
            else
                echo "remoteos-mcp: pip install of ${src_id} into ${venv} failed and nothing working is installed" >&2
                rm -rf "$work"
                return 1
            fi
        fi
    elif "${venv}/bin/python" -c 'import remoteos' >/dev/null 2>&1; then
        echo "WARNING: remoteos-mcp: source fetch failed -- keeping the installed $(cat "$marker" 2>/dev/null || echo 'venv')" >&2
    else
        rm -rf "$work"
        echo "remoteos-mcp: source fetch failed and ${venv} has no working install" >&2
        return 1
    fi
    rm -rf "$work"

    # The key files + the unit.
    mkdir -p "$(remoteos_mcp_config_dir)" && chmod 0755 "$(remoteos_mcp_config_dir)" || return 1
    # Here-strings, never a pipe: _remoteos_mcp_write must run in THIS shell to set the changed flag.
    _remoteos_mcp_write "$(remoteos_mcp_config_json_path)" 600 "config.json" <<<"$(remoteos_mcp_config_json "$key")" || return 1
    _remoteos_mcp_write "$(remoteos_mcp_env_file)" 600 "EnvironmentFile" <<<"$(remoteos_mcp_env_text "$key")" || return 1
    _remoteos_mcp_write "$(remoteos_mcp_unit_path)" 644 "unit" <<<"$unit_text" || return 1
    unset key cfg_key unit_key

    systemctl daemon-reload || { echo "remoteos-mcp: systemctl daemon-reload failed" >&2; return 1; }
    systemctl enable remoteos-mcp >/dev/null 2>&1 || true
    # The LITERAL is-enabled state: `--quiet`'s exit code also passes a `static` unit, which is not
    # started at boot.
    en="$(systemctl is-enabled remoteos-mcp 2>/dev/null || true)"
    [ "${en%%$'\n'*}" = "enabled" ] \
        || { echo "remoteos-mcp: remoteos-mcp.service is not enabled (is-enabled='${en}')" >&2; return 1; }

    if [ "$policy" = restart ]; then
        if [ "$_REMOTEOS_MCP_CHANGED" = 1 ] || [ "$pkg_changed" = 1 ]; then
            systemctl restart remoteos-mcp || { echo "remoteos-mcp: systemctl restart failed" >&2; return 1; }
        else
            systemctl start remoteos-mcp || { echo "remoteos-mcp: systemctl start failed" >&2; return 1; }
        fi
        _remoteos_mcp_healthy \
            || { echo "remoteos-mcp: the service is not active + answering /health on :$(remoteos_mcp_port)" >&2; return 1; }
        code="$(remoteos_mcp_unauth_code)"
        [ "$code" = 401 ] \
            || { echo "remoteos-mcp: authentication is not enforced -- an unauthenticated /mcp request answered '${code}' (want 401)" >&2; return 1; }
        echo "  remoteos-mcp: active + /health answering + authentication enforced on :$(remoteos_mcp_port)"
    else
        echo "  remoteos-mcp: enabled for the next boot (enable-only, not started)"
    fi
    return 0
}

# remoteos_mcp_verdict UNIT_TEXT ENV_STAT IMPORT_OK ENABLED ACTIVE UNAUTH_CODE -> `ok ...` (rc 0) or
# `FAIL: <reason>` (rc 1) for a box provisioned by remoteos_mcp_install. ENV_STAT = `stat -c '%a %U'`
# of the EnvironmentFile; IMPORT_OK = 1 when the venv python imports remoteos; ENABLED/ACTIVE = the
# first line of `systemctl is-enabled` / `is-active`; UNAUTH_CODE = remoteos_mcp_unauth_code (must be
# 401: authentication is on). Pure.
remoteos_mcp_verdict() {
    local unit="${1-}" env_stat="${2-}" import_ok="${3-}" en="${4-}" act="${5-}" unauth="${6-}"
    en="${en%%$'\n'*}"
    act="${act%%$'\n'*}"
    if [ -z "$unit" ]; then echo "FAIL: remoteos-mcp.service unit missing"; return 1; fi
    if grep -qF -- '--auth-key' <<<"$unit"; then echo "FAIL: the unit carries the auth key in its ExecStart"; return 1; fi
    if ! grep -qxF "ExecStart=$(remoteos_mcp_venv_dir)/bin/python -m remoteos --transport streamable-http --enable-all --host 0.0.0.0 --port $(remoteos_mcp_port)" <<<"$unit"; then
        echo "FAIL: the unit does not run the $(remoteos_mcp_venv_dir) venv (a pre-venv install)"; return 1
    fi
    if ! grep -qxF "EnvironmentFile=$(remoteos_mcp_env_file)" <<<"$unit"; then
        echo "FAIL: the unit has no EnvironmentFile=$(remoteos_mcp_env_file)"; return 1
    fi
    if [ "$env_stat" != "600 root" ]; then
        echo "FAIL: $(remoteos_mcp_env_file) is '${env_stat:-missing}' (want 600 root)"; return 1
    fi
    if [ "$import_ok" != 1 ]; then echo "FAIL: $(remoteos_mcp_venv_dir) cannot import remoteos"; return 1; fi
    if [ "$en" != enabled ]; then echo "FAIL: remoteos-mcp.service is '${en:-unknown}', not enabled"; return 1; fi
    if [ "$act" != active ]; then echo "FAIL: remoteos-mcp.service is '${act:-unknown}', not active"; return 1; fi
    case "$unauth" in
        401) ;;
        ''|000) echo "FAIL: authentication unverified (no answer to the unauthenticated /mcp probe)"; return 1 ;;
        *) echo "FAIL: authentication is OFF (an unauthenticated /mcp request answered ${unauth})"; return 1 ;;
    esac
    echo "ok venv $(remoteos_mcp_venv_dir), key in $(remoteos_mcp_env_file), enabled + active, unauthenticated /mcp refused"
    return 0
}
