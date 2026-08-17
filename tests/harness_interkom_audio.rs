//! #782 — pure-function guard for `scripts/lib/interkom-audio.sh`, the single source of truth for
//! the interkom (USB Audio+HID headset) ALSA provisioning: the canonical by-NAME `/etc/asound.conf`
//! body, the per-box Mic/PCM mixer-gain table, and the read-back parsers.
//!
//! `setup-device.sh` (writes/applies) and `verify-device.sh` (checks) BOTH source this lib, so a
//! re-provisioned box can never drift from what the acceptance gate verifies — this file proves the
//! lib's functions directly (same convention as `tests/setup_device_pure_functions.rs`), and the
//! two `*_pure_functions.rs` files prove the two scripts actually wire it in.
//!
//! The canonical asound.conf byte-content is pinned by its live-fleet sha256 (`d5db405c...`, read
//! from cam2/cam3 on 2026-08-17) — the `provisioning-scripts.md` "hash the live fleet file against
//! the heredoc body" recipe — so any accidental edit to the heredoc that changes ALSA behavior is
//! caught, while the comment/body stays byte-identical to what the hand-unified fleet runs.
//!
//! RED before `scripts/lib/interkom-audio.sh` exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/interkom-audio.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib (a source-only pure-function library — no `main`, no side effects) and run
/// `body` against its functions. Returns (exit_code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn sourced_stdout(body: &str) -> String {
    let (code, out, err) = run_sourced(body);
    assert_eq!(code, 0, "harness should exit 0. stderr: {err}");
    out
}

/// The canonical `/etc/asound.conf` body MUST be byte-identical to the live hand-unified fleet
/// (sha256 d5db405c..., cam2/cam3 majority form, read live 2026-08-17). This is the strongest form
/// of "mirror it byte-for-byte" — a functional edit that changes ALSA behavior moves the hash.
#[test]
fn asound_conf_content_matches_live_fleet_sha256() {
    let out = sourced_stdout("interkom_asound_conf_content | sha256sum | cut -d' ' -f1");
    assert_eq!(
        out.trim(),
        "d5db405cc1e0a2dd857caab6566ff913b9a8ba1404a4b71e1ce830ac23475b7d",
        "interkom_asound_conf_content must be byte-identical to the live hand-unified fleet asound.conf"
    );
}

/// The canonical config is the by-NAME form: it references `sysdefault:CARD=HID` and `card HID`, and
/// carries NO enumeration-time card NUMBER (`hw:0,0`, `hw:1,0`, ...) — the dangling-card class #728
/// is exactly what by-NAME defeats.
#[test]
fn asound_conf_content_is_by_name_never_a_card_number() {
    let cfg = sourced_stdout("interkom_asound_conf_content");
    assert!(
        cfg.contains("sysdefault:CARD=HID"),
        "asound.conf must reference the HID card by NAME. got:\n{cfg}"
    );
    assert!(
        cfg.contains("card HID"),
        "asound.conf ctl.!default must be `card HID`. got:\n{cfg}"
    );
    for bad in ["hw:0,0", "hw:1,0", "hw:2,0", "hw:$USB_CARD"] {
        assert!(
            !cfg.contains(bad),
            "asound.conf must NOT bake a card NUMBER ('{bad}') — that dangles on re-enumeration (#728). got:\n{cfg}"
        );
    }
}

/// Per-box Mic (capture) gain: cam5-7 = 80 (analog-headset compensation), every other box = 75
/// (the hand-tuned cam1-4 reference). Owner's final table (2026-07-15 20:24).
#[test]
fn mic_pct_per_box_table() {
    for name in ["CAM1", "CAM2", "CAM3", "CAM4"] {
        assert_eq!(
            sourced_stdout(&format!("interkom_mic_pct {name}")).trim(),
            "75",
            "{name} Mic gain must be 75%"
        );
    }
    for name in ["CAM5", "CAM6", "CAM7"] {
        assert_eq!(
            sourced_stdout(&format!("interkom_mic_pct {name}")).trim(),
            "80",
            "{name} Mic gain must be 80% (per-box compensation)"
        );
    }
    // An unknown/new box defaults to the reference baseline, never the CSCTEK power-on 91%.
    assert_eq!(
        sourced_stdout("interkom_mic_pct CAM9").trim(),
        "75",
        "an unknown box must default to the 75% reference baseline"
    );
}

/// Per-box PCM (playback / headphone) gain: cam5-7 = 94, every other box = 79.
#[test]
fn pcm_pct_per_box_table() {
    for name in ["CAM1", "CAM2", "CAM3", "CAM4"] {
        assert_eq!(
            sourced_stdout(&format!("interkom_pcm_pct {name}")).trim(),
            "79",
            "{name} PCM gain must be 79%"
        );
    }
    for name in ["CAM5", "CAM6", "CAM7"] {
        assert_eq!(
            sourced_stdout(&format!("interkom_pcm_pct {name}")).trim(),
            "94",
            "{name} PCM gain must be 94% (per-box compensation)"
        );
    }
    assert_eq!(
        sourced_stdout("interkom_pcm_pct CAM9").trim(),
        "79",
        "an unknown box must default to the 79% reference baseline"
    );
}

/// `interkom_amixer_pct` extracts the FIRST `[NN%]` from a real `amixer sget` block, "" if none.
/// The two fixture blocks are the exact live cam2 output shape (Mic mono, PCM two channels).
#[test]
fn amixer_pct_parses_the_first_percent() {
    assert_eq!(
        sourced_stdout(r#"interkom_amixer_pct "  Mono: Capture 384 [75%] [-8.00dB] [on]""#).trim(),
        "75"
    );
    // Two channels — must take the FIRST percent, not concatenate.
    let pcm = "  Front Left: Playback 704 [79%] [-20.00dB] [on]\n  Front Right: Playback 704 [79%] [-20.00dB] [on]";
    assert_eq!(
        sourced_stdout(&format!("interkom_amixer_pct \"{pcm}\"")).trim(),
        "79"
    );
    // No percent at all (e.g. an unreadable/empty amixer read) -> empty, never an abort under -e.
    let (code, out, _err) = run_sourced(r#"interkom_amixer_pct """#);
    assert_eq!(code, 0, "empty input must not abort (trailing || true)");
    assert_eq!(out.trim(), "", "no percent -> empty string");
}

/// `interkom_asound_by_name_count` is >0 for the canonical by-NAME config and exactly 0 for the old
/// enumeration-time card-NUMBER form — the discriminator the (aa) acceptance check keys on.
#[test]
fn asound_by_name_count_discriminates_by_name_from_card_number() {
    let by_name =
        sourced_stdout("interkom_asound_by_name_count \"$(interkom_asound_conf_content)\"");
    assert_ne!(
        by_name.trim(),
        "0",
        "the canonical by-NAME config must count > 0"
    );
    // The OLD dangling card-number form has zero CARD=HID references.
    let (code, out, _e) =
        run_sourced(r#"interkom_asound_by_name_count "pcm \"hw:0,0\" channels 2""#);
    assert_eq!(code, 0, "no-match must not abort (grep -c || true)");
    assert_eq!(
        out.trim(),
        "0",
        "the old card-number form must count 0 (drift signal)"
    );
}

/// The lib is a source-only pure-function library: it must NOT set `-e`/`-u`/`pipefail` (sourcing a
/// `set -e`-carrying file would silently change the CALLER's shell options — the documented reason
/// every sibling in scripts/lib/ omits it), and it must carry the `airuleset:script-ok` marker.
#[test]
fn lib_is_source_only_never_sets_shell_options() {
    let body = std::fs::read_to_string(lib()).expect("read lib");
    for banned in ["set -e", "set -u", "set -o pipefail", "set -euo pipefail"] {
        assert!(
            !body
                .lines()
                .any(|l| l.trim() == banned || l.trim().starts_with(&format!("{banned} "))),
            "a sourced lib must never `{banned}` — it would change the caller's shell options"
        );
    }
    assert!(
        body.contains("airuleset:script-ok"),
        "a source-only lib carries the airuleset:script-ok marker (pre-write-script-check bypass)"
    );
}
