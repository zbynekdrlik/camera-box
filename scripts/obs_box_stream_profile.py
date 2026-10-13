#!/usr/bin/env python3
"""issue 1412 slice B: the stream ROLE's OBS profile on a Linux OBS box (stream-pp).

setup-strih.sh step 3 (stream role) runs it with OBS STOPPED (OBS rewrites its profile on exit). It
writes into ~/.config/obs-studio/basic/profiles/<profile>/:

  * basic.ini -- Advanced output, the stream encoder chosen by the GPU (an NVIDIA dGPU -> NVENC,
    otherwise the VAAPI texture encoder: the stream-pp laptop has an AMD iGPU only), 1920x1080 base +
    output at 30 fps, the stream output rescaled to 3840x2160 lanczos (ADVOUT_RESCALE), NV12 / 709 / partial, 48 kHz stereo. Every other key is kept (an upsert).
  * streamEncoder.json -- the advanced-output stream encoder settings: CBR 19000 kb/s, a keyframe
    every 2 s, H.264 High, no B-frames (the old Windows AMF profile of the box, issue 1412 recon).
  * service.json -- `rtmp_custom` to the local restreamer. ONLY the server is written: the stream key
    is the owner's and never written by us; an existing key is kept as it is.

Pure helpers + a small CLI; tests/python/test_obs_box_stream_role_1412.py.
"""
import argparse
import configparser
import io
import json
import os
import sys

VAAPI_ENCODER = "ffmpeg_vaapi_tex"
NVENC_ENCODER = "obs_nvenc_h264_tex"
VAAPI_RENDER_NODE = "/dev/dri/renderD128"
STREAM_BITRATE_KBPS = 19000
STREAM_KEYINT_S = 2
# FF_PROFILE_H264_HIGH, the int OBS's ffmpeg_vaapi encoder takes (scripts/imag_record_encoder.py).
H264_HIGH_PROFILE = 100
DEFAULT_RTMP_SERVER = "rtmp://127.0.0.1:1234/live"
# Owner request through restreamer, 13.10.2026: the stream output rescaled to 3840x2160 lanczos like the
# SNV stream OBS (canvas + output stay 1080p30). stream-pp's VAAPI H.264 encoder did 4K30 at 2.37x
# realtime with OBS running beside it (restreamer benchmark); 4K60 would take ~85 % and is not stable.
ADVOUT_RESCALE = {"Rescale": "true", "RescaleRes": "3840x2160", "RescaleFilter": "4", "Track1Bitrate": "320"}

BASIC_INI = {
    "Output": {"Mode": "Advanced"},
    "Video": {
        "BaseCX": "1920", "BaseCY": "1080", "OutputCX": "1920", "OutputCY": "1080",
        "FPSType": "0", "FPSCommon": "30",
        "ColorFormat": "NV12", "ColorSpace": "709", "ColorRange": "Partial",
    },
    "Audio": {"SampleRate": "48000", "ChannelSetup": "Stereo"},
}


def encoder_for(has_nvidia):
    """The stream encoder for the box's GPU: NVENC on an NVIDIA dGPU, else the VAAPI texture encoder."""
    return NVENC_ENCODER if has_nvidia else VAAPI_ENCODER


def stream_encoder_settings(encoder):
    """streamEncoder.json for ENCODER (CBR 19000, keyint 2 s, High, no B-frames)."""
    if encoder == VAAPI_ENCODER:
        return {"vaapi_device": VAAPI_RENDER_NODE, "rate_control": "CBR", "bitrate": STREAM_BITRATE_KBPS,
                "keyint_sec": STREAM_KEYINT_S, "bf": 0, "profile": H264_HIGH_PROFILE}
    if encoder == NVENC_ENCODER:
        return {"rate_control": "CBR", "bitrate": STREAM_BITRATE_KBPS, "keyint_sec": STREAM_KEYINT_S,
                "bf": 0, "profile": "high"}
    raise ValueError("unknown stream encoder %r" % encoder)


def basic_ini_text(existing_text, encoder, profile_name):
    """Upsert the stream keys into a basic.ini text (None = no file yet); every other key is kept.
    Raises ValueError on a file configparser cannot read (never clobber it)."""
    cp = configparser.RawConfigParser(strict=False)
    cp.optionxform = str
    if existing_text:
        try:
            cp.read_string(existing_text)
        except configparser.Error as e:
            raise ValueError("basic.ini unreadable (%s) -- left untouched" % e)
    sections = dict(BASIC_INI)
    sections["General"] = {"Name": profile_name}
    sections["AdvOut"] = dict(ADVOUT_RESCALE, Encoder=encoder)
    for sec, kv in sections.items():
        if not cp.has_section(sec):
            cp.add_section(sec)
        for k, v in kv.items():
            cp.set(sec, k, v)
    buf = io.StringIO()
    cp.write(buf, space_around_delimiters=False)
    return buf.getvalue()


def service_json(existing, server):
    """The service.json dict: `rtmp_custom` to SERVER. Only the server is written; an existing key and
    every other setting are kept, and no key is ever added."""
    d = dict(existing) if isinstance(existing, dict) else {}
    settings = dict(d.get("settings") or {}) if isinstance(d.get("settings"), dict) else {}
    if d.get("type") != "rtmp_custom":
        # another service type carries its own server/key meaning: start a clean custom service, but
        # keep a key the owner already pasted.
        settings = {k: v for k, v in settings.items() if k == "key"}
    d["type"] = "rtmp_custom"
    settings["server"] = server
    settings.setdefault("use_auth", False)
    d["settings"] = settings
    return d


def _write(path, text, mode):
    """Temp file created WITH its final mode (never briefly world-readable), then renamed."""
    tmp = path + ".tmp-1412"
    if os.path.exists(tmp):
        os.unlink(tmp)
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(fd, "w") as fh:
        fh.write(text)
    os.chmod(tmp, mode)
    os.replace(tmp, path)


def _read(path):
    """A file OBS wrote, possibly with the UTF-8 BOM a Windows OBS writes; None when absent."""
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8-sig") as fh:
        return fh.read()


def apply(profile_dir, encoder, server, profile_name):
    """Write the three files under PROFILE_DIR (created). Returns the lines to log."""
    os.makedirs(profile_dir, exist_ok=True)
    ini = os.path.join(profile_dir, "basic.ini")
    svc_path = os.path.join(profile_dir, "service.json")
    # parse every input BEFORE writing anything: a bad file leaves the whole profile untouched.
    ini_text = basic_ini_text(_read(ini), encoder, profile_name)
    svc_text = _read(svc_path)
    existing = None
    if svc_text is not None:
        try:
            existing = json.loads(svc_text)
        except ValueError:
            raise ValueError("service.json unreadable -- the profile is left untouched")
    svc = service_json(existing, server)
    _write(ini, ini_text, 0o644)
    _write(os.path.join(profile_dir, "streamEncoder.json"),
           json.dumps(stream_encoder_settings(encoder), indent=4) + "\n", 0o644)
    _write(svc_path, json.dumps(svc, indent=4) + "\n", 0o600)
    return ["basic.ini: Advanced output, %s, 1920x1080 @ 30 fps NV12" % encoder,
            "streamEncoder.json: CBR %d kb/s, keyint %d s, High" % (STREAM_BITRATE_KBPS, STREAM_KEYINT_S),
            "service.json: rtmp_custom %s (stream key %s)" % (server, "kept" if svc["settings"].get("key") else "not set -- the owner pastes it")]


def grade_drift(profile_dir):
    """The written profile's drift from the stream keys this tool owns beyond the encoder name (the
    4K rescale + CBR / bitrate / keyint), one text per key; an empty list = no drift. Read-only."""
    out = []
    cp = configparser.RawConfigParser(strict=False)
    cp.optionxform = str
    try:
        cp.read(os.path.join(profile_dir, "basic.ini"))
    except configparser.Error as e:
        return ["basic.ini unreadable (%s)" % e]
    adv = cp["AdvOut"] if cp.has_section("AdvOut") else {}
    out += ["%s=%s (want %s)" % (k, adv.get(k), v) for k, v in ADVOUT_RESCALE.items() if adv.get(k) != v]
    try:
        with open(os.path.join(profile_dir, "streamEncoder.json")) as fh:
            enc = json.load(fh)
    except (OSError, ValueError) as e:
        return out + ["streamEncoder.json unreadable (%s)" % e]
    for k, v in (("rate_control", "CBR"), ("bitrate", STREAM_BITRATE_KBPS), ("keyint_sec", STREAM_KEYINT_S)):
        if enc.get(k) != v:
            out.append("streamEncoder.json %s=%s (want %s)" % (k, enc.get(k), v))
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--grade", metavar="PROFILE_DIR", default=None,
                    help="read-only: print the profile's drift (nothing printed = no drift)")
    ap.add_argument("--profile-dir")
    ap.add_argument("--profile-name")
    ap.add_argument("--has-nvidia", choices=("0", "1"))
    ap.add_argument("--server", default=DEFAULT_RTMP_SERVER)
    a = ap.parse_args(argv)
    if a.grade is not None:
        drift = grade_drift(a.grade)
        if drift:
            print(", ".join(drift))
        return
    if not (a.profile_dir and a.profile_name and a.has_nvidia):
        ap.error("--profile-dir, --profile-name and --has-nvidia are required (or --grade DIR)")
    try:
        lines = apply(a.profile_dir, encoder_for(a.has_nvidia == "1"), a.server, a.profile_name)
    except (ValueError, OSError) as e:
        sys.exit("FAIL issue 1412: %s" % e)
    for ln in lines:
        print("  " + ln)


if __name__ == "__main__":
    main()
