//! issue 1357: the panel-brightness keys on every OBS-box kiosk (the shared baseline).
//!
//! Openbox, the kiosk WM of the shared baseline, has no brightness handler, so the notebook Fn
//! brightness keys did nothing (the owner hit it on strih-lx during the 24.9 production; imag had
//! the same problem). The hand fix live on strih-lx (24.9.2026 16:58) is now a kiosk facet in
//! `scripts/lib/obs-box-kiosk.sh`:
//!
//! - the helper `/usr/local/bin/obs-box-brightness up|down` (sysfs backlight, 10 % step, 5 % floor);
//! - the udev rule `/etc/udev/rules.d/90-obs-box-backlight.rules` (group `video` may write);
//! - the two `XF86MonBrightnessUp/Down` keybinds MERGED into the kiosk rc.xml (the user's rc.xml,
//!   else the stock one) -- only the missing lines, never replacing operator content.
//!
//! The rendered helper + rule are the live strih-lx file text byte-for-byte (read 25.9.2026).
//! The shared grader row (`baseline:brightness`) is pinned in `tests/obs_box_baseline_1357.rs`.
//!
//! Tier-0: pure bash sourcing + static anchors (CI runs these; no cargo compile locally).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const KIOSK: &str = "scripts/lib/obs-box-kiosk.sh";
const VERIFY_LIB: &str = "scripts/lib/obs-box-baseline-verify.sh";

/// Source `lib` under `set -uo pipefail` with a caller-style `fail()`, run `body`.
fn run(lib: &str, env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let harness = format!(
        "set -uo pipefail\nfail() {{ echo \"FAIL: $1\" >&2; exit 1; }}\nYELLOW=''; NC=''\n. \"$LIB\"\n{body}"
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("LIB", manifest_dir().join(lib));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The body of one kiosk-lib function (`NAME() {` .. its column-0 `}` + blank line).
fn kiosk_fn(name: &str) -> String {
    let lib = format!("{}\n", read(KIOSK));
    let head = format!("\n{name}() {{\n");
    let start = lib
        .find(&head)
        .unwrap_or_else(|| panic!("the kiosk lib must define {name}()"));
    let end = start
        + 1
        + lib[start + 1..]
            .find("\n}\n")
            .unwrap_or_else(|| panic!("{name}() must close with a column-0 `}}`"));
    lib[start..end].to_string()
}

const HELPER: &str = r#"#!/bin/bash
# Laptop panel brightness step for the OBS-box kiosk (openbox has no brightness handler).
# Usage: obs-box-brightness up|down   (bound to XF86MonBrightnessUp/Down in ~/.config/openbox/rc.xml)
set -euo pipefail
dir="$(ls -d /sys/class/backlight/* 2>/dev/null | head -1)"
[ -n "$dir" ] || { logger -t obs-box-brightness "no backlight device"; exit 1; }
max="$(cat "$dir/max_brightness")"; cur="$(cat "$dir/brightness")"
step=$(( max / 10 )); [ "$step" -ge 1 ] || step=1
case "${1:-}" in
  up)   new=$(( cur + step )) ;;
  down) new=$(( cur - step )) ;;
  *)    echo "usage: $0 up|down" >&2; exit 2 ;;
esac
[ "$new" -gt "$max" ] && new="$max"
min=$(( max / 20 )); [ "$new" -lt "$min" ] && new="$min"
echo "$new" > "$dir/brightness"
"#;

const UDEV_RULE: &str = r#"# OBS-box kiosk: let the desktop user (group video) step the panel backlight from openbox key bindings.
ACTION=="add", SUBSYSTEM=="backlight", RUN+="/bin/chgrp video /sys/class/backlight/%k/brightness", RUN+="/bin/chmod g+w /sys/class/backlight/%k/brightness"
"#;

const KEYBINDS: &str = r#"  <!-- OBS-box kiosk: panel brightness keys (openbox has no brightness handler) -->
  <keybind key="XF86MonBrightnessUp"><action name="Execute"><command>/usr/local/bin/obs-box-brightness up</command></action></keybind>
  <keybind key="XF86MonBrightnessDown"><action name="Execute"><command>/usr/local/bin/obs-box-brightness down</command></action></keybind>
"#;

#[test]
fn the_helper_is_the_live_text() {
    let (c, out, err) = run(KIOSK, &[], "obs_box_brightness_helper_text");
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(out, HELPER);
}

#[test]
fn the_udev_rule_is_the_live_text() {
    let (c, out, err) = run(KIOSK, &[], "obs_box_backlight_udev_rule");
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(out, UDEV_RULE);
}

#[test]
fn the_keybinds_are_the_live_lines() {
    let (c, out, err) = run(KIOSK, &[], "obs_box_brightness_keybinds_xml");
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(out, KEYBINDS);
}

/// Run the rendered helper against a fake sysfs backlight dir (the ONE test-only rewrite is the
/// sysfs root; the logic under test is the rendered text).
fn helper(max: u32, cur: u32, arg: &str) -> (i32, String, String) {
    let body = format!(
        r#"
d="$(mktemp -d)"; mkdir -p "$d/bl/panel"
printf '{max}\n' > "$d/bl/panel/max_brightness"; printf '{cur}\n' > "$d/bl/panel/brightness"
obs_box_brightness_helper_text | sed "s#/sys/class/backlight#$d/bl#" > "$d/h.sh"
bash "$d/h.sh" {arg}; rc=$?
echo "NOW=$(cat "$d/bl/panel/brightness")"; rm -rf "$d"; exit "$rc"
"#
    );
    run(KIOSK, &[], &body)
}

#[test]
fn the_helper_steps_by_a_tenth_and_clamps_to_max_and_a_five_percent_floor() {
    let (c, out, _e) = helper(1000, 500, "up");
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("NOW=600"), "{out}");
    let (c, out, _e) = helper(1000, 500, "down");
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("NOW=400"), "{out}");
    let (c, out, _e) = helper(1000, 950, "up");
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("NOW=1000"), "clamped to max: {out}");
    let (c, out, _e) = helper(1000, 80, "down");
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("NOW=50"), "never below 5 %: {out}");
    // a tiny range still moves by at least one step
    let (c, out, _e) = helper(7, 3, "up");
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("NOW=4"), "{out}");
}

#[test]
fn the_helper_refuses_a_bad_argument_and_writes_nothing() {
    let (c, out, err) = helper(1000, 500, "sideways");
    assert_eq!(c, 2, "{out} {err}");
    assert!(out.contains("NOW=500"), "{out}");
    assert!(err.contains("usage:"), "{err}");
}

const STOCK_RC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<openbox_config xmlns="http://openbox.org/3.4/rc">
<keyboard>
  <keybind key="Print">
    <action name="Execute"><command>scrot</command></action>
  </keybind>
</keyboard>

<mouse>
  <context name="Root">
    <mousebind button="Right" action="Press">
      <action name="ShowMenu"><menu>root-menu</menu></action>
    </mousebind>
  </context>
</mouse>
</openbox_config>
"#;

fn merge(rc: &str) -> (i32, String) {
    let (c, out, err) = run(
        KIOSK,
        &[("RC", rc)],
        "printf '%s' \"$RC\" | obs_box_openbox_rc_with_brightness_keys",
    );
    assert!(err.is_empty(), "the merge is silent on stderr: {err}");
    (c, out)
}

#[test]
fn the_merge_inserts_the_three_live_lines_right_before_keyboard_close() {
    let (c, out) = merge(STOCK_RC);
    assert_eq!(c, 0, "{out}");
    let want = STOCK_RC.replacen("</keyboard>\n", &format!("{KEYBINDS}</keyboard>\n"), 1);
    assert_eq!(
        out, want,
        "exactly the live diff: 3 lines before </keyboard>"
    );
}

#[test]
fn the_merge_is_idempotent_and_never_duplicates() {
    let (_c, once) = merge(STOCK_RC);
    let (c, twice) = merge(&once);
    assert_eq!(c, 0);
    assert_eq!(twice, once, "a second pass changes nothing");
}

#[test]
fn the_merge_adds_only_the_missing_keybind() {
    let up = KEYBINDS.lines().nth(1).unwrap();
    let partial = STOCK_RC.replacen("</keyboard>\n", &format!("{up}\n</keyboard>\n"), 1);
    let (c, out) = merge(&partial);
    assert_eq!(c, 0, "{out}");
    assert_eq!(out.matches("XF86MonBrightnessUp").count(), 1, "{out}");
    assert_eq!(out.matches("XF86MonBrightnessDown").count(), 1, "{out}");
    assert!(
        out.contains("<context name=\"Root\">"),
        "operator content kept"
    );
}

#[test]
fn the_merge_refuses_an_rc_without_a_keyboard_section_and_prints_nothing() {
    let (c, out) = merge("<openbox_config>\n</openbox_config>\n");
    assert_eq!(c, 1);
    assert!(out.is_empty(), "no partial rc.xml: {out}");
}

/// The install: helper 0755 + udev rule + a backlight trigger + the video group + the rc.xml merge,
/// every file through the shared compare-then-rewrite helper (one writer per file).
#[test]
fn the_install_writes_every_part_through_the_idempotent_helper() {
    let f = kiosk_fn("obs_box_brightness_keys");
    for want in [
        "obs_box_brightness_helper_text | obs_box_write_if_changed /usr/local/bin/obs-box-brightness 0755 root:root",
        "obs_box_backlight_udev_rule | obs_box_write_if_changed /etc/udev/rules.d/90-obs-box-backlight.rules 0644 root:root",
        "udevadm trigger --subsystem-match=backlight --action=add",
        "usermod -aG video \"$DESKTOP_USER\"",
        "/etc/xdg/openbox/rc.xml",
        "obs_box_openbox_rc_with_brightness_keys",
    ] {
        assert!(f.contains(want), "obs_box_brightness_keys must carry `{want}`:\n{f}");
    }
    assert_eq!(
        f.matches("obs_box_write_if_changed").count(),
        3,
        "helper, udev rule and rc.xml each have exactly one writer"
    );
}

/// The facet is part of the kiosk item, so setup-imag and setup-strih both get it with no extra call.
#[test]
fn the_kiosk_item_runs_the_brightness_facet() {
    let k = kiosk_fn("obs_box_kiosk");
    let call = k
        .find("obs_box_brightness_keys \"$DESKTOP_USER\"")
        .expect("obs_box_kiosk must run the brightness facet");
    let install = k
        .find("apt-get install -y openbox")
        .expect("openbox install");
    assert!(
        install < call,
        "the merge reads the stock rc.xml, so openbox is installed first"
    );
}

/// The grader reads the keybinds from the SAME renderer the install merges (embedded via
/// declare -f), so the two can never drift.
#[test]
fn the_gather_embeds_the_keybind_renderer_and_emits_the_brightness_facts() {
    let (c, out, err) = run(
        VERIFY_LIB,
        &[],
        "obs_box_baseline_gather_snippet strih nosuchuser strih-obs.service",
    );
    assert_eq!(c, 0, "{err}");
    assert!(
        out.contains("obs_box_brightness_keybinds_xml ()"),
        "the keybind renderer is embedded"
    );
    let (c, facts, err) = run(
        VERIFY_LIB,
        &[],
        "bash -c \"$(obs_box_baseline_gather_snippet nosuchbox nosuchuser nosuch.service)\"",
    );
    assert_eq!(c, 0, "{err}");
    for key in [
        "brightness_helper=",
        "brightness_rule=",
        "brightness_keys=",
        "brightness_group=",
    ] {
        assert!(facts.contains(key), "gather must emit {key}:\n{facts}");
    }
}

#[test]
fn the_new_functions_are_defined_and_sourcing_stays_silent() {
    let (c, out, err) = run(
        VERIFY_LIB,
        &[],
        "for f in obs_box_brightness_helper_text obs_box_backlight_udev_rule obs_box_brightness_keybinds_xml \
         obs_box_openbox_rc_with_brightness_keys obs_box_brightness_keys obs_box_write_if_changed; do \
           type -t \"$f\" >/dev/null || echo \"MISSING $f\"; done",
    );
    assert_eq!(c, 0, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.is_empty(), "{err}");
}

/// The gather text with the three brightness paths pointed at fixture files, plus an optional
/// replacement of the embedded keybind renderer (the only test-only rewrites; the grading logic
/// under test is the real gather).
fn brightness_facts(dir: &std::path::Path, empty_keybind_renderer: bool) -> String {
    let (c, snippet, err) = run(
        VERIFY_LIB,
        &[],
        "obs_box_baseline_gather_snippet strih nosuchuser-1357 strih-obs.service",
    );
    assert_eq!(c, 0, "{err}");
    let mut snippet = snippet
        .replace(
            "_bh=/usr/local/bin/obs-box-brightness",
            &format!("_bh={}/helper", dir.display()),
        )
        .replace(
            "_br=/etc/udev/rules.d/90-obs-box-backlight.rules",
            &format!("_br={}/rule", dir.display()),
        )
        .replace(
            "[ -f \"$_rcx\" ] || _rcx=/etc/xdg/openbox/rc.xml",
            &format!("[ -f \"$_rcx\" ] || _rcx={}/rc.xml", dir.display()),
        );
    assert!(
        snippet.contains(&format!("{}/helper", dir.display()))
            && snippet.contains(&format!("{}/rule", dir.display()))
            && snippet.contains(&format!("{}/rc.xml", dir.display())),
        "the gather must carry the three brightness paths:\n{snippet}"
    );
    if empty_keybind_renderer {
        snippet = snippet.replacen(
            "\nset +e\n",
            "\nobs_box_brightness_keybinds_xml() { :; }\nset +e\n",
            1,
        );
    }
    let out = Command::new("bash")
        .arg("-c")
        .arg(&snippet)
        .output()
        .expect("run the gather");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fact<'a>(facts: &'a str, key: &str) -> &'a str {
    facts
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key}= in:\n{facts}"))
}

fn write_fixture(dir: &std::path::Path, helper: &str, helper_mode: u32, rule: &str, rc: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(dir.join("helper"), helper).unwrap();
    std::fs::set_permissions(
        dir.join("helper"),
        std::fs::Permissions::from_mode(helper_mode),
    )
    .unwrap();
    std::fs::write(dir.join("rule"), rule).unwrap();
    std::fs::write(dir.join("rc.xml"), rc).unwrap();
}

fn merged_stock_rc() -> String {
    STOCK_RC.replacen("</keyboard>\n", &format!("{KEYBINDS}</keyboard>\n"), 1)
}

#[test]
fn the_gather_passes_a_provisioned_box_on_all_three_files() {
    let d = tempfile::tempdir().unwrap();
    write_fixture(d.path(), HELPER, 0o755, UDEV_RULE, &merged_stock_rc());
    let facts = brightness_facts(d.path(), false);
    assert_eq!(fact(&facts, "brightness_helper"), "1", "{facts}");
    assert_eq!(fact(&facts, "brightness_rule"), "1", "{facts}");
    assert_eq!(fact(&facts, "brightness_keys"), "1", "{facts}");
}

#[test]
fn the_gather_grades_the_helper_and_rule_by_content_not_presence() {
    let d = tempfile::tempdir().unwrap();
    let drifted_helper = HELPER.replace("max / 10", "max / 5");
    let drifted_rule = UDEV_RULE.replace("ACTION==\"add\"", "ACTION==\"change\"");
    write_fixture(
        d.path(),
        &drifted_helper,
        0o755,
        &drifted_rule,
        &merged_stock_rc(),
    );
    let facts = brightness_facts(d.path(), false);
    assert_eq!(
        fact(&facts, "brightness_helper"),
        "0",
        "drifted helper: {facts}"
    );
    assert_eq!(
        fact(&facts, "brightness_rule"),
        "0",
        "drifted rule: {facts}"
    );
    // the right text but not executable is not a working helper
    write_fixture(d.path(), HELPER, 0o644, UDEV_RULE, &merged_stock_rc());
    let facts = brightness_facts(d.path(), false);
    assert_eq!(
        fact(&facts, "brightness_helper"),
        "0",
        "non-executable: {facts}"
    );
}

#[test]
fn the_gather_fails_the_keybinds_when_one_is_missing() {
    let d = tempfile::tempdir().unwrap();
    let up = KEYBINDS.lines().nth(1).unwrap();
    let only_up = STOCK_RC.replacen("</keyboard>\n", &format!("{up}\n</keyboard>\n"), 1);
    write_fixture(d.path(), HELPER, 0o755, UDEV_RULE, &only_up);
    let facts = brightness_facts(d.path(), false);
    assert_eq!(fact(&facts, "brightness_keys"), "0", "{facts}");
}

/// Fail-closed: a keybind renderer that yields nothing (renamed, not embedded, a broken heredoc)
/// must never read as "every keybind present".
#[test]
fn the_gather_keybind_check_fails_closed_when_the_renderer_yields_nothing() {
    let d = tempfile::tempdir().unwrap();
    write_fixture(d.path(), HELPER, 0o755, UDEV_RULE, &merged_stock_rc());
    let facts = brightness_facts(d.path(), true);
    assert_eq!(fact(&facts, "brightness_keys"), "0", "{facts}");
}
