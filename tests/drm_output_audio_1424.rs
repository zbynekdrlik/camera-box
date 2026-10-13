//! issue 1424 (owner 13.10.2026, design 6147051141) -- the fixed HDMI output plays the OBS program
//! mix on the SAME connector's HDMI audio by itself: no per-box setting, never OBS's global
//! monitoring device; no match / two matches / no sink = no audio + ONE named log line.
//!
//! Facet A pins the wiring with std-only source anchors: the new libobs TU
//! `obs-drm-output-audio.c` (Linux-only, built by `libobs/cmake/os-linux.cmake`, its PulseAudio half
//! only where libobs builds its PulseAudio monitoring), the raw callback on mix 0, the per-PCM ALSA
//! `ELD` control + the RandR EDID property as the two match sources, the bounded re-read window
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

const AUDIO_C: &str = "vendor/obs-studio/libobs/obs-drm-output-audio.c";
const AUDIO_H: &str = "vendor/obs-studio/libobs/obs-drm-output-audio.h";
const MATCH_H: &str = "vendor/obs-studio/libobs/obs-drm-output-audio-match.h";
const DRM_C: &str = "vendor/obs-studio/libobs/obs-drm-output.c";
const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
const LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";
const FIXTURES: &str = "tests/fixtures/drm_output_audio_1424.tsv";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
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
    assert!(c.contains("#if defined(__linux__)"), "issue 1424: the audio TU must be Linux-only");
    let cm = file(LINUX_CMAKE);
    for f in ["obs-drm-output-audio.c", "obs-drm-output-audio.h", "obs-drm-output-audio-match.h"] {
        assert!(cm.contains(f), "issue 1424: os-linux.cmake must build {f}");
    }
    // the PulseAudio half compiles only where libobs builds its own PulseAudio monitoring
    let pulse = cm
        .split("if(ENABLE_PULSEAUDIO)")
        .nth(1)
        .and_then(|s| s.split("else()").next())
        .expect("issue 1424: os-linux.cmake has an if(ENABLE_PULSEAUDIO) block");
    assert!(
        pulse.contains("OBS_DRM_OUTPUT_AUDIO_PULSE"),
        "issue 1424: the ENABLE_PULSEAUDIO block must define OBS_DRM_OUTPUT_AUDIO_PULSE"
    );
    assert!(c.contains("#if defined(OBS_DRM_OUTPUT_AUDIO_PULSE)"));
    assert!(
        c.contains("built without PulseAudio"),
        "issue 1424: a libobs without PulseAudio logs that the leg is absent, never silently"
    );
}

#[test]
fn audio_plays_mix_0_through_the_pulse_wrapper_never_the_monitoring_device() {
    let c = file(AUDIO_C);
    let s = squish(&c);
    assert!(s.contains("obs_add_raw_audio_callback(0,"), "issue 1424: the program mix = mix 0");
    assert!(s.contains("obs_remove_raw_audio_callback(0,"));
    assert!(c.contains("pulseaudio_connect_playback("), "issue 1424: libobs's PulseAudio wrapper");
    assert!(c.contains("pulseaudio_get_sink_info_list("));
    for banned in ["monitoring_device_id", "get_default_id", "obs_set_audio_monitoring_device", "\"default\""] {
        assert!(
            !c.contains(banned),
            "issue 1424: the leg must never fall back to OBS's monitoring device or the default sink ({banned})"
        );
    }
    // the sink is chosen by its alsa.card / alsa.device properties, never by a stored name
    assert!(c.contains("\"alsa.card\"") && c.contains("\"alsa.device\""));
    assert!(c.contains("drm_output_audio_pick_sink("));
    // no per-box audio key in the output's config
    assert!(!c.contains("drm-output.json"), "issue 1424: no per-box audio setting");
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
    assert!(!c.contains("/proc/asound"), "issue 1424: the /proc eld index is not a PCM number");
    assert!(c.contains("drm_output_audio_pick_pcm("));
    // the bounded re-read window, then ONE named line
    assert!(c.contains("DRM_OUTPUT_AUDIO_RESOLVE_WINDOW_MS"));
    assert!(c.contains("drm_output_audio_retryable("));
    assert!(c.contains("drm-output audio: NO AUDIO on '%s'"));
    assert!(c.contains("drm-output audio: sink %s card %d device %d delay %.1f ms"));
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
    assert!(!c.contains("static struct drm_output_audio g_"), "issue 1424: no global leg");
}

#[test]
fn audio_is_gated_to_the_program_view() {
    // the Multiview view is the cutter's own monitor: the program audio stays off it
    let c = file(AUDIO_C);
    assert!(c.contains("obs_drm_output_get_view() == OBS_DRM_OUTPUT_VIEW_PROGRAM"));
}

#[test]
fn output_start_and_stop_drive_the_audio_leg() {
    let d = squish(&file(DRM_C));
    // the EDID is read BEFORE a backend takes the connector off X, the leg opens only AFTER the
    // backend start succeeded (the mode is driven, so the connector's ELD can appear)
    let start = d.split("bool obs_drm_output_start(").nth(1).expect("start");
    let start = start.split("void obs_drm_output_stop(").next().unwrap();
    let edid = start.find("drm_output_audio_read_edid(").expect("issue 1424: start reads the EDID");
    let vk = start.find("drm_output_vk_direct_backend.start(cfg)").expect("vk start");
    assert!(edid < vk, "issue 1424: the EDID is read before the vk-direct backend acquires the display");
    let lease = start.find("drm_output_acquire_lease(").expect("lease");
    assert!(edid < lease, "issue 1424: the EDID is read before the lease");
    assert!(
        start.matches("drm_output_audio_open(").count() >= 2,
        "issue 1424: both backends open the audio leg after their start succeeded"
    );
    let stop = d.split("void obs_drm_output_stop(void)").nth(1).expect("stop");
    let stop = stop.split("bool obs_drm_output_active(").next().unwrap();
    assert!(stop.contains("drm_output_audio_close("), "issue 1424: stop closes the leg");
    let close = stop.find("drm_output_audio_close(").unwrap();
    let vkstop = stop.find("drm_output_vk_direct_backend.stop()").unwrap();
    assert!(close < vkstop, "issue 1424: the audio closes before the picture goes");
}

#[test]
fn both_backends_measure_the_present_delay() {
    let d = squish(&file(DRM_C));
    let publish = d.split("void drm_output_publish_render_buf(int idx)").nth(1).expect("publish");
    let publish = publish.split("gs_texture_t *drm_output_render_buf_texture(").next().unwrap();
    assert!(
        publish.contains("drm_output_present_note_published(idx)"),
        "issue 1424: the ONE publish entry both backends use records the frame's render tick"
    );
    let flip = d.split("static void *drm_output_flip_thread(").nth(1).expect("flip");
    assert!(flip.contains("drm_output_present_note_shown(pnext"), "issue 1424: the lease flip event");
    let vk = squish(&file(VK_C));
    let present = vk.split("static void *drm_output_vk_present_thread(").nth(1).expect("present");
    assert!(present.contains("drm_output_present_note_shown(src"), "issue 1424: the vk-direct present");
}

// ------------------------------------------------------------------------------------------------
// Facet B -- the C header vs the Rust twin
// ------------------------------------------------------------------------------------------------

fn c_bytes(v: &[u8]) -> String {
    if v.is_empty() {
        return "{0}".to_string();
    }
    format!("{{{}}}", v.iter().map(|b| format!("0x{b:02x}")).collect::<Vec<_>>().join(","))
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
        ("eld", "empty", Vec::new()),
    ]
}

/// (card, device, eld blob name) sets for pick_pcm, and the EDID that is wanted.
type PcmScenario = (&'static str, &'static str, &'static [(i32, i32, &'static str)]);

fn pcm_scenarios() -> Vec<PcmScenario> {
    vec![
        // strih-pp today: the BenQ on HDMI-0 is on PCM 3, HDMI-1's ELD is not published yet
        ("benq-today", "benq", &[(0, 3, "benq"), (0, 7, "empty"), (0, 8, "empty")]),
        ("bmd-before-mode", "bmd", &[(0, 3, "benq"), (0, 7, "empty"), (0, 8, "empty")]),
        // once a mode is driven on HDMI-1 its ELD appears
        ("bmd-after-mode", "bmd", &[(0, 3, "benq"), (0, 7, "bmd"), (0, 8, "empty")]),
        ("benq-both", "benq", &[(0, 3, "benq"), (0, 7, "bmd")]),
        ("no-monitor", "benq", &[(0, 3, "empty"), (1, 3, "short")]),
        ("no-edid", "badsum", &[(0, 3, "benq")]),
        // two identical monitors: no guess
        ("twins", "benq", &[(0, 3, "benq"), (0, 7, "benq")]),
        ("none", "benq", &[]),
    ]
}

type SinkScenario = (&'static str, i32, i32, &'static [(i32, i32)]);

fn sink_scenarios() -> Vec<SinkScenario> {
    vec![
        ("strih-pp", 0, 3, &[(1, 0), (0, 3), (2, 0)]),
        ("hdmi1-no-sink", 0, 7, &[(1, 0), (0, 3)]),
        ("dup", 0, 3, &[(0, 3), (0, 3)]),
        ("empty", 0, 3, &[]),
        ("other-card", 1, 3, &[(0, 3)]),
    ]
}

const INDEX_STRINGS: &[&str] = &["0", "3", "07", "9999", "10000", "", "-1", "3 ", "a", "12a"];

type PlanVector = (u64, u64, u64, u64, u64, u64, u64);

fn plan_vectors() -> Vec<PlanVector> {
    const MS: u64 = 1_000_000;
    let now = 86_400_000 * MS;
    let blk = 21_333_333;
    let mut v = Vec::new();
    // (block_ts, present_delay, now, stream_latency, floor, threshold, block)
    for age in [0, 10 * MS, 85 * MS, 120 * MS] {
        for pd in [0, 16 * MS, 33 * MS, 140 * MS] {
            for lat in [0, 40 * MS, 60 * MS, 150 * MS] {
                v.push((now - age, pd, now, lat, 60 * MS, 42 * MS, blk));
            }
        }
    }
    // the early-audio hold, both sides of the threshold (strict)
    v.push((now, 102 * MS, now, 60 * MS, 60 * MS, 42 * MS, blk));
    v.push((now, 102 * MS + 1, now, 60 * MS, 60 * MS, 42 * MS, blk));
    // a grown buffer, trimmed at most one block
    v.push((now - 85 * MS, 16 * MS, now, 300 * MS, 60 * MS, 42 * MS, blk));
    v.push((now - 85 * MS, 16 * MS, now, 102 * MS, 60 * MS, 42 * MS, blk));
    v.push((now - 85 * MS, 16 * MS, now, 102 * MS + 1, 60 * MS, 42 * MS, blk));
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
    let get = |kind: &str, name: &str| bl.iter().find(|b| b.0 == kind && b.1 == name).unwrap().2.clone();
    for (kind, name, bytes) in &bl {
        let id = if *kind == "edid" { m::edid_monitor_id(bytes) } else { m::eld_monitor_id(bytes) };
        out.push(id_line(&format!("{kind}:{name}"), &id));
    }
    for (tag, want, pcms) in pcm_scenarios() {
        let want_id = m::edid_monitor_id(&get("edid", want));
        let v: Vec<m::AudioPcm> = pcms
            .iter()
            .map(|&(card, device, eld)| m::AudioPcm { card, device, id: m::eld_monitor_id(&get("eld", eld)) })
            .collect();
        let (r, idx) = m::pick_pcm(&want_id, &v);
        out.push(format!(
            "pcm:{tag} r={r} idx={} retry={} reason=[{}]",
            idx.map_or(-1, |i| i as i64),
            u8::from(m::retryable(r)),
            m::reason(r)
        ));
    }
    for (tag, card, device, sinks) in sink_scenarios() {
        let (r, idx) = m::pick_sink(card, device, sinks);
        out.push(format!("sink:{tag} r={r} idx={} reason=[{}]", idx.map_or(-1, |i| i as i64), m::reason(r)));
    }
    for s in INDEX_STRINGS {
        out.push(format!("index:[{s}] {}", m::parse_index(s).map_or("none".to_string(), |v| v.to_string())));
    }
    for (i, &(ts, pd, now, lat, fl, thr, blk)) in plan_vectors().iter().enumerate() {
        let p = m::plan_block(ts, pd, now, lat, fl, thr, blk);
        out.push(format!("plan:{i} silence={} drop={} offset={}", p.silence_ns, p.drop_ns, p.offset_ns));
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
        src.push_str(&format!(" static const uint8_t b{i}[] = {};\n", c_bytes(bytes)));
        let f = if *kind == "edid" { "drm_output_edid_monitor_id" } else { "drm_output_eld_monitor_id" };
        src.push_str(&format!(" (void){f}(b{i}, {}, &id); id_line(\"{kind}:{name}\", &id);\n", bytes.len()));
    }
    let blob_idx = |kind: &str, name: &str| bl.iter().position(|b| b.0 == kind && b.1 == name).unwrap();
    for (tag, want, pcms) in pcm_scenarios() {
        src.push_str(" {\n struct drm_output_monitor_id want;\n");
        let w = blob_idx("edid", want);
        src.push_str(&format!(" (void)drm_output_edid_monitor_id(b{w}, {}, &want);\n", bl[w].2.len()));
        src.push_str(&format!(" struct drm_output_audio_pcm p[{}];\n", pcms.len().max(1)));
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
    for (tag, card, device, sinks) in sink_scenarios() {
        src.push_str(&format!(" {{\n struct drm_output_audio_sink_ref s[{}];\n", sinks.len().max(1)));
        for (j, &(c, d)) in sinks.iter().enumerate() {
            src.push_str(&format!(" s[{j}].card = {c}; s[{j}].device = {d};\n"));
        }
        src.push_str(&format!(
            " size_t idx = 0; int r = drm_output_audio_pick_sink({card}, {device}, s, {}, &idx);\n\
             printf(\"sink:{tag} r=%d idx=%lld reason=[%s]\\n\", r, r == DRM_OUTPUT_AUDIO_MATCH ? (long long)idx : -1LL, drm_output_audio_reason(r));\n }}\n",
            sinks.len()
        ));
    }
    for s in INDEX_STRINGS {
        src.push_str(&format!(
            " {{ int v = 0; if (drm_output_audio_parse_index(\"{s}\", &v)) printf(\"index:[{s}] %d\\n\", v); else printf(\"index:[{s}] none\\n\"); }}\n"
        ));
    }
    for (i, &(ts, pd, now, lat, fl, thr, blk)) in plan_vectors().iter().enumerate() {
        src.push_str(&format!(
            " {{ struct drm_output_audio_plan p = drm_output_audio_plan_block({ts}ULL, {pd}ULL, {now}ULL, {lat}ULL, {fl}ULL, {thr}ULL, {blk}ULL);\n\
             printf(\"plan:{i} silence=%\" PRIu64 \" drop=%\" PRIu64 \" offset=%\" PRId64 \"\\n\", p.silence_ns, p.drop_ns, p.offset_ns); }}\n"
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
    let dir = std::env::temp_dir().join(format!("drm_output_audio_1424_{}", std::process::id()));
    fs::create_dir_all(&dir).expect("tmp dir");
    let c = dir.join("harness.c");
    let bin = dir.join("harness");
    fs::write(&c, src).expect("write harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu11", "-Wall", "-Wextra", "-Wconversion", "-Wformat=2", "-Werror", "-I"])
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("issue 1424: no C compiler ({cc}): {e} -- the parity gate cannot run"));
    assert!(
        out.status.success(),
        "issue 1424: the pure header must compile cleanly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    assert!(run.status.success(), "issue 1424: harness failed");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout).expect("utf8")
}

#[test]
fn c_header_and_rust_twin_agree_on_every_vector() {
    let c_out = compile_and_run(&c_harness());
    let c_lines: Vec<&str> = c_out.lines().collect();
    let r_lines = rust_lines();
    assert_eq!(c_lines.len(), r_lines.len(), "issue 1424: line count differs");
    for (c, r) in c_lines.iter().zip(r_lines.iter()) {
        assert_eq!(*c, r.as_str(), "issue 1424: C and Rust disagree");
    }
}

// ------------------------------------------------------------------------------------------------
// The strih-pp decisions themselves (13.10.2026 live fixtures)
// ------------------------------------------------------------------------------------------------

#[test]
fn strih_pp_benq_on_hdmi0_resolves_to_pcm_3_and_its_sink() {
    let want = m::edid_monitor_id(&fixture("edid", "benq-hdmi0"));
    assert!(want.valid);
    assert_eq!(want.vendor, [0x09, 0xd1, 0x24, 0x80]);
    assert_eq!(want.name, "BenQ LCD");
    let eld = m::eld_monitor_id(&fixture("eld", "benq-card0-pcm3"));
    assert_eq!(eld.vendor, want.vendor, "issue 1424: EDID bytes 8..11 == ELD bytes 16..19, no byte swap");
    let pcms = vec![
        m::AudioPcm { card: 0, device: 3, id: eld },
        m::AudioPcm { card: 0, device: 7, id: m::eld_monitor_id(&[]) },
    ];
    assert_eq!(m::pick_pcm(&want, &pcms), (m::MATCH, Some(0)));
    // alsa_output.pci-0000_0a_00.1.hdmi-stereo: alsa.card 0, alsa.device 3
    assert_eq!(m::pick_sink(0, 3, &[(1, 0), (0, 3)]), (m::MATCH, Some(1)));
}

#[test]
fn strih_pp_blackmagic_on_hdmi1_waits_for_its_eld_then_names_a_missing_sink() {
    let want = m::edid_monitor_id(&fixture("edid", "blackmagic-hdmi1"));
    assert_eq!(want.vendor, [0x09, 0xa4, 0x0c, 0xbe]);
    assert_eq!(want.name, "Blackmagic");
    let benq = m::eld_monitor_id(&fixture("eld", "benq-card0-pcm3"));
    let before = vec![
        m::AudioPcm { card: 0, device: 3, id: benq.clone() },
        m::AudioPcm { card: 0, device: 7, id: m::eld_monitor_id(&[]) },
    ];
    let (r, idx) = m::pick_pcm(&want, &before);
    assert_eq!((r, idx), (m::NO_MATCH, None), "issue 1424: the BenQ is never a fallback");
    assert!(m::retryable(r), "issue 1424: an ELD not yet published is re-read in the window");
    let after = vec![
        m::AudioPcm { card: 0, device: 3, id: benq },
        m::AudioPcm { card: 0, device: 7, id: m::eld_monitor_id(&fixture("eld", "blackmagic-card0-pcm7")) },
    ];
    assert_eq!(m::pick_pcm(&want, &after), (m::MATCH, Some(1)));
    // a card profile that exposes only the HDMI-0 PCM: no sink for PCM 7 -> no audio, named
    let (r, _) = m::pick_sink(0, 7, &[(1, 0), (0, 3)]);
    assert_eq!(r, m::NO_SINK);
    assert_eq!(m::reason(r), "no PipeWire sink plays the matched PCM");
}

#[test]
fn late_audio_plays_at_the_floor_and_reports_its_offset() {
    const MS: u64 = 1_000_000;
    let now = 1_000_000 * MS;
    // the mix arrives 85 ms after its timestamp, the picture showed 16 ms after it
    let p = m::plan_block(now - 85 * MS, 16 * MS, now, 60 * MS, 60 * MS, 42 * MS, 21 * MS);
    assert_eq!((p.silence_ns, p.drop_ns), (0, 0));
    assert_eq!(p.offset_ns, (85 + 60 - 16) as i64 * MS as i64, "issue 1424: audio behind the picture");
    // early audio is held for the picture
    let p = m::plan_block(now, 150 * MS, now, 60 * MS, 60 * MS, 42 * MS, 21 * MS);
    assert_eq!(p.silence_ns, 90 * MS);
    assert_eq!(p.offset_ns, 0);
}
