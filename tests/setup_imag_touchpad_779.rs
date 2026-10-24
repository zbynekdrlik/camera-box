//! #779 — imag-nb touchpad usability must be REPROVISION-DURABLE.
//!
//! The touchpad config (tap-to-click + natural scrolling + a gentler scroll step) was hand-placed
//! LIVE on the box as `/etc/X11/xorg.conf.d/30-touchpad-tap.conf` (owner root, 2026-07-15) and
//! WORKS — but `scripts/setup-imag.sh`, the single unambiguous (re)provisioner, never generated it.
//! A reimage would therefore silently drop tap-to-click, exactly the "provisioning gap hidden by a
//! hand patch" class (issue 840) the ticket's "Zostáva" section calls out.
//!
//! These content-asserts pin that `setup-imag.sh` bakes the file in, with the five `Option`s at their
//! values (`Tapping`/`TappingDrag`/`NaturalScrolling` on, `ScrollPixelDistance` 15 and `AccelSpeed`
//! 0.3 — the 23.10.2026 operator finding of issue 1465 item 5, which replaced the July value 50 that
//! made two-finger scrolling far too slow on the stream boxes). Style follows
//! `tests/setup_imag_guards.rs`: read the
//! REAL script and assert on the REAL contract via `contains()` (each needle is unique in the
//! script, so no static-anchor collision — verified before writing this file).

use std::path::PathBuf;

/// setup-imag.sh's provisioning text. issue 1357: the step-25 touchpad item moved VERBATIM into the
/// shared OBS-box baseline (scripts/lib/obs-box-kiosk.sh, obs_box_touchpad) that setup-imag.sh calls
/// and setup-strih.sh runs too, so the text is setup-imag.sh PLUS the baseline libs.
fn setup_body() -> String {
    let read = |rel: &str| {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    };
    format!(
        "{}\n{}\n{}",
        read("scripts/setup-imag.sh"),
        read("scripts/lib/obs-box-baseline.sh"),
        read("scripts/lib/obs-box-kiosk.sh")
    )
}

/// The provisioner must WRITE the xorg.conf.d InputClass file — presence of the literal path proves
/// the file is generated at provisioning, not left to a hand patch (the issue-840 lesson).
#[test]
fn setup_imag_generates_the_touchpad_xorg_conf_779() {
    let body = setup_body();
    assert!(
        body.contains("/etc/X11/xorg.conf.d/30-touchpad-tap.conf"),
        "scripts/setup-imag.sh must GENERATE /etc/X11/xorg.conf.d/30-touchpad-tap.conf (#779) — \
         without it, a reprovision silently drops tap-to-click (the hand-placed live file is not \
         durable). This is the issue-840 provisioning-gap class."
    );
}

/// The generated InputClass must carry EXACTLY the five options at their values, plus
/// the `MatchIsTouchpad`/`Driver "libinput"` selector — a reprovisioned box must reproduce what is
/// live on the box today, not a partial subset.
#[test]
fn setup_imag_touchpad_conf_has_all_five_options_779() {
    let body = setup_body();
    for needle in [
        "MatchIsTouchpad \"on\"",
        "Driver \"libinput\"",
        "Option \"Tapping\" \"on\"",
        "Option \"TappingDrag\" \"on\"",
        "Option \"NaturalScrolling\" \"on\"",
        "Option \"ScrollPixelDistance\" \"15\"",
        "Option \"AccelSpeed\" \"0.3\"",
    ] {
        assert!(
            body.contains(needle),
            "scripts/setup-imag.sh's touchpad InputClass (#779) must contain `{needle}` — the \
             bake-in must reproduce the 30-touchpad-tap.conf byte-for-byte \
             (Tapping/TappingDrag/NaturalScrolling on, ScrollPixelDistance 15 + AccelSpeed 0.3 — \
             the 23.10.2026 operator finding, issue 1465 item 5)."
        );
    }
    assert!(
        !body.contains("Option \"ScrollPixelDistance\" \"50\""),
        "the July ScrollPixelDistance 50 is gone (issue 1465 item 5: far too slow on the stream boxes)"
    );
}

/// The bake-in must be a real numbered provisioning step (not a stray untracked block) so the
/// `TOTAL_STEPS`/step-count invariant in `setup_imag_guards.rs` accounts for it and the progress
/// display stays honest.
#[test]
fn setup_imag_touchpad_is_a_numbered_step_779() {
    let body = setup_body();
    assert!(
        body.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("step ") && t.to_lowercase().contains("touchpad")
        }),
        "scripts/setup-imag.sh must provision the touchpad config as a numbered `step N \"...\"` \
         mentioning touchpad (#779), so TOTAL_STEPS accounts for it."
    );
}
