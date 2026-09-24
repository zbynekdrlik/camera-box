//! Issue 1361 — the strih provisioning takes its box/venue FACTS from ONE per-box fact file
//! (`scripts/strih-boxes/<box>.env`), loaded + validated by ONE loader
//! (`scripts/lib/strih-box-facts.sh`), selected by `setup-strih.sh --box <name>` /
//! `verify-strih.sh --box <name>` (default `strih-lx`).
//!
//! What this pins:
//!   * the loader: parses the file WITHOUT executing it, refuses unknown / missing / duplicate keys,
//!     unsafe values, malformed facts, and every `TODO_OWNER` value (naming each one);
//!   * the orchestrators: `--box strih-pp` (the template) refuses; an unknown box refuses;
//!   * byte identity: every fact-dependent file/value `setup-strih.sh --box strih-lx` generates equals
//!     the PRE-change output (`tests/fixtures/strih_box_1361/strih-lx.golden`, captured from the
//!     strih-lx-literal scripts);
//!   * the fleet list's strih-lx row agrees with the fact file;
//!   * no strih-lx identity literal remains on a code line of the touched scripts.
//!
//! Std-only + shells out to bash (no root, no network, nothing written outside a temp dir).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn facts_lib() -> PathBuf {
    root().join("scripts/lib/strih-box-facts.sh")
}

/// Env vars that could leak box facts in from the CI/dev shell; always removed so the fact file is
/// the only source.
const LEAKY_ENV: &[&str] = &[
    "STRIH_LX_IP",
    "STRIH_LX_HOST",
    "STRIH_LX_DANTESYNC_ROLE",
    "STRIH_LX_NTP_SERVER",
    "STRIH_LX_TARGET_IP",
    "COMPANION_SATELLITE_HOST",
    "STRIH_NIC_IFACE",
    "STRIH_NDI_PEER",
    "STRIH_BOXES_DIR",
    "OBS_FLEET",
];

/// Run `bash -c <script>` with `env` set on top of a cleaned environment. (exit, stdout, stderr).
fn bash(env: &[(&str, &str)], script: &str) -> (i32, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script).current_dir(root());
    for k in LEAKY_ENV {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Source the facts loader and run `body`.
fn with_loader(env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let script = format!("set -uo pipefail\n. \"{}\"\n{body}", facts_lib().display());
    bash(env, &script)
}

static TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// A fresh, empty per-test directory under the system temp dir (removed by the caller's guard).
struct TmpDir(PathBuf);
impl TmpDir {
    fn new(tag: &str) -> Self {
        let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
        let p =
            std::env::temp_dir().join(format!("strih-box-1361-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        TmpDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.0.join(name), body).expect("write fixture");
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// strih-lx.env's facts, as a fixture body we can mutate one line at a time.
fn strih_lx_env() -> String {
    std::fs::read_to_string(root().join("scripts/strih-boxes/strih-lx.env"))
        .expect("scripts/strih-boxes/strih-lx.env must exist")
}

/// A fixture box named `name` whose facts are strih-lx's with `edit` applied (line-level replace).
fn fixture_box(dir: &TmpDir, name: &str, edits: &[(&str, &str)]) {
    let mut body = strih_lx_env();
    for (from, to) in edits {
        assert!(
            body.contains(from),
            "fixture edit anchor `{from}` not in strih-lx.env"
        );
        body = body.replacen(from, to, 1);
    }
    dir.write(&format!("{name}.env"), &body);
}

const FACT_KEYS: &[&str] = &[
    "STRIH_HOSTNAME",
    "STRIH_IP",
    "STRIH_NDI_PREFIX",
    "STRIH_DANTESYNC_ROLE",
    "STRIH_DANTESYNC_UPSTREAM",
    "STRIH_INTERCOM_CONFIG",
    "STRIH_NIC_DRIVER",
    "STRIH_OBS_PROFILE",
    "STRIH_OBS_COLLECTION",
    "STRIH_NDI_RUNTIME_PEER",
    "STRIH_COMPANION_HOST",
    "STRIH_CG_SENDER",
    "STRIH_CAMERAS",
];

// ---------------------------------------------------------------------------------------------
// the loader
// ---------------------------------------------------------------------------------------------

#[test]
fn loader_is_source_only_and_lists_the_fact_keys() {
    let (c, out, err) = with_loader(&[], "strih_box_fact_keys");
    assert_eq!(c, 0, "stderr={err}");
    let keys: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(keys, FACT_KEYS, "the fact key list is the contract");
}

#[test]
fn loader_loads_strih_lx_facts() {
    let (c, out, err) = with_loader(
        &[],
        "strih_box_load strih-lx || exit 9\n\
         for k in $(strih_box_fact_keys); do printf '%s=%s\\n' \"$k\" \"$(strih_box_fact \"$k\")\"; done\n\
         printf 'LOADED=%s\\n' \"$(strih_box_loaded_name)\"",
    );
    assert_eq!(c, 0, "strih-lx must load cleanly; stderr={err}");
    for want in [
        "STRIH_HOSTNAME=strih-lx",
        "STRIH_IP=10.77.9.202",
        "STRIH_NDI_PREFIX=STRIH-LX",
        "STRIH_DANTESYNC_ROLE=server",
        "STRIH_DANTESYNC_UPSTREAM=",
        "STRIH_INTERCOM_CONFIG=intercom/intercom.strih-lx.toml",
        "STRIH_NIC_DRIVER=r8152",
        "STRIH_OBS_PROFILE=strih-lx",
        "STRIH_OBS_COLLECTION=strih-lx",
        "STRIH_NDI_RUNTIME_PEER=10.77.9.61",
        "STRIH_COMPANION_HOST=10.77.9.205",
        "STRIH_CG_SENDER=RESOLUME-SNV (cg-obs)",
        "STRIH_CAMERAS=1 2 3 4 5 6 7",
        "LOADED=strih-lx",
    ] {
        assert!(
            out.lines().any(|l| l == want),
            "missing `{want}` in:\n{out}"
        );
    }
}

#[test]
fn accessors_lazily_load_the_default_box_strih_lx() {
    // No explicit load: the first accessor call loads the default box.
    let (c, out, err) = with_loader(&[], "strih_box_fact STRIH_IP; echo; strih_box_loaded_name");
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(out, "10.77.9.202\nstrih-lx", "got: {out}");
}

#[test]
fn todo_owner_template_is_refused_naming_every_missing_fact() {
    let (c, out, err) = with_loader(&[], "strih_box_load strih-pp && echo LOADED");
    assert_ne!(
        c, 0,
        "strih-pp (TODO_OWNER template) must refuse; out={out}"
    );
    assert!(!out.contains("LOADED"), "must not load: {out}");
    for k in FACT_KEYS {
        assert!(
            err.contains(&format!("{k} is TODO_OWNER")),
            "the refusal must name {k}; stderr:\n{err}"
        );
    }
}

#[test]
fn a_single_todo_owner_value_is_refused_by_name() {
    let d = TmpDir::new("todo1");
    fixture_box(
        &d,
        "strih-lx",
        &[("STRIH_NIC_DRIVER=r8152", "STRIH_NIC_DRIVER=TODO_OWNER")],
    );
    let dir = d.path().to_string_lossy().into_owned();
    let (c, _o, err) = with_loader(&[("STRIH_BOXES_DIR", &dir)], "strih_box_load strih-lx");
    assert_ne!(c, 0);
    assert!(err.contains("STRIH_NIC_DRIVER is TODO_OWNER"), "{err}");
}

/// Every malformed-fact shape refuses with a message naming the problem.
#[test]
fn malformed_fact_files_are_refused() {
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);
    let cases: Vec<Case> = vec![
        (
            "unknown key",
            vec![("STRIH_CAMERAS=", "STRIH_BOGUS=1\nSTRIH_CAMERAS=")],
            "unknown fact STRIH_BOGUS",
        ),
        (
            "duplicate key",
            vec![("STRIH_CAMERAS=", "STRIH_IP=10.77.9.202\nSTRIH_CAMERAS=")],
            "duplicate fact STRIH_IP",
        ),
        (
            "missing key",
            vec![("STRIH_NIC_DRIVER=r8152\n", "")],
            "missing fact STRIH_NIC_DRIVER",
        ),
        (
            "shell metachar",
            vec![("STRIH_OBS_PROFILE=strih-lx", "STRIH_OBS_PROFILE=$(reboot)")],
            "unsafe character",
        ),
        (
            "not KEY=value",
            vec![("STRIH_CAMERAS=", "export STRIH_X\nSTRIH_CAMERAS=")],
            "not a KEY=value line",
        ),
        (
            "bad ip",
            vec![("STRIH_IP=10.77.9.202", "STRIH_IP=10.77.9")],
            "STRIH_IP",
        ),
        (
            "bad role",
            vec![("STRIH_DANTESYNC_ROLE=server", "STRIH_DANTESYNC_ROLE=master")],
            "STRIH_DANTESYNC_ROLE",
        ),
        (
            "client without upstream",
            vec![("STRIH_DANTESYNC_ROLE=server", "STRIH_DANTESYNC_ROLE=client")],
            "STRIH_DANTESYNC_UPSTREAM",
        ),
        (
            "server with upstream",
            vec![(
                "STRIH_DANTESYNC_UPSTREAM=\n",
                "STRIH_DANTESYNC_UPSTREAM=strih.lan\n",
            )],
            "STRIH_DANTESYNC_UPSTREAM",
        ),
        (
            "prefix != upper(hostname)",
            vec![("STRIH_NDI_PREFIX=STRIH-LX", "STRIH_NDI_PREFIX=STRIH-SNV")],
            "STRIH_NDI_PREFIX",
        ),
        (
            "hostname != file name",
            vec![("STRIH_HOSTNAME=strih-lx", "STRIH_HOSTNAME=strih-other")],
            "STRIH_HOSTNAME",
        ),
        (
            "intercom config not a repo-relative intercom/*.toml",
            vec![(
                "STRIH_INTERCOM_CONFIG=intercom/intercom.strih-lx.toml",
                "STRIH_INTERCOM_CONFIG=/etc/intercom-hub/intercom.toml",
            )],
            "STRIH_INTERCOM_CONFIG",
        ),
        (
            "cameras not numbers",
            vec![("STRIH_CAMERAS=1 2 3 4 5 6 7", "STRIH_CAMERAS=1 two")],
            "STRIH_CAMERAS",
        ),
        (
            "empty required fact",
            vec![("STRIH_NIC_DRIVER=r8152", "STRIH_NIC_DRIVER=")],
            "STRIH_NIC_DRIVER is empty",
        ),
    ];
    for (label, edits, want) in cases {
        let d = TmpDir::new("bad");
        fixture_box(&d, "strih-lx", &edits);
        let dir = d.path().to_string_lossy().into_owned();
        let (c, out, err) = with_loader(
            &[("STRIH_BOXES_DIR", &dir)],
            "strih_box_load strih-lx && echo LOADED",
        );
        assert_ne!(c, 0, "[{label}] must refuse; out={out}");
        assert!(!out.contains("LOADED"), "[{label}] loaded anyway");
        assert!(
            err.contains(want),
            "[{label}] stderr must mention `{want}`:\n{err}"
        );
    }
}

#[test]
fn a_box_name_is_a_plain_name_never_a_path() {
    for bad in ["../strih-lx", "strih lx", "", "Strih-LX", "strih-lx/"] {
        let (c, _o, err) = with_loader(&[], &format!("strih_box_load '{bad}'"));
        assert_ne!(c, 0, "box name `{bad}` must be refused");
        assert!(err.contains("box name"), "`{bad}`: {err}");
    }
    let (c, _o, err) = with_loader(&[], "strih_box_load strih-nowhere");
    assert_ne!(c, 0);
    assert!(err.contains("no fact file"), "{err}");
}

#[test]
fn a_fact_file_is_parsed_never_executed() {
    let d = TmpDir::new("noexec");
    let marker = d.path().join("executed");
    // A line that would create the marker if the loader SOURCED the file.
    let edit = format!("touch {}\nSTRIH_CAMERAS=", marker.display());
    fixture_box(&d, "strih-lx", &[("STRIH_CAMERAS=", &edit)]);
    let dir = d.path().to_string_lossy().into_owned();
    let (c, _o, _e) = with_loader(&[("STRIH_BOXES_DIR", &dir)], "strih_box_load strih-lx");
    assert_ne!(c, 0, "a non KEY=value line refuses");
    assert!(!marker.exists(), "the fact file must never be executed");
}

#[test]
fn a_fleet_row_that_disagrees_with_the_fact_ip_is_refused() {
    let fleet =
        "strih-lx|10.77.9.203|linux-genlock|always\nstream|10.77.9.204|windows-genlock|always";
    let (c, _o, err) = with_loader(&[("OBS_FLEET", fleet)], "strih_box_load strih-lx");
    assert_ne!(c, 0, "fleet row .203 vs fact .202 must refuse");
    assert!(err.contains("fleet"), "{err}");
    // A box with NO fleet row yet (a new strih before go-live) is fine.
    let fleet2 = "stream|10.77.9.204|windows-genlock|always";
    let (c2, _o, err2) = with_loader(&[("OBS_FLEET", fleet2)], "strih_box_load strih-lx");
    assert_eq!(
        c2, 0,
        "no fleet row = not yet in the fleet, allowed: {err2}"
    );
}

#[test]
fn the_real_fleet_row_matches_the_fact_file() {
    let (c, out, err) = with_loader(
        &[],
        &format!(
            ". \"{}\"\nprintf '%s|%s' \"$(obs_fleet_host strih-lx)\" \"$(strih_box_fact STRIH_IP)\"",
            root().join("scripts/lib/obs-fleet.sh").display()
        ),
    );
    assert_eq!(c, 0, "{err}");
    let (fleet, fact) = out.split_once('|').expect("two fields");
    assert_eq!(
        fleet, fact,
        "obs-fleet.sh strih-lx row must equal strih-lx.env STRIH_IP"
    );
}

#[test]
fn a_per_run_env_knob_that_contradicts_a_fact_is_refused() {
    let (c, _o, err) = with_loader(&[("STRIH_LX_IP", "10.77.9.203")], "strih_box_load strih-lx");
    assert_ne!(c, 0);
    assert!(err.contains("STRIH_LX_IP"), "{err}");
    let (c2, _o, err2) = with_loader(
        &[("STRIH_LX_DANTESYNC_ROLE", "client")],
        "strih_box_load strih-lx",
    );
    assert_ne!(c2, 0);
    assert!(err2.contains("STRIH_LX_DANTESYNC_ROLE"), "{err2}");
    // An env value EQUAL to the fact is harmless (the old documented invocation still works).
    let (c3, _o, err3) = with_loader(&[("STRIH_LX_IP", "10.77.9.202")], "strih_box_load strih-lx");
    assert_eq!(c3, 0, "{err3}");
}

#[test]
fn cli_parses_box_flag_default_strih_lx() {
    for (args, want) in [
        ("", "strih-lx"),
        ("--box strih-pp", "strih-pp"),
        ("--box=strih-pp", "strih-pp"),
        ("--yes --box strih-lx", "strih-lx"),
    ] {
        let (c, out, err) = with_loader(&[], &format!("strih_box_cli_box {args}"));
        assert_eq!(c, 0, "`{args}`: {err}");
        assert_eq!(out.trim(), want, "`{args}`");
    }
    for bad in ["--box", "--frobnicate", "--box a --box b"] {
        let (c, _o, err) = with_loader(&[], &format!("strih_box_cli_box {bad}"));
        assert_ne!(c, 0, "`{bad}` must be refused");
        assert!(!err.is_empty(), "`{bad}` must say why");
    }
}

// ---------------------------------------------------------------------------------------------
// the orchestrators
// ---------------------------------------------------------------------------------------------

/// `. setup-strih.sh --box X` (the source-guard stops before the root-only flow).
fn source_orchestrator(script: &str, args: &str) -> (i32, String, String) {
    let body = format!(
        ". \"{}\" {args}\necho SOURCED-OK",
        root().join(script).display()
    );
    bash(&[], &body)
}

#[test]
fn setup_and_verify_refuse_the_todo_owner_box() {
    for script in ["scripts/setup-strih.sh", "scripts/verify-strih.sh"] {
        let (c, out, err) = source_orchestrator(script, "--box strih-pp");
        assert_ne!(c, 0, "{script} --box strih-pp must refuse; out={out}");
        assert!(
            !out.contains("SOURCED-OK"),
            "{script} continued past the refusal"
        );
        for k in FACT_KEYS {
            assert!(
                err.contains(&format!("{k} is TODO_OWNER")),
                "{script}: refusal must name {k}:\n{err}"
            );
        }
        let (c2, _o, err2) = source_orchestrator(script, "--box strih-nowhere");
        assert_ne!(c2, 0, "{script}: unknown box must refuse");
        assert!(err2.contains("no fact file"), "{script}: {err2}");
    }
}

#[test]
fn setup_and_verify_default_to_strih_lx() {
    for script in ["scripts/setup-strih.sh", "scripts/verify-strih.sh"] {
        let (c, out, err) = source_orchestrator(script, "");
        assert_eq!(c, 0, "{script}: {err}");
        assert!(out.contains("SOURCED-OK"));
        let body = format!(
            ". \"{}\"\nstrih_box_loaded_name",
            root().join(script).display()
        );
        let (_c, out2, _e) = bash(&[], &body);
        assert_eq!(out2.trim(), "strih-lx", "{script} default box");
    }
}

/// The byte-identity net: every fact-dependent output of `setup-strih.sh --box strih-lx` equals the
/// pre-change (strih-lx-literal) output.
#[test]
fn strih_lx_generated_output_is_byte_identical_to_the_pre_change_golden() {
    let render = root().join("tests/fixtures/strih_box_1361/render.sh");
    let golden =
        std::fs::read_to_string(root().join("tests/fixtures/strih_box_1361/strih-lx.golden"))
            .expect("golden");
    let body = format!(
        "bash \"{}\" \"{}\" strih-lx",
        render.display(),
        root().display()
    );
    let (c, out, err) = bash(&[], &body);
    assert_eq!(c, 0, "render failed: {err}");
    if out != golden {
        let first = out
            .lines()
            .zip(golden.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {}: got `{a}` want `{b}`", i + 1))
            .unwrap_or_else(|| format!("length differs: got {} want {}", out.len(), golden.len()));
        panic!("strih-lx output drifted from the pre-change golden -- {first}");
    }
}

#[test]
fn setup_strih_takes_every_identity_value_from_the_facts() {
    let s = std::fs::read_to_string(root().join("scripts/setup-strih.sh")).unwrap();
    for want in [
        ". \"${HERE}/lib/strih-box-facts.sh\"",
        "STRIH_BOX=\"$(strih_box_cli_box \"$@\")\"",
        "strih_box_load \"$STRIH_BOX\"",
        "DS_ROLE=\"$(strih_lx_dantesync_role)\"",
        "DS_ARGS=\"$(strih_lx_dantesync_args)\"",
        "NDI_PEER=\"${STRIH_NDI_PEER:-$(strih_lx_ndi_runtime_peer)}\"",
        // the file's existence is checked where it is installed, not at fact load (the deploy
        // plan stages scripts/ + systemd/ only, so loading must not depend on intercom/).
        "[ -f \"${HERE}/../$(strih_lx_intercom_config)\" ]",
        "\"${HERE}/../$(strih_lx_intercom_config)\" /etc/intercom-hub/intercom.toml",
        "strih_obs_box_facts_dropin_text > \"${USER_HOME}/.config/systemd/user/strih-obs.service.d/10-box-facts.conf\"",
        "\"${HERE}/verify-strih.sh\" --box \"$STRIH_BOX\"",
    ] {
        assert!(s.contains(want), "setup-strih.sh must carry `{want}`");
    }
    // The box selection + load happens BEFORE the source-guard, so a sourced setup (tests) sees the
    // same facts the real run uses.
    let load = s.find("strih_box_load \"$STRIH_BOX\"").unwrap();
    let guard = s
        .find("if [ \"${BASH_SOURCE[0]}\" != \"${0}\" ]; then")
        .unwrap();
    assert!(load < guard, "load facts before the source-guard");
}

#[test]
fn verify_strih_takes_the_box_facts() {
    let v = std::fs::read_to_string(root().join("scripts/verify-strih.sh")).unwrap();
    for want in [
        ". \"${HERE}/lib/strih-box-facts.sh\"",
        "STRIH_BOX=\"$(strih_box_cli_box \"$@\")\"",
        "strih_box_load \"$STRIH_BOX\"",
        "DS_ROLE_V=\"$(strih_lx_dantesync_role)\"",
        "IRQ_TARGET_IP=\"${STRIH_LX_TARGET_IP:-$(strih_lx_ip)}\"",
        "strih_nic_iface_by_driver /sys \"$(strih_lx_nic_driver)\"",
    ] {
        assert!(v.contains(want), "verify-strih.sh must carry `{want}`");
    }
}

#[test]
fn obs_profile_and_collection_facts_reach_the_launcher_via_a_unit_dropin() {
    let (c, out, err) = with_loader(
        &[],
        &format!(
            ". \"{}\"\nstrih_obs_box_facts_dropin_text",
            root().join("scripts/lib/strih-provision.sh").display()
        ),
    );
    assert_eq!(c, 0, "{err}");
    assert_eq!(
        out,
        "[Service]\nEnvironment=\"STRIH_OBS_PROFILE=strih-lx\"\nEnvironment=\"STRIH_OBS_COLLECTION=strih-lx\"\n",
        "drop-in text"
    );
    // the launcher still reads exactly these two env names.
    let l = std::fs::read_to_string(root().join("scripts/strih-obs-start.sh")).unwrap();
    assert!(l.contains("STRIH_OBS_PROFILE=\"${STRIH_OBS_PROFILE:-"));
    assert!(l.contains("STRIH_OBS_COLLECTION=\"${STRIH_OBS_COLLECTION:-"));
}

#[test]
fn seed_manifest_follows_the_camera_and_cg_facts() {
    let d = TmpDir::new("seed");
    fixture_box(
        &d,
        "strih-lx",
        &[
            ("STRIH_CAMERAS=1 2 3 4 5 6 7", "STRIH_CAMERAS=2 5"),
            (
                "STRIH_CG_SENDER=RESOLUME-SNV (cg-obs)",
                "STRIH_CG_SENDER=none",
            ),
        ],
    );
    let dir = d.path().to_string_lossy().into_owned();
    let (c, out, err) = with_loader(
        &[("STRIH_BOXES_DIR", &dir)],
        &format!(
            ". \"{}\"\nstrih_box_load strih-lx || exit 9\nstrih_lx_seed_manifest_json | python3 -c 'import json,sys; d=json.load(sys.stdin); print(\" \".join(i[\"input\"] for i in d[\"inputs\"]))'",
            root().join("scripts/lib/strih-provision.sh").display()
        ),
    );
    assert_eq!(c, 0, "{err}");
    assert_eq!(out.trim(), "NDI cam2 NDI cam5 NDI 2ME PVW NDI 2ME PGM (mv)");
}

// ---------------------------------------------------------------------------------------------
// the static net: no strih-lx identity literal outside strih-lx.env
// ---------------------------------------------------------------------------------------------

/// Tokens that legitimately contain an identity string and are NOT a box fact:
///   * the on-box ROLE artifact paths (the same name on every strih box, read by strih_scenes.py /
///     strih-obs-start.sh -- historical names, not the box identity);
///   * the retired logind drop-in a self-heal removes (a historical strih-lx-only file name);
///   * the committed strih-nic-irq-affinity.service Description (pinned byte-identical to systemd/);
///   * the loader's default-box constant (the selector default, the dispatch's `--box` default).
const ALLOWED_TOKENS: &[&str] = &[
    "strih-lx-seed.json",
    "strih-lx-projector.json",
    "strih-lx-profile-facts.txt",
    "90-strih-lx.conf",
    "Description=strih-lx:",
    "STRIH_BOX_DEFAULT=strih-lx",
];

#[test]
fn no_strih_lx_identity_literal_remains_outside_the_fact_file() {
    // The identity values = strih-lx.env's facts that name THIS box / venue.
    let env = strih_lx_env();
    let fact = |k: &str| -> String {
        env.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!("{k} not in strih-lx.env"))
            .to_string()
    };
    let mut needles: Vec<String> = [
        "STRIH_HOSTNAME",
        "STRIH_IP",
        "STRIH_NDI_PREFIX",
        "STRIH_NDI_RUNTIME_PEER",
        "STRIH_COMPANION_HOST",
        "STRIH_CG_SENDER",
    ]
    .iter()
    .map(|k| fact(k))
    .collect();
    let ic = fact("STRIH_INTERCOM_CONFIG");
    needles.push(ic.rsplit('/').next().unwrap().to_string());
    needles.push("strih.lan".to_string());

    let mut hits = Vec::new();
    for f in [
        "scripts/setup-strih.sh",
        "scripts/verify-strih.sh",
        "scripts/lib/strih-provision.sh",
        "scripts/lib/strih-box-facts.sh",
    ] {
        let text = std::fs::read_to_string(root().join(f)).unwrap();
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let mut l = line.to_string();
            for t in ALLOWED_TOKENS {
                l = l.replace(t, "");
            }
            for n in &needles {
                if l.contains(n.as_str()) {
                    hits.push(format!("{f}:{}: `{n}` in `{}`", i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "strih-lx identity literals must live only in scripts/strih-boxes/strih-lx.env:\n{}",
        hits.join("\n")
    );
}
