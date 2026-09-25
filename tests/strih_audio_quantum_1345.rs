//! issue 1345 (24.9.2026, owner accepted 25.9 "perfektne"): the strih-lx interkom audio fixes that
//! existed ONLY as hand-made files on the box are provisioned from the repo.
//!
//! - `51-minifuse-output-period.conf` (WirePlumber): the MiniFuse playback opened at period 256
//!   against the 1024 graph, ~24 xruns/s = the buzz. It must run period 1024 x 3, headroom 256.
//! - `51-strih-quantum-1024.conf` (PipeWire): `default.clock.min-quantum = 1024`, so no client can
//!   pull the graph below the MiniFuse period (the robotic cameraman).
//! - setup-strih step 12 installs both through the shared compare-then-rewrite helper, and
//!   verify-strih grades both files plus the LIVE graph quantum.
//!
//! The rendered text is the live strih-lx file text byte-for-byte (read 25.9.2026), so a verify on
//! the hand-fixed box already passes and a setup re-run logs "unchanged".
//!
//! Tier-0: pure bash sourcing + static anchors (no cargo compile locally; CI runs these).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source `lib` (repo-relative) under `set -uo pipefail` with a caller-style `fail()`, run `body`.
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

const PROVISION: &str = "scripts/lib/strih-provision.sh";
const BASELINE: &str = "scripts/lib/obs-box-baseline.sh";

const MINIFUSE_OUTPUT_PERIOD: &str = "\
# issue 1345 (24.9.2026): the MiniFuse playback opened with period 256 / buffer 768 while the graph runs
# at quantum 1024 -> ~24 xruns/s = the buzz/chop the operator heard. Match the capture side (1024 x 3).
monitor.alsa.rules = [
  {
    matches = [ { node.name = \"~alsa_output.usb-ARTURIA_MiniFuse.*\" } ]
    actions = { update-props = { api.alsa.period-size = 1024, api.alsa.period-num = 3, api.alsa.headroom = 256 } }
  }
]
";

const STRIH_QUANTUM: &str = "\
# issue 1345 (24.9.2026): keep the whole graph at the MiniFuse period (1024). With the hub capture child
# asking --latency 256 the graph dropped to 256 while the MiniFuse playback ran period 1024, and the
# cameraman sounded robotic in the operator headphones. Takes effect at the next pipewire start; the
# running session carries the same value via pw-metadata clock.force-quantum 1024.
context.properties = {
    default.clock.min-quantum = 1024
}
";

#[test]
fn the_minifuse_output_period_dropin_is_the_live_text() {
    let (c, out, err) = run(
        PROVISION,
        &[],
        "strih_wireplumber_minifuse_output_period_conf",
    );
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(out, MINIFUSE_OUTPUT_PERIOD);
}

#[test]
fn the_graph_quantum_dropin_is_the_live_text() {
    let (c, out, err) = run(PROVISION, &[], "strih_pipewire_quantum_conf");
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(out, STRIH_QUANTUM);
}

/// The live `pw-metadata -n settings` dump (strih-lx 25.9.2026, the running session: forced 1024,
/// min-quantum still the pre-restart 32).
const LIVE_SETTINGS: &str = "Found \"settings\" metadata 34
update: id:0 key:'log.level' value:'2' type:''
update: id:0 key:'clock.rate' value:'48000' type:''
update: id:0 key:'clock.allowed-rates' value:'[ 48000 ]' type:''
update: id:0 key:'clock.quantum' value:'1024' type:''
update: id:0 key:'clock.min-quantum' value:'32' type:''
update: id:0 key:'clock.max-quantum' value:'2048' type:''
update: id:0 key:'clock.force-quantum' value:'1024' type:''
update: id:0 key:'clock.force-rate' value:'0' type:''
";

fn quantum(dump: &str) -> (i32, String) {
    let (c, out, err) = run(
        PROVISION,
        &[("DUMP", dump)],
        "strih_lx_graph_quantum_ok \"$DUMP\"",
    );
    assert!(err.is_empty(), "the verdict is silent on stderr: {err}");
    (c, out)
}

#[test]
fn graph_quantum_is_ok_when_forced_to_1024_in_the_running_session() {
    let (c, out) = quantum(LIVE_SETTINGS);
    assert_eq!(c, 0, "{out}");
    assert!(out.contains("1024"), "the detail names the quantum: {out}");
}

#[test]
fn graph_quantum_is_ok_after_a_restart_through_the_min_quantum_floor() {
    // After a reboot the force is gone (0) and the drop-in's min-quantum 1024 holds the floor.
    let dump = LIVE_SETTINGS
        .replace(
            "'clock.force-quantum' value:'1024'",
            "'clock.force-quantum' value:'0'",
        )
        .replace(
            "'clock.min-quantum' value:'32'",
            "'clock.min-quantum' value:'1024'",
        );
    let (c, out) = quantum(&dump);
    assert_eq!(c, 0, "{out}");
}

#[test]
fn graph_quantum_fails_when_a_client_can_pull_it_below_1024() {
    // no force, floor 32: the hub capture child's request would drop the graph (the robotic cameraman)
    let unforced = LIVE_SETTINGS.replace(
        "'clock.force-quantum' value:'1024'",
        "'clock.force-quantum' value:'0'",
    );
    let (c, out) = quantum(&unforced);
    assert_eq!(c, 1, "{out}");
    // forced to a different value
    let forced256 = LIVE_SETTINGS.replace(
        "'clock.force-quantum' value:'1024'",
        "'clock.force-quantum' value:'256'",
    );
    let (c, out) = quantum(&forced256);
    assert_eq!(c, 1, "{out}");
}

#[test]
fn graph_quantum_fails_closed_on_an_unreadable_session() {
    for dump in ["", "Failed to connect to PipeWire\n"] {
        let (c, out) = quantum(dump);
        assert_eq!(c, 1, "an unreadable session is never a pass: {out}");
        assert!(out.contains("unreadable"), "{out}");
    }
}

/// The ONE compare-then-rewrite install helper both slices use: writes when absent/different,
/// leaves an identical file untouched, logs each outcome, applies the mode.
#[test]
fn write_if_changed_writes_rewrites_and_leaves_an_identical_file_alone() {
    let body = r#"
d="$(mktemp -d)"; f="$d/sub.conf"; own="$(id -un):$(id -gn)"
printf 'a\n' | obs_box_write_if_changed "$f" 0644 "$own" "test drop-in" || exit 9
[ "$(cat "$f")" = a ] || exit 10
[ "$(stat -c %a "$f")" = 644 ] || exit 11
m1="$(stat -c %Y "$f")"
touch -d '2001-01-01' "$f"
printf 'a\n' | obs_box_write_if_changed "$f" 0644 "$own" "test drop-in" || exit 12
[ "$(stat -c %Y "$f")" = "$(date -d 2001-01-01 +%s)" ] || exit 13
printf 'b\n' | obs_box_write_if_changed "$f" 0755 "$own" "test drop-in" || exit 14
[ "$(cat "$f")" = b ] || exit 15
[ "$(stat -c %a "$f")" = 755 ] || exit 16
ls -A "$d" | grep -v '^sub.conf$' && exit 17
rm -rf "$d"; : "$m1"
"#;
    let (c, out, err) = run(BASELINE, &[], body);
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "one log line per call: {out}");
    assert!(lines[0].contains("written"), "{out}");
    assert!(lines[1].contains("unchanged"), "{out}");
    assert!(lines[2].contains("written"), "{out}");
    assert!(lines.iter().all(|l| l.contains("test drop-in")), "{out}");
}

#[test]
fn write_if_changed_fails_loud_on_an_unwritable_destination() {
    let (c, _out, err) = run(
        BASELINE,
        &[],
        "printf 'a\\n' | obs_box_write_if_changed /nonexistent-dir-1345/x.conf 0644 \"$(id -un):$(id -gn)\" probe",
    );
    assert_eq!(c, 1);
    assert!(err.contains("FAIL:"), "{err}");
}

/// The text of setup-strih.sh between the `step 12 "` banner and the `step 13 "` banner.
fn setup_step12() -> String {
    let s = read("scripts/setup-strih.sh");
    let start = s.find("\nstep 12 \"").expect("setup-strih step 12 banner");
    let end = s.find("\nstep 13 \"").expect("setup-strih step 13 banner");
    s[start..end].to_string()
}

#[test]
fn setup_strih_step12_installs_both_dropins_through_the_idempotent_helper() {
    let s = setup_step12();
    for (renderer, dest) in [
        (
            "strih_wireplumber_minifuse_output_period_conf",
            "${USER_HOME}/.config/wireplumber/wireplumber.conf.d/51-minifuse-output-period.conf",
        ),
        (
            "strih_pipewire_quantum_conf",
            "${USER_HOME}/.config/pipewire/pipewire.conf.d/51-strih-quantum-1024.conf",
        ),
    ] {
        let line = s
            .lines()
            .find(|l| l.contains(renderer))
            .unwrap_or_else(|| panic!("step 12 must render {renderer}:\n{s}"));
        assert!(
            line.contains("obs_box_write_if_changed") && line.contains(dest),
            "step 12 must install {renderer} via obs_box_write_if_changed into {dest}: {line}"
        );
    }
}

#[test]
fn verify_strih_grades_both_dropins_and_the_live_quantum() {
    let v = read("scripts/verify-strih.sh");
    for want in [
        "strih_wireplumber_minifuse_output_period_conf",
        "strih_pipewire_quantum_conf",
        "51-minifuse-output-period.conf",
        "51-strih-quantum-1024.conf",
        "pw-metadata -n settings",
        "strih_lx_graph_quantum_ok",
    ] {
        assert!(v.contains(want), "verify-strih.sh must carry `{want}`");
    }
    // The grade sits with the other audio item, before the closing items the item-33 test slices.
    let item = v.find("# 9b) ").expect("verify-strih item 9b");
    let item10 = v.find("# 10) NVENC").expect("item 10");
    assert!(item < item10, "item 9b sits right after item 9");
}
