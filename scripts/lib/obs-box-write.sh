#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing setup script owns strict mode (set -euo pipefail) and fail()
# obs-box-write.sh -- the ONE idempotent install of a rendered OBS-box config file, sourced by
# scripts/lib/obs-box-baseline.sh (callers source that ONE entry point; split out in issue 1474 because
# the baseline file is at the ~1000-line budget). Two front ends over one core: the stdin form
# obs_box_write_if_changed and the command form obs_box_write_if_changed_rendered.

# obs_box_write_if_changed DEST MODE OWNER LABEL (content on stdin) -- the ONE idempotent install of a
# rendered config file: compare, rewrite only when the content, the mode or the owner differs, log the
# outcome (`LABEL: DEST unchanged` / `LABEL: DEST written`). OWNER is `user:group` (names). The new
# content goes to an exclusive `mktemp` sibling first (chmod + chown there), then one rename, so a reader
# never sees a half-written file and a pre-planted temp name is never reused; the temp is removed when any
# step fails. Fails loud via the caller's fail() (after the tool's own error) when the file cannot be
# written. Used for the operator-session audio drop-ins (setup-strih step 12) and the kiosk brightness
# facet (obs_box_brightness_keys).
#
# The stdin form cannot see its producer's exit status (issue 1474): feed it only a producer that
# cannot fail -- `printf '%s\n' "$var"` of a text already captured with `|| return 1`, or a `< file`
# redirect. A renderer goes through obs_box_write_if_changed_rendered below. An empty input is refused
# (rc 1, nothing written): no installed file is ever empty. tests/python/test_obs_box_write_rendered_1474.py
# sweeps every caller for that rule.
obs_box_write_if_changed() {
    local dest="${1:?obs_box_write_if_changed: DEST required}" mode="${2:?obs_box_write_if_changed: MODE required}"
    local owner="${3:?obs_box_write_if_changed: OWNER required}" label="${4:-file}"
    local want
    want="$(cat && printf x)" || { echo "${label}: could not read the content for ${dest} -- left untouched" >&2; return 1; }
    want="${want%x}"
    if [ -z "$want" ]; then
        echo "${label}: the content for ${dest} is empty -- left untouched" >&2
        return 1
    fi
    _obs_box_write_text "$dest" "$mode" "$owner" "$label" "$want"
}

# obs_box_write_if_changed_rendered DEST MODE OWNER LABEL CMD [ARGS...] -- the same install, with the
# renderer run HERE (issue 1474): `want="$(CMD ARGS && printf x)"` keeps the exact bytes (trailing
# newlines included, so verify's byte compare against the renderer stays identical) AND the renderer's
# exit code. A renderer that fails (rc named) or renders nothing is refused: DEST untouched, rc 1, the
# label on stderr; the caller adds `|| return 1` / `|| fail`.
obs_box_write_if_changed_rendered() {
    local dest="${1:?obs_box_write_if_changed_rendered: DEST required}"
    local mode="${2:?obs_box_write_if_changed_rendered: MODE required}"
    local owner="${3:?obs_box_write_if_changed_rendered: OWNER required}" label="${4:-file}" want rc=0
    shift 4 || return 1
    [ "$#" -gt 0 ] || { echo "${label}: no renderer named for ${dest}" >&2; return 1; }
    want="$("$@" && printf x)" || rc=$?
    if [ "$rc" != 0 ]; then
        echo "${label}: the renderer $1 failed (rc ${rc}) -- ${dest} left untouched" >&2
        return 1
    fi
    want="${want%x}"
    if [ -z "$want" ]; then
        echo "${label}: the renderer $1 renders nothing -- ${dest} left untouched" >&2
        return 1
    fi
    _obs_box_write_text "$dest" "$mode" "$owner" "$label" "$want"
}

# _obs_box_write_text DEST MODE OWNER LABEL TEXT -- the shared core of both forms: TEXT verbatim.
_obs_box_write_text() {
    local dest="$1" mode="$2" owner="$3" label="$4" want="$5" tmp
    if [ -f "$dest" ] && [ "$(stat -c %a "$dest" 2>/dev/null)" = "${mode#0}" ] \
        && [ "$(stat -c %U:%G "$dest" 2>/dev/null)" = "$owner" ] \
        && cmp -s "$dest" <(printf '%s' "$want"); then
        echo "  ${label}: ${dest} unchanged"
        return 0
    fi
    tmp="$(mktemp "${dest%/*}/.${dest##*/}.XXXXXX")" || fail "${label}: could not create a temp file next to ${dest}"
    if ! { printf '%s' "$want" > "$tmp" && chmod "$mode" "$tmp" && chown "$owner" "$tmp" && mv -f "$tmp" "$dest"; }; then
        rm -f "$tmp"
        fail "${label}: could not write ${dest}"
    fi
    echo "  ${label}: ${dest} written"
}
