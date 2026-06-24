//! #193 — the probe tooling (recording-verdict) MUST stay Windows-buildable so the
//! recording decode runs ON stream.lan (where the multi-GB video lives), never downloaded
//! to slow dev1 (the root of the download + OOM #187 + disk-drain).
//!
//! recording-verdict is a pure Rust binary (ffmpeg spawn + rqrr + image — all cross-platform).
//! The ONLY thing that stops it cross-building for Windows is the camera-box lib pulling in
//! the Linux-only crates (v4l, alsa, cpal, evdev, drm, libc-ioctl) UNCONDITIONALLY. The fix
//! confines those to `[target.'cfg(target_os = "linux")'.dependencies]` and gates the lib
//! modules that use them behind `#[cfg(target_os = "linux")]`, so a non-Linux target never
//! resolves them.
//!
//! These are STRUCTURAL guards: they fail loudly if a future edit moves a Linux-only crate
//! back into the unconditional `[dependencies]` table, or drops the `cfg(target_os="linux")`
//! gate on an appliance / hardware-glue module — either of which silently re-breaks the
//! Windows verdict build (and #193's "decode where the video is"). They run on every push, on
//! any host, with no Windows toolchain needed. The DEFINITIVE end-to-end proof is the
//! `windows-probe` CI job that actually cross-builds `recording-verdict.exe`.

use std::fs;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The Linux-only crates that the camera APPLIANCE + probe HARDWARE GLUE bind, and which
/// must NOT be reachable on a non-Linux target. v4l (V4L2 capture), alsa + cpal (intercom
/// audio), evdev (power-button input), drm (KMS page-flip presenter). `libc` is technically
/// cross-platform as a crate but is only USED here for the Linux /dev/fb0 ioctl glue, so it
/// is gated with the rest; we still assert it is NOT in the plain table.
const LINUX_ONLY_CRATES: &[&str] = &["v4l", "alsa", "cpal", "evdev", "drm", "libc"];

/// Split Cargo.toml at the Linux-target dependency table header. Everything BEFORE the header
/// (after the `[dependencies]` line) is the unconditional `[dependencies]` region; everything
/// AT/AFTER is the Linux-target region. A Linux-only crate must appear ONLY in the latter.
fn cargo_sections() -> (String, String) {
    let toml = read("Cargo.toml");
    let header = "[target.'cfg(target_os = \"linux\")'.dependencies]";
    let idx = toml.find(header).unwrap_or_else(|| {
        panic!(
            "#193: Cargo.toml must declare a [target.'cfg(target_os = \"linux\")'.dependencies] \
             table so the Linux-only crates are confined to Linux (else recording-verdict can't \
             cross-build for Windows and the decode is forced back onto dev1)."
        )
    });
    // Unconditional region: from the `[dependencies]` line up to (not including) the linux header.
    let dep_idx = toml
        .find("\n[dependencies]")
        .expect("Cargo.toml must have a [dependencies] table");
    let unconditional = toml[dep_idx..idx].to_string();
    // Linux region: from the header to the next top-level table (`\n[` at column 0) or EOF.
    let after = &toml[idx + header.len()..];
    let end = after
        .find("\n[")
        .map(|e| idx + header.len() + e)
        .unwrap_or(toml.len());
    let linux_region = toml[idx..end].to_string();
    (unconditional, linux_region)
}

/// A Cargo dependency declaration line for `crate` (`crate = ...` or `crate.something`),
/// ignoring comment lines, so a comment merely mentioning the crate name is not a false hit.
fn declares_dep(section: &str, krate: &str) -> bool {
    section.lines().any(|l| {
        let t = l.trim_start();
        if t.starts_with('#') {
            return false;
        }
        t.starts_with(&format!("{krate} =")) || t.starts_with(&format!("{krate}."))
    })
}

/// EVERY Linux-only crate must be declared under the Linux-target table, NOT in the
/// unconditional `[dependencies]` (where it would force-resolve on the Windows verdict build).
#[test]
fn linux_only_crates_are_confined_to_the_linux_target_table() {
    let (unconditional, linux_region) = cargo_sections();
    for krate in LINUX_ONLY_CRATES {
        assert!(
            !declares_dep(&unconditional, krate),
            "#193 REGRESSION: `{krate}` is declared in the UNCONDITIONAL [dependencies] — it is \
             a Linux-only crate and must live under [target.'cfg(target_os = \"linux\")'.\
             dependencies], else recording-verdict cannot cross-build for Windows and the \
             recording decode is forced back onto slow dev1."
        );
        assert!(
            declares_dep(&linux_region, krate),
            "#193: `{krate}` must be declared under the Linux-target dependency table (it is a \
             Linux-only appliance/hardware-glue crate)."
        );
    }
}

/// The camera APPLIANCE lib modules (v4l/alsa/cpal/evdev/fb-bound) must be
/// `#[cfg(target_os = "linux")]`-gated in src/lib.rs, so a non-Linux build of the probe
/// tooling never tries to compile them. vban (pure UDP) and probe stay cross-platform.
#[test]
fn lib_gates_the_linux_appliance_modules() {
    let s = read("src/lib.rs");
    for module in [
        "capture",
        "config",
        "display",
        "grab_record",
        "intercom",
        "ndi",
        "ndi_display",
    ] {
        let decl = format!("pub mod {module};");
        let pos = s
            .find(&decl)
            .unwrap_or_else(|| panic!("src/lib.rs must declare `{decl}`"));
        // The 80 chars before the declaration must carry the cfg gate (it sits right above).
        let preceding = &s[pos.saturating_sub(80)..pos];
        assert!(
            preceding.contains("#[cfg(target_os = \"linux\")]"),
            "#193 REGRESSION: `{module}` (a Linux-only appliance module) must be gated with \
             #[cfg(target_os = \"linux\")] immediately above its `{decl}`, else the Windows \
             recording-verdict build pulls in v4l/alsa/cpal/evdev and fails."
        );
    }
    // vban (pure UDP) must stay UNGATED — it is cross-platform and proves we gate precisely,
    // not the whole lib.
    let vban = s.find("pub mod vban;").expect("lib must declare vban");
    let before_vban = &s[vban.saturating_sub(40)..vban];
    assert!(
        !before_vban.contains("#[cfg(target_os = \"linux\")]"),
        "#193: vban is cross-platform (pure UDP) and must NOT be Linux-gated."
    );
}

/// The probe HARDWARE GLUE modules (fb/kms/presenter/painter/reader/multi_reader/run) must be
/// `#[cfg(target_os = "linux")]`-gated in src/probe/mod.rs. recording-verdict needs NONE of
/// them; gating lets the verdict cross-build for Windows.
#[test]
fn probe_mod_gates_the_hardware_glue_modules() {
    let s = read("src/probe/mod.rs");
    for module in [
        "fb",
        "kms",
        "presenter",
        "painter",
        "reader",
        "multi_reader",
        "run",
    ] {
        let decl = format!("pub mod {module};");
        let pos = s
            .find(&decl)
            .unwrap_or_else(|| panic!("src/probe/mod.rs must declare `{decl}`"));
        let preceding = &s[pos.saturating_sub(80)..pos];
        assert!(
            preceding.contains("#[cfg(target_os = \"linux\")]"),
            "#193 REGRESSION: probe glue `{module}` must be gated with #[cfg(target_os = \
             \"linux\")] above its `{decl}` (it binds /dev/fb0 / drm / v4l / evdev). \
             recording-verdict needs none of them; the gate keeps the Windows build green."
        );
    }
    // The verdict's transitive set must stay UNGATED (cross-platform) so the Windows build
    // CAN use them. Spot-check the load-bearing ones.
    for module in [
        "recording",
        "recording_verdict",
        "recording_latency",
        "burn_contiguity",
    ] {
        let decl = format!("pub mod {module};");
        let pos = s
            .find(&decl)
            .unwrap_or_else(|| panic!("src/probe/mod.rs must declare `{decl}`"));
        let preceding = &s[pos.saturating_sub(40)..pos];
        assert!(
            !preceding.contains("#[cfg(target_os = \"linux\")]"),
            "#193: `{module}` is in recording-verdict's cross-platform transitive set and must \
             NOT be Linux-gated (the Windows verdict build needs it)."
        );
    }
}

/// CI must actually cross-build recording-verdict.exe for Windows and upload it as the
/// `probe-tools-windows-amd64` artifact — the artifact the harness uploads to stream.lan to
/// decode the recording in place (#193). A structural guard on ci.yml so the build job can't
/// be silently dropped.
#[test]
fn ci_builds_and_uploads_the_windows_probe_artifact() {
    let ci = read(".github/workflows/ci.yml");
    assert!(
        ci.contains("probe-tools-windows-amd64"),
        "#193: ci.yml must upload the `probe-tools-windows-amd64` artifact (the Windows \
         recording-verdict the harness runs ON stream.lan)."
    );
    assert!(
        ci.contains("recording-verdict") && ci.contains("windows"),
        "#193: ci.yml must build recording-verdict on a Windows runner."
    );
    // The Windows job must build the probe-featured verdict bin specifically.
    assert!(
        ci.contains("--features probe") && ci.contains("--bin recording-verdict"),
        "#193: the Windows job must `cargo build --release --features probe --bin \
         recording-verdict`."
    );
}
