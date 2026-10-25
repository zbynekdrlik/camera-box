#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements) -- the
# scripts/lib/*.sh convention: strict mode belongs to the caller (setup-strih.sh / verify-strih.sh).
#
# scripts/lib/obs-audio-monitor.sh -- issue 1436 (owner 25.10.2026): the stream operator's headphones had
# exeldro's Audio Monitor plugin on the Windows stream box (the program audio on its own fader, before and
# during the stream); the Linux stream boxes had none. This installs the pinned upstream plugin on the
# STREAM role (stream-snv, stream-pp), the issue-1361 Downstream Keyer pattern:
#   * the .deb pinned by URL + sha256 (its GitHub release digest), fail-loud on a mismatch;
#   * EXTRACTED (`dpkg-deb -x`), never dpkg-installed: its `Depends: obs-studio` is never met by the
#     genlock bundle OBS (a tarball in the /usr prefix);
#   * the extracted plugin must hash to the pinned .so sha256; only the plugin + its locale go into the
#     /usr prefix OBS loads; a box whose plugin already has the pinned hash downloads nothing;
#   * the strih role is production-only and never gets it.
# The plugin reaches the headphones through PulseAudio (libpulse0, kept on the stream role, pipewire-pulse):
# its filter's device is the locked sink `stream-operator-headphones` (issue 1465).
#
#   obs_audio_monitor_install LIBDIR SHAREDIR -> setup-strih step 4c (stream role), rc 1 + a reason
#   obs_audio_monitor_grade_report            -> verify-strih item 35 rows through the caller's ok/bad/note

obs_audio_monitor_version() { printf '0.10.1'; }
obs_audio_monitor_deb_url() {
    printf 'https://github.com/exeldro/obs-audio-monitor/releases/download/0.10.1/audio-monitor-0.10.1-x86_64-linux-gnu.deb'
}
obs_audio_monitor_deb_sha256() { printf '0cd326b90f5b0fafc812b5c406edf802118f12ccf5e3dd309a3966fe190cc886'; }
obs_audio_monitor_so_sha256() { printf '6a2b8e084e5ee5440d5b14fa84858b894a5486ac47b5ea5ae9aac97a3658a50b'; }
obs_audio_monitor_so_path() { printf '%s/obs-plugins/audio-monitor.so' "${1:-/usr/lib/x86_64-linux-gnu}"; }
obs_audio_monitor_data_dir() { printf '%s/obs/obs-plugins/audio-monitor' "${1:-/usr/share}"; }

# obs_audio_monitor_install LIBDIR SHAREDIR -> install the pinned plugin on the stream role; a no-op (one
# line) on the strih role. Downloads only when the installed plugin is not the pinned file. rc 1 + a
# reason on any failure (nothing half-installed: the plugin is copied only after both hashes match).
obs_audio_monitor_install() {
    local libdir="${1:-/usr/lib/x86_64-linux-gnu}" sharedir="${2:-/usr/share}" so data tmp got v
    v="$(obs_audio_monitor_version)"
    if [ "$(strih_box_role)" != stream ]; then
        echo "  audio-monitor: not installed on the strih role (stream role only, issue 1436)"
        return 0
    fi
    so="$(obs_audio_monitor_so_path "$libdir")"
    data="$(obs_audio_monitor_data_dir "$sharedir")"
    if [ -f "$so" ] && [ "$(sha256sum "$so" | awk '{print $1}')" = "$(obs_audio_monitor_so_sha256)" ] \
       && [ -f "${data}/locale/en-US.ini" ]; then
        echo "  audio-monitor ${v} already installed (${so}, pinned sha256)"
        return 0
    fi
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/audio-monitor-${v}.XXXXXX")" || { echo "audio-monitor: mktemp failed" >&2; return 1; }
    if ! curl -fsSL -o "${tmp}/am.deb" "$(obs_audio_monitor_deb_url)"; then
        rm -rf "$tmp"; echo "audio-monitor: download failed ($(obs_audio_monitor_deb_url))" >&2; return 1
    fi
    got="$(sha256sum "${tmp}/am.deb" | awk '{print $1}')"
    if [ "$got" != "$(obs_audio_monitor_deb_sha256)" ]; then
        rm -rf "$tmp"; echo "audio-monitor: .deb sha256 mismatch (want $(obs_audio_monitor_deb_sha256), got ${got}) -- refusing to install" >&2; return 1
    fi
    if ! dpkg-deb -x "${tmp}/am.deb" "${tmp}/x"; then rm -rf "$tmp"; echo "audio-monitor: dpkg-deb -x failed" >&2; return 1; fi
    local new="${tmp}/x/usr/lib/x86_64-linux-gnu/obs-plugins/audio-monitor.so"
    local loc="${tmp}/x/usr/share/obs/obs-plugins/audio-monitor/locale"
    if [ ! -f "$new" ] || [ ! -d "$loc" ]; then rm -rf "$tmp"; echo "audio-monitor: the .deb has no plugin/locale at the expected paths" >&2; return 1; fi
    got="$(sha256sum "$new" | awk '{print $1}')"
    if [ "$got" != "$(obs_audio_monitor_so_sha256)" ]; then
        rm -rf "$tmp"; echo "audio-monitor: plugin sha256 mismatch (want $(obs_audio_monitor_so_sha256), got ${got})" >&2; return 1
    fi
    # a new inode (install unlinks first): a running OBS keeps the file it mapped
    if ! mkdir -p "$(dirname "$so")" "${data}/locale" || ! install -m 0644 "$new" "$so" \
       || ! cp -a "${loc}/." "${data}/locale/"; then
        rm -rf "$tmp"; echo "audio-monitor: installing ${so} / ${data} failed" >&2; return 1
    fi
    chmod 0755 "$data" "${data}/locale"
    chmod 0644 "${data}/locale/"*
    rm -rf "$tmp"
    echo "  audio-monitor ${v} installed -> ${so} (+ locale); OBS loads it at its next start"
}

# obs_audio_monitor_newest_log -> the newest OBS log of the desktop user (STRIH_AUDIO_MONITOR_LOG_DIR seam).
obs_audio_monitor_newest_log() {
    local d="${STRIH_AUDIO_MONITOR_LOG_DIR:-/home/${STRIH_LX_USER:-newlevel}/.config/obs-studio/logs}"
    ls -1t "$d"/*.txt 2>/dev/null | head -n 1 || true
}

# obs_audio_monitor_grade_report -> verify-strih item 35's (audio-monitor) rows through the caller's
# ok / bad / note: the plugin at the pin with its locale (FAIL otherwise), and the newest OBS log -- when
# OBS started after the plugin was installed -- carries `[Audio Monitor] loaded version <pin>` (FAIL
# otherwise; NOTE when OBS has not started since). One NOTE on the strih role. Always rc 0.
obs_audio_monitor_grade_report() {
    local libdir="${STRIH_LIBDIR:-/usr/lib/x86_64-linux-gnu}" sharedir="${STRIH_SHAREDIR:-/usr/share}" so sha log v
    v="$(obs_audio_monitor_version)"
    if [ "$(strih_box_role)" != stream ]; then
        note "(audio-monitor) not graded: the Audio Monitor plugin is installed on the stream role only (issue 1436)"
        return 0
    fi
    so="$(obs_audio_monitor_so_path "$libdir")"
    sha="$(sha256sum "$so" 2>/dev/null | awk '{print $1}' || true)"
    if [ -z "$sha" ]; then
        bad "(audio-monitor) ${so} missing -- re-run setup-strih.sh step 4c (issue 1436)"; return 0
    elif [ "$sha" != "$(obs_audio_monitor_so_sha256)" ]; then
        bad "(audio-monitor) ${so} is not the pinned ${v} plugin (sha256 ${sha}) -- re-run setup-strih.sh step 4c"; return 0
    elif [ ! -f "$(obs_audio_monitor_data_dir "$sharedir")/locale/en-US.ini" ]; then
        bad "(audio-monitor) the plugin's locale is missing -- re-run setup-strih.sh step 4c"; return 0
    fi
    ok "(audio-monitor) ${v} installed at ${so} (pinned sha256)"
    log="$(obs_audio_monitor_newest_log)"
    if [ -z "$log" ] || [ ! "$log" -nt "$so" ]; then
        note "(audio-monitor) OBS has not started since the plugin was installed -- restart OBS to load it"
    elif grep -a -q -F -- "[Audio Monitor] loaded version ${v}" "$log"; then
        ok "(audio-monitor) the running OBS loaded it (${log##*/})"
    else
        bad "(audio-monitor) OBS started after the install but ${log##*/} has no '[Audio Monitor] loaded version ${v}' -- read the log's module load errors (libpulse0 present?)"
    fi
    return 0
}
