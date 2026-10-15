#!/usr/bin/env python3
"""Port an OBS scene collection to a Linux OBS box (issue 1412, the Poprad rework).

OBS stores a scene collection as one JSON file (`~/.config/obs-studio/basic/scenes/<name>.json`).
This tool rewrites that JSON ONCE, before the import, while OBS is stopped. It never talks to OBS
and never writes anything but its output file. The result is imported once and never rewritten by
this tool afterwards (the CLI refuses an existing output file).

Three uses, as presets (every preset value can be overridden on the command line):

* `--preset stream-pp`: the Windows stream-pp collection -> the Linux stream box.
  - keeps the scenes PRO / Development / PRE / POST and every scene and source they nest; drops the rest;
  - the program ASIO input `Zvuk z mixu` becomes `pulse_input_capture` on `strih-program-source`
    (the same binding `strih_scenes.py` seeds);
  - `NDI® 2ME PGM` (also spelled `NDIAr` / `NDIÂ®` / `NDI(R)`) <- `OBS-PP (2ME PGM)` becomes `NDI 2ME PGM` <- `STRIH-PP (2ME PGM)` (the input the
    stream seed manifest declares);
  - drops the YouTube player (`ytfast*`) and every input / filter / module the Linux bundle does not
    have; `text_gdiplus` becomes the FreeType2 text source;
  - Windows file paths move under `/srv/obs-media/stream-pp/<folder>/<file>` (the keep-set copy keeps
    those folder names).
* `--preset strih-pp`: the strih-snv collection -> strih-pp.
  - keeps every scene; `STRIH-SNV (...)` -> `STRIH-PP (...)`, the CG sender `RESOLUME-SNV (SP-program)`
    -> `RESOLUME-PP (SP-program)`; drops the shader filter and Windows-path scripts.
* `--preset strih-snv` (issue 1414): the IN-PLACE rename of the SNV strih's own collection when the
  box takes its venue name -- the self-loop inputs follow the new NDI prefix, nothing else changes
  (every scene, input and shader filter kept). Map the two SNV hosts to themselves:
  `--presenter-host presenter.lan --fohabl-host fohabl.lan`.

Browser URLs that name an SNV-only host (`presenter.lan`, `fohabl.lan`) must be mapped:
`--presenter-host H`, `--fohabl-host H` or `--url-host OLD=NEW`. An unmapped one refuses (map a host
to itself to keep it). The PP presenter address is not settled yet, so it is never guessed here.

The report (stderr, or `--report FILE`) has one line per change: DROP / CONVERT / RENAME / SENDER /
URL / PATH / MODULE / NOTE. It never prints a setting value that may be secret: a URL is printed as
its host only, a script by its file name only.

Usage:
    python3 scripts/obs_collection_port.py --preset stream-pp --presenter-host <host> IN.json OUT.json
    python3 scripts/obs_collection_port.py --preset strih-pp --presenter-host <host> \\
        --fohabl-host <host> IN.json OUT.json
"""

import argparse
import copy
import json
import ntpath
import os
import re
import sys
from urllib.parse import urlsplit, urlunsplit

# The program-audio binding, pinned equal to scripts/strih_scenes.py by the pytest (strih_scenes
# imports websocket at load, so this pure tool does not import it).
PROGRAM_AUDIO_KIND = "pulse_input_capture"
PROGRAM_AUDIO_SETTINGS = {"device_id": "strih-program-source"}

# Input kinds the Linux strih genlock bundle provides (unversioned OBS ids). Anything else is dropped.
LINUX_INPUT_KINDS = frozenset({
    "scene", "group", "ndi_source", "browser_source", "image_source", "color_source", "slideshow",
    "ffmpeg_source", "text_ft2_source", "pulse_input_capture", "pulse_output_capture",
    "alsa_input_capture", "jack_output_capture", "v4l2_input", "xshm_input", "xcomposite_input",
    "pipewire-desktop-capture-source", "pipewire-window-capture-source", "pipewire-screen-capture-source",
})

# Filter kinds the Linux bundle provides: OBS's own obs-filters + the vendored DistroAV's filters (the NDI
# output filters, the premultiplied-alpha filter and the measurement QR burn, vendor/distroav/src).
LINUX_FILTER_KINDS = frozenset({
    "premultiplied_alpha_filter", "distroav_qr_burn_filter",
    "crop_filter", "scale_filter", "scroll_filter", "gpu_delay", "color_filter", "color_key_filter",
    "chroma_key_filter", "luma_key_filter", "mask_filter", "sharpness_filter", "clut_filter",
    "async_delay_filter", "gain_filter", "invert_polarity_filter", "noise_gate_filter",
    "noise_suppress_filter", "compressor_filter", "expander_filter", "upward_compressor_filter",
    "limiter_filter", "basic_eq_filter", "hdr_tonemap_filter", "ndi_filter", "ndi_audiofilter",
})

# Transition kinds the Linux bundle provides (obs-transitions).
LINUX_TRANSITION_KINDS = frozenset({
    "cut_transition", "fade_transition", "swipe_transition", "slide_transition", "obs_stinger_transition",
    "fade_to_color_transition", "wipe_transition",
})

# Frontend modules that are Windows-only or belong to a plugin the Linux bundle does not ship.
DROPPED_MODULES = {
    "captions": "Windows-only (captions)",
    "advanced-scene-switcher": "plugin not in the Linux bundle",
}

# The collection's global audio devices (top-level keys, not in `sources`).
GLOBAL_AUDIO_KEYS = ("DesktopAudioDevice1", "DesktopAudioDevice2", "AuxAudioDevice1", "AuxAudioDevice2",
                     "AuxAudioDevice3", "AuxAudioDevice4")

# Browser URL hosts that exist only at SNV: a port must map each one it meets.
MUST_MAP_HOSTS = ("presenter.lan", "fohabl.lan")

_WIN_PATH = re.compile(r"^[A-Za-z]:[\\/]")
# a Windows drive path or a UNC path (\\\\server\\share) as a JSON string value
_WIN_PATH_IN_JSON = re.compile(r'"([A-Za-z]:(/|\\\\)|\\\\\\\\)')

PRESETS = {
    "stream-pp": {
        "name": "stream-pp",
        "keep_scenes": ["PRO", "Development", "PRE", "POST"],
        "program_audio_input": "Zvuk z mixu",
        # The Windows collection spells it with a real U+00AE (the recon report printed it "NDIAr");
        # source_name_spellings matches the other spellings of the same name too.
        "source_renames": {"NDI\u00ae 2ME PGM": "NDI 2ME PGM"},
        "sender_prefixes": {"OBS-PP": "STRIH-PP"},
        "sender_renames": {},
        "foreign_senders": [r"-SNV \(", r"^OBS-PP \("],
        "drop_patterns": [r"^ytfast"],
        "drop_sources": [],
        "media_dir": "/srv/obs-media/stream-pp",
        "path_prefixes": {},
        "url_hosts": {},
        "keep_kinds": [],
    },
    "strih-pp": {
        "name": "strih-pp",
        "keep_scenes": None,
        "program_audio_input": "ASIO zvuk",
        "source_renames": {},
        "sender_prefixes": {"STRIH-SNV": "STRIH-PP", "STRIH-LX": "STRIH-PP"},  # issue 1414: + the pre-rename prefix
        "sender_renames": {"RESOLUME-SNV (SP-program)": "RESOLUME-PP (SP-program)"},
        "foreign_senders": [r"-SNV \(", r"^STRIH-SNV \(", r"^STRIH-LX \("],  # issue 1414: + the pre-rename prefix
        "drop_patterns": [r"^ytfast"],
        "drop_sources": [],
        "media_dir": "/srv/obs-media/strih-pp",
        "path_prefixes": {},
        "url_hosts": {},
        "keep_kinds": [],
    },
    # issue 1414: the IN-PLACE rename of the SNV strih's own collection (the pre-1414 strih-lx one ->
    # strih-snv): the self-loop inputs follow the box's new NDI prefix, nothing else changes -- every
    # scene and input stays, the program audio is already the Linux binding, and the operator's shader
    # filters are kept (a rename must drop nothing: read the report for any DROP line before importing).
    "strih-snv": {
        "name": "strih-snv",
        "keep_scenes": None,
        "program_audio_input": None,
        "source_renames": {},
        "sender_prefixes": {"STRIH-LX": "STRIH-SNV"},  # issue 1414: the pre-rename prefix
        "sender_renames": {},
        "foreign_senders": [r"^STRIH-LX \("],  # issue 1414: a pre-rename sender left behind
        "drop_patterns": [],
        "drop_sources": [],
        "media_dir": None,
        "path_prefixes": {},
        "url_hosts": {},
        "keep_kinds": ["shader_filter"],
    },
}


class PortError(Exception):
    """A port that cannot be done safely (the caller writes nothing)."""


def preset_options(name):
    """A fresh, mutable copy of preset NAME's options."""
    if name not in PRESETS:
        raise PortError("unknown preset %r (known: %s)" % (name, ", ".join(sorted(PRESETS))))
    return copy.deepcopy(PRESETS[name])


def _all(coll):
    return list(coll.get("sources", [])) + list(coll.get("groups", []))


def _items(src):
    return (src.get("settings") or {}).get("items") or []


def _is_container(src):
    return src.get("id") in ("scene", "group")


def _drop_names(coll, names, rep, reason_of):
    """Remove the sources NAMES (and their scene items, scene_order entries) from COLL."""
    if not names:
        return
    for key in ("sources", "groups"):
        kept = []
        for s in coll.get(key, []):
            if s.get("name") in names:
                rep.append('DROP source "%s" %s: %s' % (s["name"], s.get("id"), reason_of(s)))
            else:
                kept.append(s)
        coll[key] = kept
    _prune_items(coll)


def _prune_items(coll):
    """Remove every scene/group item whose source no longer exists, and stale scene_order entries."""
    have = {s.get("name") for s in _all(coll)}
    for s in _all(coll):
        if _is_container(s) and "items" in (s.get("settings") or {}):
            s["settings"]["items"] = [i for i in _items(s) if i.get("name") in have]
    coll["scene_order"] = [e for e in coll.get("scene_order", []) if e.get("name") in have]


def _reachable(coll, roots):
    by_name = {s.get("name"): s for s in _all(coll)}
    seen, stack = set(), [r for r in roots if r in by_name]
    while stack:
        n = stack.pop()
        if n in seen:
            continue
        seen.add(n)
        src = by_name.get(n)
        if src is not None and _is_container(src):
            stack.extend(i.get("name") for i in _items(src) if i.get("name") not in seen)
    return seen


def _convert_program_audio(coll, name, rep):
    src = next((s for s in _all(coll) if s.get("name") == name), None)
    if src is None:
        raise PortError('the program audio input "%s" is not in the collection' % name)
    old = (src.get("id"), src.get("settings"))
    src["id"] = src["versioned_id"] = PROGRAM_AUDIO_KIND
    src["settings"] = dict(PROGRAM_AUDIO_SETTINGS)
    if old != (PROGRAM_AUDIO_KIND, PROGRAM_AUDIO_SETTINGS):
        rep.append('CONVERT source "%s" %s -> %s on %s' % (name, old[0], PROGRAM_AUDIO_KIND,
                                                          PROGRAM_AUDIO_SETTINGS["device_id"]))


def gdiplus_to_ft2(settings):
    """Pure: the FreeType2 text settings for a text_gdiplus SETTINGS dict (colours are 0xAABBGGRR)."""
    s = settings or {}
    out = {"text": s.get("text", "")}
    if "font" in s:
        out["font"] = s["font"]
    # integer arithmetic like the C source (opacity 50 -> alpha 127)
    alpha = 255 * max(0, min(100, int(s.get("opacity", 100)))) // 100
    colour = (alpha << 24) | (int(s.get("color", 0xFFFFFF)) & 0xFFFFFF)
    out["color1"] = out["color2"] = colour
    for src_key, dst_key in (("outline", "outline"), ("read_from_file", "from_file"), ("file", "text_file")):
        if src_key in s:
            out[dst_key] = s[src_key]
    if s.get("extents"):
        out["word_wrap"] = bool(s.get("extents_wrap", False))
        if "extents_cx" in s:
            out["custom_width"] = s["extents_cx"]
    return out


def _convert_kinds(coll, rep):
    for s in _all(coll):
        if s.get("id") == "text_gdiplus":
            s["settings"] = gdiplus_to_ft2(s.get("settings"))
            s["id"], s["versioned_id"] = "text_ft2_source", "text_ft2_source_v2"
            rep.append('CONVERT source "%s" text_gdiplus -> text_ft2_source (gradient, outline colour/size,'
                       ' background colour/opacity, alignment, vertical text and the chat-log mode are not'
                       ' carried)' % s["name"])


# The spellings a ported collection may carry for a registered sign (U+00AE) in a source name: the
# sign itself, its UTF-8 bytes read as Latin-1 ("\u00c2\u00ae"), the ASCII "(R)", and the "Ar" the
# recon report of the Windows stream collection printed (issue 1412).
REGISTERED_SIGN_SPELLINGS = ("\u00ae", "\u00c2\u00ae", "(R)", "Ar")


def source_name_spellings(name):
    """Pure: NAME first, then every other spelling of its registered signs (none when it has none)."""
    out = [name]
    if "\u00ae" in name:
        for alt in REGISTERED_SIGN_SPELLINGS[1:]:
            v = name.replace("\u00ae", alt)
            if v not in out:
                out.append(v)
    return out


def _rename_sources(coll, renames, rep):
    if not renames:
        return
    have = {s.get("name") for s in _all(coll)}
    for key, new in renames.items():
        found = [v for v in source_name_spellings(key) if v in have]
        if not found:
            continue
        if len(found) > 1:
            raise PortError('cannot rename "%s": the collection holds more than one spelling of it (%s)'
                            % (key, ", ".join('"%s"' % f for f in found)))
        old = found[0]
        if new in have:
            raise PortError('cannot rename "%s" to "%s": a source of that name exists' % (old, new))
        for s in _all(coll):
            if s.get("name") == old:
                s["name"] = new
            for i in _items(s):
                if i.get("name") == old:
                    i["name"] = new
        for e in coll.get("scene_order", []):
            if e.get("name") == old:
                e["name"] = new
        for k in ("current_scene", "current_program_scene"):
            if coll.get(k) == old:
                coll[k] = new
        rep.append('RENAME source "%s" -> "%s"' % (old, new))


def rename_sender(value, prefixes, renames):
    """Pure: the NDI sender name VALUE after the exact renames, then the `PREFIX (` prefix renames."""
    if value in renames:
        return renames[value]
    for old, new in prefixes.items():
        if value.startswith(old + " ("):
            return new + value[len(old):]
    return value


def _warn_foreign_senders(coll, patterns, rep):
    """A WARN line for every NDI name still matching a pattern of the venue this port leaves."""
    for s in _all(coll):
        st = s.get("settings") or {}
        names = [("", st.get("ndi_source_name"))] if s.get("id") == "ndi_source" else []
        names += [('filter "%s" on ' % f.get("name"), (f.get("settings") or {}).get("ndi_filter_ndiname"))
                  for f in s.get("filters", []) or []]
        for where, n in names:
            if isinstance(n, str) and any(re.search(p, n) for p in patterns):
                rep.append('WARN sender %s"%s" still names %s (no rename maps it: --rename-sender OLD=NEW)'
                           % (where, s["name"], n))


def _rename_senders(coll, prefixes, renames, rep):
    for s in _all(coll):
        st = s.get("settings") or {}
        if s.get("id") == "ndi_source" and isinstance(st.get("ndi_source_name"), str):
            new = rename_sender(st["ndi_source_name"], prefixes, renames)
            if new != st["ndi_source_name"]:
                rep.append('SENDER "%s": %s -> %s' % (s["name"], st["ndi_source_name"], new))
                st["ndi_source_name"] = new
        for f in s.get("filters", []) or []:
            fs = f.get("settings") or {}
            if isinstance(fs.get("ndi_filter_ndiname"), str):
                new = rename_sender(fs["ndi_filter_ndiname"], prefixes, renames)
                if new != fs["ndi_filter_ndiname"]:
                    rep.append('SENDER filter "%s" on "%s": %s -> %s' % (f.get("name"), s["name"],
                                                                         fs["ndi_filter_ndiname"], new))
                    fs["ndi_filter_ndiname"] = new


def map_url_host(url, hosts):
    """Pure: (new_url, host) -- URL with its host replaced per HOSTS (case-insensitive; the port, user
    info, path and query kept). host is the lower-cased ORIGINAL host, or None for a non-URL."""
    parts = urlsplit(url)
    host = (parts.hostname or "").lower()
    if not parts.scheme or not host:
        return url, None
    if host not in hosts:
        return url, host
    netloc = parts.netloc
    at = netloc.rfind("@") + 1
    rest = netloc[at:]
    port = ""
    if rest.startswith("["):
        port = rest[rest.find("]") + 1:]
    elif ":" in rest:
        port = rest[rest.find(":"):]
    return urlunsplit((parts.scheme, netloc[:at] + hosts[host] + port, parts.path, parts.query,
                       parts.fragment)), host


def _map_urls(coll, hosts, rep):
    hosts = {k.lower(): v for k, v in hosts.items()}
    unmapped = []
    for s in _all(coll):
        st = s.get("settings") or {}
        if s.get("id") != "browser_source" or not isinstance(st.get("url"), str):
            continue
        new, host = map_url_host(st["url"], hosts)
        if host is None:
            continue
        if host in hosts and hosts[host] != host:
            st["url"] = new
            rep.append('URL "%s": host %s -> %s' % (s["name"], host, hosts[host]))
        elif host in MUST_MAP_HOSTS and host not in hosts:
            unmapped.append((s["name"], host))
        else:
            rep.append('NOTE URL "%s": host %s kept' % (s["name"], host))
    if unmapped:
        names = ", ".join('"%s" (%s)' % u for u in unmapped)
        raise PortError("browser URLs name an SNV-only host, map each one (--presenter-host / "
                        "--fohabl-host / --url-host OLD=NEW; map a host to itself to keep it): " + names)


def remap_path(value, media_dir, prefixes):
    """Pure: the path VALUE on the Linux box, or None when it is not a path this port moves.
    A Windows path -> MEDIA_DIR/<parent folder>/<file>; a POSIX path under an OLD prefix -> NEW."""
    if _WIN_PATH.match(value) and media_dir:
        p = value.replace("\\", "/")
        parent, base = ntpath.split(p)
        folder = ntpath.basename(parent)
        return "/".join(x for x in (media_dir.rstrip("/"), folder, base) if x)
    for old, new in prefixes.items():
        if value == old or value.startswith(old.rstrip("/") + "/"):
            return new.rstrip("/") + value[len(old.rstrip("/")):]
    return None


def _walk_paths(obj, media_dir, prefixes, on_change):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if isinstance(v, str):
                new = remap_path(v, media_dir, prefixes)
                if new is not None and new != v:
                    obj[k] = new
                    on_change(v, new)
            else:
                _walk_paths(v, media_dir, prefixes, on_change)
    elif isinstance(obj, list):
        for v in obj:
            _walk_paths(v, media_dir, prefixes, on_change)


def _remap_paths(coll, media_dir, prefixes, rep):
    leftover, targets = [], {}
    for s in _all(coll) + list(coll.get("transitions", []) or []):
        def note(old, new, name=s.get("name")):
            rep.append('PATH "%s": %s -> %s' % (name, ntpath.basename(old.replace("\\", "/")), new))
            targets.setdefault(new, set()).add(old)
        _walk_paths(s.get("settings"), media_dir, prefixes, note)
        if _WIN_PATH_IN_JSON.search(json.dumps(s.get("settings"))):
            leftover.append(s.get("name"))
    for n in leftover:
        rep.append('NOTE "%s" still names a Windows path or a UNC path (not moved: --media-dir / --path-prefix)' % n)
    for new, olds in sorted(targets.items()):
        if len(olds) > 1:
            rep.append("WARN %d different source paths land on %s: copy the right file there" % (len(olds), new))


def _port_modules(coll, drop_patterns, rep):
    mods = coll.get("modules")
    if not isinstance(mods, dict):
        return
    for key in list(mods):
        if key in DROPPED_MODULES:
            del mods[key]
            rep.append("MODULE %s dropped: %s" % (key, DROPPED_MODULES[key]))
    scripts = mods.get("scripts-tool")
    if isinstance(scripts, list):
        kept = []
        for sc in scripts:
            path = str(sc.get("path", ""))
            base = ntpath.basename(path.replace("\\", "/"))
            if _WIN_PATH.match(path):
                rep.append("MODULE scripts-tool: script %s dropped (a Windows path)" % base)
            elif any(re.search(p, base) for p in drop_patterns):
                rep.append("MODULE scripts-tool: script %s dropped (a drop pattern)" % base)
            else:
                kept.append(sc)
        mods["scripts-tool"] = kept


def port(collection, options):
    """Pure: (ported collection, report lines). COLLECTION is never modified; PortError on refusal."""
    o = dict(options)
    coll = copy.deepcopy(collection)
    rep = []
    if not isinstance(coll.get("sources"), list):
        raise PortError("not an OBS scene collection (no `sources` list)")
    keep_kinds = LINUX_INPUT_KINDS | set(o.get("keep_kinds") or [])
    keep_filters = LINUX_FILTER_KINDS | set(o.get("keep_kinds") or [])
    patterns = list(o.get("drop_patterns") or [])

    # 1. explicitly named + pattern drops (the YouTube player).
    named = set(o.get("drop_sources") or [])
    by_pattern = {s["name"] for s in _all(coll) if any(re.search(p, s.get("name", "")) for p in patterns)}
    _drop_names(coll, named | by_pattern,
                rep, lambda s: "named by --drop-source" if s["name"] in named else "matches a drop pattern")

    # 2. the scene keep-set: every kept scene and everything it nests; the rest goes.
    keep = o.get("keep_scenes")
    if keep:
        have = {s.get("name") for s in _all(coll)}
        for k in keep:
            if k not in have:
                rep.append('NOTE keep scene "%s" absent from the collection (a seeder creates it)' % k)
        reach = _reachable(coll, keep)
        _drop_names(coll, {s["name"] for s in _all(coll) if s["name"] not in reach},
                    rep, lambda s: "not used by a kept scene")

    # 3. the program audio input, then the kind conversions, then the kinds Linux does not have.
    if o.get("program_audio_input"):
        _convert_program_audio(coll, o["program_audio_input"], rep)
    _convert_kinds(coll, rep)
    _drop_names(coll, {s["name"] for s in _all(coll) if s.get("id") not in keep_kinds},
                rep, lambda s: "kind not in the Linux bundle")
    for key in GLOBAL_AUDIO_KEYS:
        dev = coll.get(key)
        if isinstance(dev, dict) and dev.get("id") not in keep_kinds:
            del coll[key]
            rep.append('DROP global audio %s "%s" %s: kind not in the Linux bundle'
                       % (key, dev.get("name"), dev.get("id")))
    trans = []
    for t in coll.get("transitions", []) or []:
        if t.get("id") in LINUX_TRANSITION_KINDS | set(o.get("keep_kinds") or []):
            trans.append(t)
        else:
            rep.append('DROP transition "%s" %s: kind not in the Linux bundle' % (t.get("name"), t.get("id")))
    if "transitions" in coll:
        coll["transitions"] = trans
    order = [e["name"] for e in coll.get("scene_order", [])]
    for k in ("current_scene", "current_program_scene"):
        if coll.get(k) not in order and order:
            pick = next((n for n in (keep or []) if n in order), order[0])
            rep.append('NOTE %s "%s" -> "%s"' % (k, coll.get(k), pick))
            coll[k] = pick

    # 4. filters of plugins the bundle lacks (sources, groups and the global audio devices).
    for s in _all(coll) + [coll[k] for k in GLOBAL_AUDIO_KEYS if isinstance(coll.get(k), dict)]:
        kept = []
        for f in s.get("filters", []) or []:
            if f.get("id") in keep_filters:
                kept.append(f)
            else:
                rep.append('DROP filter "%s" %s on "%s": kind not in the Linux bundle'
                           % (f.get("name"), f.get("id"), s["name"]))
        if "filters" in s:
            s["filters"] = kept

    # 5. names, senders, URLs, paths, modules.
    _rename_sources(coll, o.get("source_renames") or {}, rep)
    _rename_senders(coll, o.get("sender_prefixes") or {}, o.get("sender_renames") or {}, rep)
    _warn_foreign_senders(coll, o.get("foreign_senders") or [], rep)
    _map_urls(coll, o.get("url_hosts") or {}, rep)
    _remap_paths(coll, o.get("media_dir"), o.get("path_prefixes") or {}, rep)
    _port_modules(coll, patterns, rep)
    if o.get("name"):
        coll["name"] = o["name"]
    return coll, rep


def _pairs(values, flag):
    out = {}
    for v in values or []:
        if "=" not in v:
            raise PortError("%s wants OLD=NEW, got %r" % (flag, v))
        old, new = v.split("=", 1)
        out[old] = new
    return out


def options_from_args(a):
    o = preset_options(a.preset)
    if a.name:
        o["name"] = a.name
    if a.keep_scene:
        o["keep_scenes"] = list(a.keep_scene)
    if a.all_scenes:
        o["keep_scenes"] = None
    if a.program_audio_input is not None:
        o["program_audio_input"] = a.program_audio_input or None
    if a.media_dir is not None:
        o["media_dir"] = a.media_dir or None
    o["source_renames"].update(_pairs(a.rename_source, "--rename-source"))
    o["sender_prefixes"].update(_pairs(a.sender_prefix, "--sender-prefix"))
    o["sender_renames"].update(_pairs(a.rename_sender, "--rename-sender"))
    o["path_prefixes"].update(_pairs(a.path_prefix, "--path-prefix"))
    o["drop_sources"] = list(o["drop_sources"]) + list(a.drop_source or [])
    o["drop_patterns"] = list(o["drop_patterns"]) + list(a.drop_pattern or [])
    o["keep_kinds"] = list(o["keep_kinds"]) + list(a.keep_kind or [])
    hosts = _pairs(a.url_host, "--url-host")
    if a.presenter_host:
        hosts["presenter.lan"] = a.presenter_host
    if a.fohabl_host:
        hosts["fohabl.lan"] = a.fohabl_host
    o["url_hosts"].update(hosts)
    return o


def _parser():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("input")
    p.add_argument("output")
    p.add_argument("--preset", required=True, choices=sorted(PRESETS))
    p.add_argument("--name")
    p.add_argument("--keep-scene", action="append")
    p.add_argument("--all-scenes", action="store_true")
    p.add_argument("--program-audio-input")
    p.add_argument("--media-dir")
    p.add_argument("--rename-source", action="append")
    p.add_argument("--sender-prefix", action="append")
    p.add_argument("--rename-sender", action="append")
    p.add_argument("--path-prefix", action="append")
    p.add_argument("--drop-source", action="append")
    p.add_argument("--drop-pattern", action="append")
    p.add_argument("--keep-kind", action="append")
    p.add_argument("--url-host", action="append")
    p.add_argument("--presenter-host")
    p.add_argument("--fohabl-host")
    p.add_argument("--report")
    return p


def main(argv):
    a = _parser().parse_args(argv)
    try:
        if os.path.lexists(a.output):
            raise PortError("%s exists: a ported collection is written once, never rewritten" % a.output)
        with open(a.input, encoding="utf-8-sig") as fh:
            try:
                coll = json.load(fh)
            except ValueError as e:
                raise PortError("%s is not JSON: %s" % (a.input, e))
        out, rep = port(coll, options_from_args(a))
    except PortError as e:
        print("obs_collection_port: REFUSED: %s" % e, file=sys.stderr)
        return 2
    text = json.dumps(out, indent=4, ensure_ascii=False) + "\n"
    tmp = "%s.tmp-%d" % (a.output, os.getpid())
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(text)
        os.link(tmp, a.output)  # fails if the output appeared meanwhile: never overwritten
    finally:
        os.unlink(tmp)
    lines = rep + ["SUMMARY %d change(s); wrote %s (import it with OBS stopped)" % (len(rep), a.output)]
    if a.report:
        with open(a.report, "w", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    print("\n".join(lines), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
