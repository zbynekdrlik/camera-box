//! issue 1424 (owner 13.10.2026, design 6147051141 + the owner's ROZHODNUTE of the same day) -- the
//! fixed HDMI output plays the OBS program mix on the SAME connector's HDMI audio by itself, straight
//! into that connector's ALSA hw PCM (no sound server, no monitoring device, no mixer layer, no
//! per-box setting); no match / two matches / a PCM that will not open = no audio + ONE named line.
//!
//! Facet A pins the wiring with std-only source anchors: the new libobs TU
//! `obs-drm-output-audio.c` (Linux-only, built by `libobs/cmake/os-linux.cmake`, alsa-lib dlopen'd),
//! the raw callback on mix 0 written to `hw:<card>,<device>`, the per-PCM ALSA `ELD` control + the
//! RandR EDID property as the two match sources, the PCM's IEC958 switch, the bounded re-read window
//! (the NVIDIA HDA driver publishes a connector's ELD only once a mode is driven on it), the
//! start-after-the-backend + stop wiring, and the present-delay notes of both backends.
//!
//! Facet B is the C-vs-Rust parity gate: the PURE header `obs-drm-output-audio-match.h` is compiled
//! for real (`-Wall -Wextra -Wconversion -Werror`) into a harness that prints every decision over
//! the strih-pp fixtures (`tests/fixtures/drm_output_audio_1424.tsv`) and a grid of placement
//! vectors; the Rust twin `camera_box::drm_output_audio_match` must print the same lines. It FAILS
//! LOUDLY when no C compiler is present (never skips).

use camera_box::drm_output_audio_match as m;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Each compile gets its own `{tag}-{pid}-{counter}` dir (the issue-1409 std-only pattern).
static SCRATCH: AtomicUsize = AtomicUsize::new(0);

const AUDIO_C: &str = "vendor/obs-studio/libobs/obs-drm-output-audio.c";
const AUDIO_H: &str = "vendor/obs-studio/libobs/obs-drm-output-audio.h";
const MATCH_H: &str = "vendor/obs-studio/libobs/obs-drm-output-audio-match.h";
const DRM_C: &str = "vendor/obs-studio/libobs/obs-drm-output.c";
const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
/// issue 1424 part 2: each output instance starts / stops its own leg in the registry TU, and the
/// vk-direct OBS side forwards the core's present to the instance's clock.
const OUTPUTS_C: &str = "vendor/obs-studio/libobs/obs-drm-output-outputs.c";
const BACKEND_C: &str = "vendor/obs-studio/libobs/obs-drm-output-backend.c";
const LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";
const FIXTURES: &str = "tests/fixtures/drm_output_audio_1424.tsv";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
}

/// The body of the C function whose definition starts with `sig` (up to the first `\n}\n`).
fn body_of_c(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1424: `{sig}` not found"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("closing brace");
    src[start..end].to_string()
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The fixture rows: (kind, name) -> bytes.
fn fixture(kind: &str, name: &str) -> Vec<u8> {
    for line in file(FIXTURES).lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert!(cols.len() >= 3, "issue 1424: bad fixture row {line:?}");
        if cols[0] == kind && cols[1] == name {
            return (0..cols[2].len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&cols[2][i..i + 2], 16).expect("hex"))
                .collect();
        }
    }
    panic!("issue 1424: fixture {kind}/{name} missing from {FIXTURES}");
}

// ------------------------------------------------------------------------------------------------
// Facet A -- the wiring
// ------------------------------------------------------------------------------------------------

#[test]
fn audio_tu_is_linux_only_and_built_by_libobs_linux_cmake() {
    let c = file(AUDIO_C);
    assert!(
        c.contains("#if defined(__linux__)"),
        "issue 1424: the audio TU must be Linux-only"
    );
    let cm = file(LINUX_CMAKE);
    for f in [
        "obs-drm-output-audio.c",
        "obs-drm-output-audio.h",
        "obs-drm-output-audio-match.h",
    ] {
        assert!(cm.contains(f), "issue 1424: os-linux.cmake must build {f}");
    }
    // alsa-lib is dlopen'd at run time: no link dependency (the vk-direct Vulkan loader pattern)
    assert!(
        !cm.contains("ALSA::ALSA"),
        "issue 1424: no link-time alsa-lib"
    );
    assert!(
        c.contains("dlopen(\"libasound.so.2\""),
        "issue 1424: alsa-lib is dlopen'd"
    );
    assert!(c.contains("dlsym("));
}

#[test]
fn audio_writes_mix_0_straight_to_the_hw_pcm_never_pipewire_or_the_monitoring_device() {
    let c = file(AUDIO_C);
    let s = squish(&c);
    assert!(
        s.contains("obs_add_raw_audio_callback(0,"),
        "issue 1424: the program mix = mix 0"
    );
    assert!(s.contains("obs_remove_raw_audio_callback(0,"));
    // the ALSA hw PCM of the matched (card, device), opened non-blocking, written in place
    assert!(
        c.contains("\"hw:%d,%d\""),
        "issue 1424: the hw PCM, never a plug or a sound server"
    );
    assert!(c.contains("snd_pcm_writei") && c.contains("snd_pcm_delay"));
    assert!(
        c.contains("SND_PCM_NONBLOCK"),
        "issue 1424: the audio thread never blocks on the PCM"
    );
    // no layer in between, no fallback
    for banned in [
        "pulseaudio",
        "pa_stream",
        "pipewire",
        "monitoring_device_id",
        "get_default_id",
        "\"default\"",
        "plughw",
        "dmix",
    ] {
        assert!(
            !c.to_lowercase().contains(&banned.to_lowercase()),
            "issue 1424: no sound server, no monitoring device, no plug/mixer layer, no default device ({banned})"
        );
    }
    assert!(
        !c.contains("drm-output.json"),
        "issue 1424: no per-box audio setting"
    );
}

#[test]
fn audio_matches_the_connector_edid_against_the_per_pcm_eld_control() {
    let c = file(AUDIO_C);
    assert!(c.contains("#include \"obs-drm-output-audio-match.h\""));
    // the EDID: the X RandR output property, read by output name (works for lease AND vk-direct)
    assert!(c.contains("xcb_randr_get_output_property"));
    assert!(c.contains("\"EDID\""));
    // the ELD: the per-PCM ALSA control (the kernel's own pcm->pin map), never a /proc eld file
    assert!(c.contains("SNDRV_CTL_IOCTL_ELEM_LIST") && c.contains("SNDRV_CTL_IOCTL_ELEM_READ"));
    assert!(c.contains("SNDRV_CTL_ELEM_IFACE_PCM"));
    assert!(c.contains("\"ELD\""));
    assert!(
        !c.contains("/proc/asound"),
        "issue 1424: the /proc eld index is not a PCM number"
    );
    assert!(c.contains("drm_output_audio_pick_pcm("));
    // the matched PCM's IEC958 switch is turned on (no sound server unmutes it any more)
    assert!(c.contains("\"IEC958 Playback Switch\"") && c.contains("SNDRV_CTL_IOCTL_ELEM_WRITE"));
    assert!(c.contains("drm_output_audio_iec958_index("));
    // the bounded re-read window, then ONE named line
    assert!(c.contains("DRM_OUTPUT_AUDIO_RESOLVE_WINDOW_MS"));
    assert!(c.contains("drm_output_audio_retryable("));
    assert!(c.contains("drm-output audio: NO AUDIO on '%s'"));
    // the resolver's own open line (written in the Multiview view too, where no block is placed)
    assert!(c.contains("drm-output audio: '%s' matched hw:%d,%d"));
    // review Y2: the switch is touched only when the card's switches are exactly its HDMI PCMs
    assert!(c.contains("IEC958 switch ambiguous"));
    // review Y5: the open PCM is re-checked against the connector's monitor (an HDMI replug or a
    // re-mode can move the NVIDIA pin -> PCM map), muted + named while it no longer matches
    assert!(c.contains("drm_output_audio_pcm_still_matches("));
    assert!(c.contains("no longer carries the connector's monitor"));
    // review B2: no allocation on libobs's audio thread, every dropped frame counted
    assert!(c.contains("dropped_frames"));
    let mix = squish(&body_of_c(&c, "static void drm_output_audio_on_mix("));
    assert!(
        !mix.contains("bzalloc(") && !mix.contains("bfree("),
        "issue 1424: the audio thread never allocates"
    );
    assert!(c.contains("drm-output audio: pcm hw:%d,%d card %d device %d delay %.1f ms"));
}

#[test]
fn the_matcher_header_is_pure_std_only_c() {
    // the parity gate compiles it alone, so it must never reach for libobs or a system service
    let h = file(MATCH_H);
    assert!(!h.contains("#include \""), "issue 1424: std headers only");
    for f in [
        "drm_output_edid_monitor_id(",
        "drm_output_eld_monitor_id(",
        "drm_output_audio_pick_pcm(",
        "drm_output_audio_iec958_index(",
        "drm_output_audio_plan_block(",
        "drm_output_present_ewma(",
    ] {
        let at = h
            .find(f)
            .unwrap_or_else(|| panic!("issue 1424: {f} missing"));
        let line_start = h[..at].rfind('\n').map_or(0, |i| i + 1);
        let decl = &h[line_start..at];
        let prev = &h[..line_start];
        let prev_line = prev.trim_end().rsplit('\n').next().unwrap_or("");
        assert!(
            decl.contains("static inline") || prev_line.contains("static inline"),
            "issue 1424: {f} is a static inline helper"
        );
    }
}

#[test]
fn audio_leg_is_a_per_output_instance() {
    let h = file(AUDIO_H);
    let s = squish(&h);
    assert!(s.contains("struct drm_output_audio *drm_output_audio_open("));
    assert!(s.contains("void drm_output_audio_close(struct drm_output_audio *a)"));
    let c = file(AUDIO_C);
    // the callback's state is its instance, never a file-static leg
    assert!(squish(&c).contains("obs_add_raw_audio_callback(0, &conv, drm_output_audio_on_mix, a)"));
    assert!(
        !c.contains("static struct drm_output_audio g_"),
        "issue 1424: no global leg"
    );
}

#[test]
fn audio_is_gated_to_the_program_view() {
    // the Multiview view is the cutter's own monitor: the program audio stays off it
    // issue 1424 part 2: THIS output's view (its own atomic), never the module-wide one
    let c = file(AUDIO_C);
    assert!(c.contains("os_atomic_load_long(a->view) == OBS_DRM_OUTPUT_VIEW_PROGRAM"));
}

#[test]
fn output_start_and_stop_drive_the_audio_leg() {
    // issue 1424 part 2: per output instance. The EDID is read BEFORE the instance's backend takes the
    // connector off X (the lease acquire / the vk-direct acquire both run inside ops->start), and the
    // leg opens only AFTER that start succeeded (the mode is driven, so the connector's ELD can appear).
    let o = squish(&file(OUTPUTS_C));
    let start = o
        .split("bool drm_output_inst_start_locked(struct drm_output_inst *inst)")
        .nth(1)
        .expect("start");
    let start = start
        .split("void drm_output_inst_stop_locked(")
        .next()
        .unwrap();
    let edid = start
        .find("drm_output_audio_read_edid(")
        .expect("issue 1424: start reads the EDID");
    let backend = start
        .find("inst->ops->start(inst)")
        .expect("the backend start (lease or vk-direct)");
    let open = start.find("drm_output_audio_open(").expect("the leg opens");
    assert!(
        edid < backend && backend < open,
        "issue 1424: EDID -> the backend takes the connector -> the leg opens"
    );
    let d = squish(&file(DRM_C));
    assert!(
        !d.contains("drm_output_audio_open(") && !d.contains("drm_output_audio_read_edid("),
        "issue 1424 part 2: the backends never open a leg themselves (one leg per instance)"
    );
    let stop = o
        .split("void drm_output_inst_stop_locked(struct drm_output_inst *inst)")
        .nth(1)
        .expect("stop");
    let stop = stop.split("bool obs_drm_output_start(").next().unwrap();
    let close = stop
        .find("drm_output_audio_close(")
        .expect("issue 1424: stop closes the leg");
    let backend_stop = stop.find("inst->ops->stop(inst);").unwrap();
    assert!(
        close < backend_stop,
        "issue 1424: the audio closes before the picture goes"
    );
}

#[test]
fn both_backends_measure_the_present_delay() {
    let o = squish(&file(OUTPUTS_C));
    let publish = o
        .split("void drm_output_publish_render_buf(struct drm_output_inst *inst, int idx)")
        .nth(1)
        .expect("publish");
    let publish = publish
        .split("gs_texture_t *drm_output_render_buf_texture(")
        .next()
        .unwrap();
    assert!(
        publish.contains("drm_output_present_note_published(&inst->clock, idx)"),
        "issue 1424: the ONE publish entry both backends use records the frame's render tick on THAT \
         output's clock"
    );
    let d = squish(&file(DRM_C));
    let flip = d
        .split("static void *drm_output_flip_thread(")
        .nth(1)
        .expect("flip");
    assert!(
        flip.contains("drm_output_present_note_shown(&d->inst->clock, pnext"),
        "issue 1424: the lease flip event"
    );
    let vk = squish(&file(VK_C));
    let present = vk
        .split("static void *drm_output_vk_present_thread(")
        .nth(1)
        .expect("present");
    assert!(
        present.contains("d->shown(d->shown_param, src"),
        "issue 1424: the vk-direct present reports to its owning output"
    );
    let b = squish(&file(BACKEND_C));
    assert!(
        b.contains("drm_output_present_note_shown(&inst->clock, idx, shown_ns)")
            && b.contains(
                "drm_output_vk_open(inst->index, inst->connector, inst->solid_argb, vkd_note_shown, inst)"
            ),
        "issue 1424: the vk-direct OBS side wires the core's present note to the instance's clock"
    );
}

// ------------------------------------------------------------------------------------------------
// Facet B -- the C header vs the Rust twin
// ------------------------------------------------------------------------------------------------

fn c_bytes(v: &[u8]) -> String {
    if v.is_empty() {
        return "{0}".to_string();
    }
    format!(
        "{{{}}}",
        v.iter()
            .map(|b| format!("0x{b:02x}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn id_line(tag: &str, id: &m::MonitorId) -> String {
    format!(
        "{tag} valid={} vendor={:02x}{:02x}{:02x}{:02x} name=[{}]",
        u8::from(id.valid),
        id.vendor[0],
        id.vendor[1],
        id.vendor[2],
        id.vendor[3],
        id.name
    )
}

/// The byte blobs both sides parse: the fixtures plus the shapes that must fail.
fn blobs() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let benq = fixture("edid", "benq-hdmi0");
    let bmd = fixture("edid", "blackmagic-hdmi1");
    let eld_benq = fixture("eld", "benq-card0-pcm3");
    let eld_bmd = fixture("eld", "blackmagic-card0-pcm7");
    let mut bad_sum = bmd.clone();
    bad_sum[20] ^= 0x01;
    let mut bad_hdr = benq.clone();
    bad_hdr[0] = 0x01;
    let mut eld_partial = eld_benq.clone();
    eld_partial[0] = 31 << 3;
    let mut eld_long_name = eld_benq.clone();
    eld_long_name[4] = (eld_long_name[4] & 0xE0) | 0x1F;
    let eld_short = eld_benq[..19].to_vec();
    // a monitor name padded with spaces (as an EDID descriptor pads it): read back trimmed
    let mut eld_padded = eld_benq.clone();
    eld_padded[4] = (eld_padded[4] & 0xE0) | 10;
    eld_padded[28] = b' ';
    eld_padded[29] = b' ';
    vec![
        ("edid", "benq", benq.clone()),
        ("edid", "bmd", bmd.clone()),
        ("edid", "short", benq[..127].to_vec()),
        ("edid", "badsum", bad_sum),
        ("edid", "badhdr", bad_hdr),
        ("edid", "empty", Vec::new()),
        ("eld", "benq", eld_benq),
        ("eld", "bmd", eld_bmd),
        ("eld", "partial", eld_partial),
        ("eld", "longname", eld_long_name),
        ("eld", "short", eld_short),
        ("eld", "padded", eld_padded),
        ("eld", "empty", Vec::new()),
    ]
}

/// (card, device, eld blob name) sets for pick_pcm, and the EDID that is wanted.
type PcmScenario = (
    &'static str,
    &'static str,
    &'static [(i32, i32, &'static str)],
);

fn pcm_scenarios() -> Vec<PcmScenario> {
    vec![
        // strih-pp today: the BenQ on HDMI-0 is on PCM 3, HDMI-1's ELD is not published yet
        (
            "benq-today",
            "benq",
            &[(0, 3, "benq"), (0, 7, "empty"), (0, 8, "empty")],
        ),
        (
            "bmd-before-mode",
            "bmd",
            &[(0, 3, "benq"), (0, 7, "empty"), (0, 8, "empty")],
        ),
        // once a mode is driven on HDMI-1 its ELD appears
        (
            "bmd-after-mode",
            "bmd",
            &[(0, 3, "benq"), (0, 7, "bmd"), (0, 8, "empty")],
        ),
        ("benq-both", "benq", &[(0, 3, "benq"), (0, 7, "bmd")]),
        ("no-monitor", "benq", &[(0, 3, "empty"), (1, 3, "short")]),
        ("no-edid", "badsum", &[(0, 3, "benq")]),
        // two identical monitors: no guess
        ("twins", "benq", &[(0, 3, "benq"), (0, 7, "benq")]),
        ("none", "benq", &[]),
    ]
}

type IecVector = (&'static [i32], i32, usize);

/// (the card's ELD-carrying PCM devices, the matched device, the card's IEC958 switch count).
const IEC958_VECTORS: &[IecVector] = &[
    (&[3, 7, 8, 9], 3, 4),
    (&[3, 7, 8, 9], 9, 4),
    (&[9, 8, 7, 3], 8, 4),
    (&[3, 7, 7], 7, 2),
    (&[3, 7], 0, 2),
    (&[], 3, 0),
    // an analog codec's own S/PDIF switch on the same card: which index is HDMI is unknown -> untouched
    (&[3, 7, 8], 3, 4),
    (&[3, 7, 8], 7, 2),
];

type PlanVector = (u64, u64, u64, u64, u64, u64, u64, u64);

fn plan_vectors() -> Vec<PlanVector> {
    const MS: u64 = 1_000_000;
    let now = 86_400_000 * MS;
    let blk = 21_333_333;
    let band = 5 * MS;
    let mut v = Vec::new();
    // (block_ts, present_delay, now, pcm delay, floor, threshold, block, band)
    for age in [0, 10 * MS, 85 * MS, 120 * MS] {
        for pd in [0, 16 * MS, 33 * MS, 140 * MS] {
            for lat in [0, 40 * MS, 60 * MS, 150 * MS] {
                v.push((now - age, pd, now, lat, 60 * MS, 42 * MS, blk, band));
            }
        }
    }
    // the early-audio hold, both sides of the threshold (strict)
    v.push((now, 102 * MS, now, 60 * MS, 60 * MS, 42 * MS, blk, band));
    v.push((now, 102 * MS + 1, now, 60 * MS, 60 * MS, 42 * MS, blk, band));
    // a grown buffer, trimmed at most one block
    v.push((
        now - 85 * MS,
        16 * MS,
        now,
        300 * MS,
        60 * MS,
        42 * MS,
        blk,
        band,
    ));
    v.push((
        now - 85 * MS,
        16 * MS,
        now,
        102 * MS,
        60 * MS,
        42 * MS,
        blk,
        band,
    ));
    v.push((
        now - 85 * MS,
        16 * MS,
        now,
        102 * MS + 1,
        60 * MS,
        42 * MS,
        blk,
        band,
    ));
    // clock drift inside the threshold: one frame repeated / dropped outside the band, nothing inside
    // it (the band edge is strict), never a nudge together with a hold or a trim
    for d in [0, 4 * MS, 5 * MS, 5 * MS + 1, 20 * MS, 42 * MS] {
        v.push((
            now - 85 * MS,
            16 * MS,
            now,
            60 * MS + d,
            60 * MS,
            42 * MS,
            blk,
            band,
        ));
        v.push((
            now - 85 * MS,
            16 * MS,
            now,
            60 * MS - d.min(60 * MS),
            60 * MS,
            42 * MS,
            blk,
            band,
        ));
    }
    // no band = no nudge (the pre-review behaviour)
    v.push((
        now - 85 * MS,
        16 * MS,
        now,
        80 * MS,
        60 * MS,
        42 * MS,
        blk,
        0,
    ));
    v
}

const EWMA_VECTORS: &[(u64, u64)] = &[
    (0, 0),
    (0, 16_000_000),
    (16_000_000, 0),
    (16_000_000, 999_999_999),
    (16_000_000, 1_000_000_000),
    (16_000_000, 24_000_000),
    (7, 9),
];

/// What the Rust twin says, in the harness's line format.
fn rust_lines() -> Vec<String> {
    let mut out = Vec::new();
    let bl = blobs();
    let get = |kind: &str, name: &str| {
        bl.iter()
            .find(|b| b.0 == kind && b.1 == name)
            .unwrap()
            .2
            .clone()
    };
    for (kind, name, bytes) in &bl {
        let id = if *kind == "edid" {
            m::edid_monitor_id(bytes)
        } else {
            m::eld_monitor_id(bytes)
        };
        out.push(id_line(&format!("{kind}:{name}"), &id));
    }
    for (tag, want, pcms) in pcm_scenarios() {
        let want_id = m::edid_monitor_id(&get("edid", want));
        let v: Vec<m::AudioPcm> = pcms
            .iter()
            .map(|&(card, device, eld)| m::AudioPcm {
                card,
                device,
                id: m::eld_monitor_id(&get("eld", eld)),
            })
            .collect();
        let (r, idx) = m::pick_pcm(&want_id, &v);
        out.push(format!(
            "pcm:{tag} r={r} idx={} retry={} reason=[{}]",
            idx.map_or(-1, |i| i as i64),
            u8::from(m::retryable(r)),
            m::reason(r)
        ));
    }
    for (i, &(devices, device, switches)) in IEC958_VECTORS.iter().enumerate() {
        out.push(format!(
            "iec:{i} {}",
            m::iec958_index(devices, device, switches).unwrap_or(-1)
        ));
    }
    for (i, &(ts, pd, now, lat, fl, thr, blk, band)) in plan_vectors().iter().enumerate() {
        let p = m::plan_block(ts, pd, now, lat, fl, thr, blk, band);
        out.push(format!(
            "plan:{i} silence={} drop={} offset={} nudge={}",
            p.silence_ns, p.drop_ns, p.offset_ns, p.nudge_frames
        ));
    }
    for &(prev, s) in EWMA_VECTORS {
        out.push(format!("ewma:{prev}:{s} {}", m::present_ewma(prev, s)));
    }
    out
}

/// The same decisions from the compiled C header.
fn c_harness() -> String {
    let mut src = String::from(
        "#include <stdio.h>\n#include <inttypes.h>\n#include \"obs-drm-output-audio-match.h\"\n\
         static void id_line(const char *tag, const struct drm_output_monitor_id *id) {\n\
         printf(\"%s valid=%d vendor=%02x%02x%02x%02x name=[%s]\\n\", tag, id->valid ? 1 : 0, id->vendor[0], id->vendor[1], id->vendor[2], id->vendor[3], id->name);\n}\n\
         int main(void) {\n struct drm_output_monitor_id id;\n",
    );
    let bl = blobs();
    for (i, (kind, name, bytes)) in bl.iter().enumerate() {
        src.push_str(&format!(
            " static const uint8_t b{i}[] = {};\n",
            c_bytes(bytes)
        ));
        let f = if *kind == "edid" {
            "drm_output_edid_monitor_id"
        } else {
            "drm_output_eld_monitor_id"
        };
        src.push_str(&format!(
            " (void){f}(b{i}, {}, &id); id_line(\"{kind}:{name}\", &id);\n",
            bytes.len()
        ));
    }
    let blob_idx =
        |kind: &str, name: &str| bl.iter().position(|b| b.0 == kind && b.1 == name).unwrap();
    for (tag, want, pcms) in pcm_scenarios() {
        src.push_str(" {\n struct drm_output_monitor_id want;\n");
        let w = blob_idx("edid", want);
        src.push_str(&format!(
            " (void)drm_output_edid_monitor_id(b{w}, {}, &want);\n",
            bl[w].2.len()
        ));
        src.push_str(&format!(
            " struct drm_output_audio_pcm p[{}];\n",
            pcms.len().max(1)
        ));
        for (j, &(card, device, eld)) in pcms.iter().enumerate() {
            let e = blob_idx("eld", eld);
            src.push_str(&format!(
                " p[{j}].card = {card}; p[{j}].device = {device}; (void)drm_output_eld_monitor_id(b{e}, {}, &p[{j}].id);\n",
                bl[e].2.len()
            ));
        }
        src.push_str(&format!(
            " size_t idx = 0; int r = drm_output_audio_pick_pcm(&want, p, {}, &idx);\n\
             printf(\"pcm:{tag} r=%d idx=%lld retry=%d reason=[%s]\\n\", r, r == DRM_OUTPUT_AUDIO_MATCH ? (long long)idx : -1LL, drm_output_audio_retryable(r) ? 1 : 0, drm_output_audio_reason(r));\n }}\n",
            pcms.len()
        ));
    }
    for (i, &(devices, device, switches)) in IEC958_VECTORS.iter().enumerate() {
        let list: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
        src.push_str(&format!(
            " {{ static const int d{i}[] = {{{}}}; printf(\"iec:{i} %d\\n\", drm_output_audio_iec958_index(d{i}, {}, {device}, {switches})); }}\n",
            if list.is_empty() { "0".to_string() } else { list.join(",") },
            devices.len()
        ));
    }
    for (i, &(ts, pd, now, lat, fl, thr, blk, band)) in plan_vectors().iter().enumerate() {
        src.push_str(&format!(
            " {{ struct drm_output_audio_plan p = drm_output_audio_plan_block({ts}ULL, {pd}ULL, {now}ULL, {lat}ULL, {fl}ULL, {thr}ULL, {blk}ULL, {band}ULL);\n\
             printf(\"plan:{i} silence=%\" PRIu64 \" drop=%\" PRIu64 \" offset=%\" PRId64 \" nudge=%d\\n\", p.silence_ns, p.drop_ns, p.offset_ns, (int)p.nudge_frames); }}\n"
        ));
    }
    for &(prev, s) in EWMA_VECTORS {
        src.push_str(&format!(
            " printf(\"ewma:{prev}:{s} %\" PRIu64 \"\\n\", drm_output_present_ewma({prev}ULL, {s}ULL));\n"
        ));
    }
    src.push_str(" return 0;\n}\n");
    src
}

fn compile_and_run(src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "drm_output_audio_1424-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).expect("tmp dir");
    let c = dir.join("harness.c");
    let bin = dir.join("harness");
    fs::write(&c, src).expect("write harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-I",
        ])
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1424: no C compiler ({cc}): {e} -- the parity gate cannot run")
        });
    if !out.status.success() {
        let _ = fs::remove_dir_all(&dir);
    }
    assert!(
        out.status.success(),
        "issue 1424: the pure header must compile cleanly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(run.status.success(), "issue 1424: harness failed");
    String::from_utf8(run.stdout).expect("utf8")
}

#[test]
fn c_header_and_rust_twin_agree_on_every_vector() {
    let c_out = compile_and_run(&c_harness());
    let c_lines: Vec<&str> = c_out.lines().collect();
    let r_lines = rust_lines();
    assert_eq!(
        c_lines.len(),
        r_lines.len(),
        "issue 1424: line count differs"
    );
    for (c, r) in c_lines.iter().zip(r_lines.iter()) {
        assert_eq!(*c, r.as_str(), "issue 1424: C and Rust disagree");
    }
}

// ------------------------------------------------------------------------------------------------
// The strih-pp decisions themselves (13.10.2026 live fixtures)
// ------------------------------------------------------------------------------------------------

#[test]
fn strih_pp_benq_on_hdmi0_resolves_to_hw_0_3() {
    let want = m::edid_monitor_id(&fixture("edid", "benq-hdmi0"));
    assert!(want.valid);
    assert_eq!(want.vendor, [0x09, 0xd1, 0x24, 0x80]);
    assert_eq!(want.name, "BenQ LCD");
    let eld = m::eld_monitor_id(&fixture("eld", "benq-card0-pcm3"));
    assert_eq!(
        eld.vendor, want.vendor,
        "issue 1424: EDID bytes 8..11 == ELD bytes 16..19, no byte swap"
    );
    let pcms = vec![
        m::AudioPcm {
            card: 0,
            device: 3,
            id: eld,
        },
        m::AudioPcm {
            card: 0,
            device: 7,
            id: m::eld_monitor_id(&[]),
        },
    ];
    assert_eq!(m::pick_pcm(&want, &pcms), (m::MATCH, Some(0)));
    // hw:0,3 = the PCM PipeWire called hdmi:0 on strih-pp (alsa.card 0, alsa.device 3); its IEC958
    // switch is index 0 of the card's HDMI PCMs 3/7/8/9
    assert_eq!((pcms[0].card, pcms[0].device), (0, 3));
    assert_eq!(m::iec958_index(&[3, 7, 8, 9], 3, 4), Some(0));
}

#[test]
fn strih_pp_blackmagic_on_hdmi1_waits_for_its_eld() {
    let want = m::edid_monitor_id(&fixture("edid", "blackmagic-hdmi1"));
    assert_eq!(want.vendor, [0x09, 0xa4, 0x0c, 0xbe]);
    assert_eq!(want.name, "Blackmagic");
    let benq = m::eld_monitor_id(&fixture("eld", "benq-card0-pcm3"));
    let before = vec![
        m::AudioPcm {
            card: 0,
            device: 3,
            id: benq.clone(),
        },
        m::AudioPcm {
            card: 0,
            device: 7,
            id: m::eld_monitor_id(&[]),
        },
    ];
    let (r, idx) = m::pick_pcm(&want, &before);
    assert_eq!(
        (r, idx),
        (m::NO_MATCH, None),
        "issue 1424: the BenQ is never a fallback"
    );
    assert!(
        m::retryable(r),
        "issue 1424: an ELD not yet published is re-read in the window"
    );
    let after = vec![
        m::AudioPcm {
            card: 0,
            device: 3,
            id: benq,
        },
        m::AudioPcm {
            card: 0,
            device: 7,
            id: m::eld_monitor_id(&fixture("eld", "blackmagic-card0-pcm7")),
        },
    ];
    assert_eq!(m::pick_pcm(&want, &after), (m::MATCH, Some(1)));
    assert_eq!(m::iec958_index(&[3, 7, 8, 9], 7, 4), Some(1));
}

#[test]
fn late_audio_plays_at_the_floor_and_reports_its_offset() {
    const MS: u64 = 1_000_000;
    let now = 1_000_000 * MS;
    // the mix arrives 85 ms after its timestamp, the picture showed 16 ms after it
    let p = m::plan_block(
        now - 85 * MS,
        16 * MS,
        now,
        60 * MS,
        60 * MS,
        42 * MS,
        21 * MS,
        5 * MS,
    );
    assert_eq!((p.silence_ns, p.drop_ns), (0, 0));
    assert_eq!(
        p.offset_ns,
        (85 + 60 - 16) as i64 * MS as i64,
        "issue 1424: audio behind the picture"
    );
    // early audio is held for the picture
    let p = m::plan_block(
        now,
        150 * MS,
        now,
        60 * MS,
        60 * MS,
        42 * MS,
        21 * MS,
        5 * MS,
    );
    assert_eq!(p.silence_ns, 90 * MS);
    assert_eq!(p.offset_ns, 0);
}

#[test]
fn clock_drift_is_walked_back_one_frame_at_a_time_never_a_skip() {
    // review Y1: the HDMI audio clock and the OBS clock drift apart by tens of ppm; inside the
    // threshold the queue is walked back to the floor one frame per block (inaudible), never by
    // inserting silence or cutting a block
    const MS: u64 = 1_000_000;
    let now = 1_000_000 * MS;
    let place = |pcm: u64| {
        m::plan_block(
            now - 85 * MS,
            16 * MS,
            now,
            pcm,
            60 * MS,
            42 * MS,
            21 * MS,
            5 * MS,
        )
    };
    let grown = place(66 * MS);
    assert_eq!(
        (grown.silence_ns, grown.drop_ns, grown.nudge_frames),
        (0, 0, -1)
    );
    let shrunk = place(54 * MS);
    assert_eq!(
        (shrunk.silence_ns, shrunk.drop_ns, shrunk.nudge_frames),
        (0, 0, 1)
    );
    assert_eq!(
        place(64 * MS).nudge_frames,
        0,
        "inside the band nothing moves"
    );
    let far = place(150 * MS);
    assert_eq!(
        (far.nudge_frames, far.drop_ns),
        (0, 21 * MS),
        "a large offset still trims a block"
    );
}
