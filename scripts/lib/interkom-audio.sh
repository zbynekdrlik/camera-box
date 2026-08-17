#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library, sourced by setup-device.sh /
# verify-device.sh / create-usb-linux.sh — mirrors every sibling in scripts/lib/ (e.g.
# timesync-authority.sh, log-bound.sh, udev-camera-box.sh), none of which set -euo pipefail
# either: sourcing a `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/interkom-audio.sh — #782 "interkom audio provisioning bake-in".
#
# SINGLE SOURCE OF TRUTH for the interkom (USB Audio+HID headset) ALSA provisioning: the canonical
# by-NAME /etc/asound.conf body, the per-box mixer-gain table, and the read-back parsers. Because
# setup-device.sh (writes/applies) and verify-device.sh (checks) BOTH source this file, a
# re-provisioned box can never drift from what the acceptance gate verifies — which is the whole
# point of the ticket.
#
# Root cause (#782): setup-device.sh STEP 5 generated /etc/asound.conf with the enumeration-time
# card NUMBER (`hw:$USB_CARD,0`) — which dangles when USB re-enumerates (cam7 live proof, #728
# class) — and never set the interkom mixer gains at all, so a freshly-provisioned box kept the
# CSCTEK headset's power-on default (Mic 91%/-3dB) while the hand-tuned older boxes ran 75%/-8dB
# → ~5dB louder mics in the intercom. The live fleet was hand-unified 2026-07-15; this lib bakes
# that unified state into provisioning so the next box comes out identical.

# interkom_asound_conf_content -> the canonical by-NAME /etc/asound.conf body on stdout. Referenced
# by card NAME (sysdefault:CARD=HID) so it is enumeration-proof — byte-identical to the live
# hand-unified fleet (sha256 d5db405c..., the cam2/cam3 majority form). Quoted heredoc: literal,
# no shell expansion.
interkom_asound_conf_content() {
    cat << 'ASOUND_EOF'
# Default: Use USB Audio HID device for intercom (by NAME — enumeration-proof, ref cam1/cam4)
pcm.!default {
    type asym
    playback.pcm {
        type plug
        slave.pcm "sysdefault:CARD=HID"
    }
    capture.pcm {
        type plug
        slave.pcm "sysdefault:CARD=HID"
    }
}

ctl.!default {
    type hw
    card HID
}
ASOUND_EOF
}

# interkom_mic_pct DEVICE_NAME -> the canonical interkom Mic (capture) gain percent for a fleet
# box. Per-box compensation (#782, owner 2026-07-15 final table): cam5-7 = 80 (analog-headset
# compensation), every other box = 75 (the hand-tuned cam1-4 reference). DEVICE_NAME is the
# uppercase hostname (CAM5), as resolved by resolve_device_name / camera_resolve.
interkom_mic_pct() {
    case "$1" in
        CAM5 | CAM6 | CAM7) printf '80' ;;
        *) printf '75' ;;
    esac
}

# interkom_pcm_pct DEVICE_NAME -> the canonical interkom PCM (playback / headphone) gain percent.
# Per-box compensation (#782, owner 2026-07-15 final table): cam5-7 = 94, every other box = 79.
interkom_pcm_pct() {
    case "$1" in
        CAM5 | CAM6 | CAM7) printf '94' ;;
        *) printf '79' ;;
    esac
}

# interkom_amixer_pct AMIXER_OUTPUT -> the first "[NN%]" percent from an `amixer sget` block (e.g.
# `Mono: Capture 384 [75%] [-8.00dB] [on]`), "" if none. Mirrors genlock_dropin_fps's parser shape
# with the trailing `|| true` (the #458 footgun: a no-match grep|head|tr fails under pipefail even
# though head/tr succeed on empty input, so a bare X="$(interkom_amixer_pct ...)" caller must never
# abort). `head -1` (never `grep -m1 ... | head`): a plain grep reads all input, no SIGPIPE.
interkom_amixer_pct() {
    printf '%s\n' "$1" | grep -oE '\[[0-9]+%\]' | head -1 | tr -d '[]%' || true
}

# interkom_asound_by_name_count TEXT -> the COUNT of by-NAME "CARD=HID" references in TEXT (an
# /etc/asound.conf). "0" for the old enumeration-time card-NUMBER form (`hw:0,0`) — exactly the
# drift the acceptance gate must catch. `grep -c` (NEVER -q: -q's early pipe close can SIGPIPE the
# upstream printf and, under pipefail, return non-zero even on a real match) + `|| true` (grep -c
# exits 1 with a printed "0" on no match; the bare-substitution caller must never abort).
interkom_asound_by_name_count() {
    printf '%s\n' "$1" | grep -cE 'CARD=HID' || true
}
