//! Issue 1439 — the obs-vban receive timeline's WIRING, split out of
//! `tests/vban_receive_timeline_parity_1439.rs` (review round 1: the parity file had grown past ~1100
//! lines). The C-vs-Rust decision parity stays there; this file pins what surrounds it:
//!
//! 1. the source stays a directly stamped source on the genlock ASRC's STAMP basis (the libobs
//!    decision lifted from `obs-source.c` and run) and never moves itself off the ASRC path;
//! 2. the receive callback stamps from the timeline, outputs a lost slot as silence in the
//!    packet's own sample format, and logs the 10 s status line;
//! 3. the Linux strih bundle builds and stages the plugin, and both Windows builds assert it.
//!
//! Std-only, like the parity file:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_receive_wiring_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required (the ASRC lift is compiled); a missing compiler FAILS, never skips.

#[path = "support/vban_c_1439.rs"]
mod vban_c;

use std::fs;
use vban_c::{compile_and_run, repo};

const SOURCE: &str = "vendor/obs-vban/src/vban-source.c";
const HEADER: &str = "vendor/obs-vban/src/vban-receive-timeline.h";
const MS: u64 = 1_000_000;

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("issue 1439: read {rel}: {e}"))
}

/// A `#define NAME <integer>` of the shipped receive-timeline header.
fn header_define(name: &str) -> u64 {
    read(HEADER)
        .lines()
        .find_map(|l| l.strip_prefix(&format!("#define {name} ")))
        .unwrap_or_else(|| panic!("issue 1439: the header lost #define {name}"))
        .trim()
        .trim_end_matches('u')
        .parse()
        .unwrap_or_else(|e| panic!("issue 1439: #define {name} is not an integer: {e}"))
}

// ---------------------------------------------------------------------------------------------
// The source stays on the genlock ASRC's STAMP basis (design addendum 6338457001)

/// Lifts a `static inline` helper from `obs-source.c` by its signature, up to the first `\n}\n`.
fn lift(src: &str, signature: &str) -> String {
    let a = src
        .find(signature)
        .unwrap_or_else(|| panic!("issue 1439: obs-source.c lost `{signature}`"));
    assert_eq!(
        src.matches(signature).count(),
        1,
        "issue 1439: `{signature}` is not unique in obs-source.c"
    );
    let b = src[a..].find("\n}\n").expect("the helper's end") + a + 3;
    src[a..b].to_string()
}

/// The libobs decision itself, run for the vban source's state: it must read STAMP, and the
/// stamps the timeline hands OBS (R ahead of the fitted arrival, R up to the setting's maximum)
/// must read as direct stamps against MAX_TS_VAR.
#[test]
fn the_vban_source_is_a_directly_stamped_source_on_the_asrc_stamp_basis_1439() {
    let obs_source = read("vendor/obs-studio/libobs/obs-source.c");
    let internal = read("vendor/obs-studio/libobs/obs-internal.h");
    let max_ts_var: u64 = internal
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define MAX_TS_VAR "))
        .expect("issue 1439: obs-internal.h lost MAX_TS_VAR")
        .trim()
        .trim_end_matches("ULL")
        .parse()
        .expect("MAX_TS_VAR is a plain number");
    let mut defines = String::new();
    for name in [
        "GENLOCK_AUDIO_HOLD_OFF",
        "GENLOCK_AUDIO_HOLD_LATENCY",
        "GENLOCK_AUDIO_HOLD_TIMECODE",
        "GENLOCK_AUDIO_HOLD_PENDING",
        "GENLOCK_AUDIO_ASRC_BASIS_ARRIVAL",
        "GENLOCK_AUDIO_ASRC_BASIS_TIMECODE",
        "GENLOCK_AUDIO_ASRC_BASIS_STAMP",
    ] {
        let l = obs_source
            .lines()
            .find(|l| l.starts_with(&format!("#define {name} ")))
            .unwrap_or_else(|| panic!("issue 1439: obs-source.c lost #define {name}"));
        defines.push_str(l);
        defines.push('\n');
    }
    let hold = lift(&obs_source, "static inline int genlock_audio_hold_mode(");
    let placement = lift(
        &obs_source,
        "static inline int genlock_audio_asrc_placement(",
    );
    let reserve_max_ns = header_define("VBAN_RECV_RESERVE_MS_MAX") * MS;
    let c = format!(
        "#include <stdbool.h>\n#include <stdint.h>\n#include <stdio.h>\n{defines}\n{hold}\n{placement}\n\
         static uint64_t diff(uint64_t a, uint64_t b) {{ return a > b ? a - b : b - a; }}\n\
         int main(void)\n{{\n\
         \tconst uint64_t now = 5000000000ULL;\n\
         \tconst uint64_t aheads[3] = {{10000000ULL, 40000000ULL, {reserve_max_ns}ULL}};\n\
         \tfor (int i = 0; i < 3; i++) {{\n\
         \t\tconst bool direct = diff(now + aheads[i], now) < {max_ts_var}ULL;\n\
         \t\t/* the vban source: no genlock FIFO, no latency, not monitor-only, ASRC on + a resampler */\n\
         \t\tconst int mode = genlock_audio_hold_mode(false, 0u, true, 0u, false);\n\
         \t\tprintf(\"basis %d %d\\n\", i, genlock_audio_asrc_placement(mode, direct, false, true));\n\
         \t}}\n\
         \tprintf(\"monitor-only %d\\n\", genlock_audio_asrc_placement(genlock_audio_hold_mode(false, 0u, true, 0u, false), true, true, true));\n\
         \tprintf(\"stamp %d arrival %d\\n\", GENLOCK_AUDIO_ASRC_BASIS_STAMP, GENLOCK_AUDIO_ASRC_BASIS_ARRIVAL);\n\
         \treturn 0;\n}}\n"
    );
    let out = compile_and_run("asrc", &c, &["-Wno-unused-function"]);
    let stamp_arrival = out.last().expect("output").clone();
    let mut it = stamp_arrival.split_whitespace();
    let stamp: i32 = it.nth(1).unwrap().parse().unwrap();
    let arrival: i32 = it.nth(1).unwrap().parse().unwrap();
    for (i, l) in out.iter().take(3).enumerate() {
        assert_eq!(
            l,
            &format!("basis {i} {stamp}"),
            "issue 1439: a vban stamp R ahead must keep the source on the ASRC STAMP basis: {out:?}"
        );
    }
    assert_eq!(
        out[3],
        format!("monitor-only {arrival}"),
        "issue 1439: the control (a monitor-only source) must read the arrival basis: {out:?}"
    );

    // The source's own flags and stamping keep it there.
    let src = read(SOURCE);
    let squished = src.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        ".output_flags = OBS_SOURCE_AUDIO | OBS_SOURCE_DO_NOT_DUPLICATE,",
        "audio.timestamp = d.stamp_ns; obs_source_output_audio(s->context, &audio);",
        "silence.timestamp = vban_recv_stamp_at(&s->timeline, d->silence_pos + done);",
    ] {
        assert!(
            squished.contains(needle),
            "issue 1439: vban-source.c lost `{needle}`"
        );
    }
    let mut plugin = String::new();
    for f in fs::read_dir(repo("vendor/obs-vban/src")).unwrap() {
        plugin.push_str(&fs::read_to_string(f.unwrap().path()).unwrap());
    }
    for banned in [
        "obs_source_set_asrc_enabled",
        "obs_source_set_genlock_fifo",
        "obs_source_set_monitoring_type",
        "obs_source_set_audio_mixers",
    ] {
        assert!(
            !plugin.contains(banned),
            "issue 1439: obs-vban calls `{banned}`: it must never move its source off the ASRC path"
        );
    }
    assert!(
        obs_source.contains("source->asrc_enabled = true;"),
        "issue 1439: libobs no longer turns the ASRC on for every source by default"
    );
}

// ---------------------------------------------------------------------------------------------
// Wiring

#[test]
fn the_receive_timeline_is_wired_into_the_source_1439() {
    let src = read(SOURCE);
    let squished = src.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "#include \"vban-receive-timeline.h\"",
        "vban_recv_packet(&s->timeline, now, header->nuFrame, audio.frames,",
        "if (os_atomic_exchange_bool(&s->timeline_reset, false)) vban_recv_init(&s->timeline);",
        "(uint64_t)os_atomic_load_long(&s->receive_buffer_ms) * VBAN_RECV_NS_PER_MS;",
        "vban_recv_clamp_reserve_ms((int64_t)obs_data_get_int(settings, \"receive_buffer_ms\"))",
        "obs_data_set_default_int(data, \"receive_buffer_ms\", VBAN_RECV_RESERVE_MS_DEFAULT);",
        "obs_properties_add_int(props, \"receive_buffer_ms\",",
        // the silence keeps the packet's own sample format (review round 1: a format change resets
        // libobs's resampler and the genlock ASRC), U8 silence is 0x80
        "if (d.silence_samples > 0) vban_src_output_silence(s, &d, &audio);",
        ".format = like->format,",
        "memset(s->silence.array, like->format == AUDIO_FORMAT_U8BIT ? 0x80 : 0, plane_bytes);",
        "\"obs-vban receive: received=%\" PRIu64 \" lost=%\" PRIu64 \" late=%\" PRIu64 \" re_anchors=%\" PRIu64",
        "\"obs-vban receive re-anchor: source '%s' reason=%s",
        "\"obs-vban receive anchored: source '%s'",
    ] {
        assert!(
            squished.contains(needle),
            "issue 1439: vban-source.c lost `{needle}`"
        );
    }
    // The order inside the callback: the decision, then the silence, then the packet.
    let at = |n: &str| {
        squished
            .find(n)
            .unwrap_or_else(|| panic!("issue 1439: `{n}` missing"))
    };
    assert!(
        at("vban_recv_packet(&s->timeline") < at("vban_src_output_silence(s, &d,")
            && at("vban_src_output_silence(s, &d,") < at("audio.timestamp = d.stamp_ns;"),
        "issue 1439: the callback must decide, then output the silence, then the packet"
    );
    // The stock arrival stamping and the copy-padding are gone for good.
    for gone in [
        "Padding packet",
        "lost_ns",
        "os_gettime_ns() - (uint64_t)audio.frames",
        ".format = AUDIO_FORMAT_FLOAT_PLANAR,",
    ] {
        assert!(
            !src.contains(gone),
            "issue 1439: `{gone}` is back in vban-source.c"
        );
    }
    let mut status_lines = 0;
    for f in fs::read_dir(repo("vendor/obs-vban/src")).unwrap() {
        status_lines += fs::read_to_string(f.unwrap().path())
            .unwrap()
            .matches("obs-vban receive: ")
            .count();
    }
    assert_eq!(
        status_lines, 1,
        "issue 1439: `obs-vban receive: ` must appear on exactly one log line (the 10 s status)"
    );
    let locale = read("vendor/obs-vban/data/locale/en-US.ini");
    assert!(
        locale.contains("VBAN.src.prop.receive_buffer_ms="),
        "issue 1439: the Receive Buffer property has no label"
    );
}

/// The Linux strih bundle builds the vendored obs-vban against the genlocked SDK and stages it next
/// to DistroAV; a vendor/obs-vban change re-runs the Linux build.
#[test]
fn the_linux_strih_bundle_builds_and_stages_obs_vban_1439() {
    let wf = read(".github/workflows/linux-genlock.yml");
    assert!(
        wf.contains("      - 'vendor/obs-vban/**'"),
        "issue 1439: linux-genlock.yml must trigger on vendor/obs-vban/**"
    );
    let job = wf
        .find("  linux-genlock-build-strih:")
        .expect("the strih job");
    let strih = &wf[job..];
    let at = |n: &str| {
        strih
            .find(n)
            .unwrap_or_else(|| panic!("issue 1439: the strih job lost `{n}`"))
    };
    let build_dav = at("      - name: Build DistroAV");
    let configure =
        at("      - name: Configure obs-vban against the genlocked OBS SDK (issue 1439)");
    let build = at("      - name: Build obs-vban (issue 1439)");
    let stage = at("      - name: Stage artifact");
    assert!(
        build_dav < configure && configure < build && build < stage,
        "issue 1439: the strih job must configure + build obs-vban after DistroAV, before staging"
    );
    for needle in [
        "working-directory: vendor/obs-vban",
        "-DCMAKE_PREFIX_PATH=\"$GITHUB_WORKSPACE/obs-sdk\"",
        "find vendor/obs-vban/build_ubuntu -name 'obs-vban.so' -not -path '*/CMakeFiles/*'",
        "cp \"$vban_so\" \"$plugin_lib_dir/\"",
        "cp -a vendor/obs-vban/data/. \"$data_root/obs-vban/\"",
    ] {
        assert!(
            strih.contains(needle),
            "issue 1439: the strih job lost `{needle}`"
        );
    }
    // The imag-parity Linux bundle stays without it (imag consumes no VBAN).
    let imag = &wf[wf.find("  linux-genlock-build:").unwrap()..job];
    assert!(
        !imag.contains("obs-vban"),
        "issue 1439: the imag-parity Linux bundle must not build obs-vban"
    );
}

/// Both Windows genlock builds compile the same source: their pwsh gate checks the receive side too.
#[test]
fn both_windows_builds_assert_the_receive_timeline_1439() {
    for name in ["windows-genlock.yml", "windows-genlock-fast.yml"] {
        let yml = read(&format!(".github/workflows/{name}"));
        for needle in [
            "Test-Path \"vendor/obs-vban/src/vban-receive-timeline.h\"",
            "[regex]::Escape('#include \"vban-receive-timeline.h\"')",
            "[regex]::Escape('vban_recv_packet(&s->timeline, now, header->nuFrame,')",
            "[regex]::Escape('\"obs-vban receive: received=%\"')",
            "$source -match [regex]::Escape('Padding packet')",
        ] {
            assert!(
                yml.contains(needle),
                "issue 1439: {name}'s obs-vban assert lost `{needle}`"
            );
        }
    }
}
