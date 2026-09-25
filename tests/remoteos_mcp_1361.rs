//! issue 1361 (slice: the fresh-install gaps, design comment 5830231872), G2 -- the ONE shared
//! RemoteOS MCP installer `scripts/lib/remoteos-mcp.sh`, used by setup-strih step 10, setup-imag
//! step 23 and setup-device STEP 17b: the project's GitHub API tarball (the GH token on curl's
//! STDIN, never argv) pip-installed into `/opt/remoteos-mcp-venv`, the live strih-lx unit shape with
//! the key in a 0600 EnvironmentFile, idempotent, graded by verify-strih item 8.
//!
//! G1 lives in `tests/obs_box_baseline_1357.rs`; G3 + G4 in `tests/fresh_install_gaps_1361.rs`.
//!
//! Tier-0: sourced bash + PATH stubs (curl / apt-get / systemctl) + a fake venv, no root, no
//! network, no rig.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("issue 1361: read {}: {e}", p.display()))
}

const REMOTEOS_LIB: &str = "scripts/lib/remoteos-mcp.sh";

/// Source `lib` under the callers' strict mode and run `body` with `env`. (exit, stdout, stderr)
fn run(lib: &str, env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("LIB", root().join(lib));
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

fn write_exec(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn on_code_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

fn whoami() -> String {
    let out = Command::new("id").arg("-un").output().expect("id -un");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().expect("id -u");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ------------------------------------------------------------------------------------------------
// G2 -- the shared remoteos-mcp installer: pure helpers
// ------------------------------------------------------------------------------------------------

#[test]
fn remoteos_lib_is_source_only_and_defines_its_api() {
    let (c, out, err) = run(
        REMOTEOS_LIB,
        &[],
        "for f in remoteos_mcp_install remoteos_mcp_verdict remoteos_mcp_unit_text \
         remoteos_mcp_env_text remoteos_mcp_config_json remoteos_mcp_resolve_key \
         remoteos_mcp_key_ok remoteos_mcp_key_from_config_text remoteos_mcp_key_from_unit_text \
         remoteos_mcp_fetch_source remoteos_mcp_source_url remoteos_mcp_venv_dir; do \
           type -t \"$f\" >/dev/null || echo \"MISSING $f\"; done",
    );
    assert_eq!(c, 0, "stderr={err}");
    assert!(out.is_empty(), "every function must be defined: {out}");
    assert!(err.is_empty(), "sourcing must be silent: {err}");
}

/// The desktop unit is the live strih-lx unit (read 25.9.2026) with ONE change: the key moved out of
/// the world-readable ExecStart into a 0600 EnvironmentFile (remoteos reads REMOTEOS_AUTH_KEY).
#[test]
fn remoteos_desktop_unit_is_the_live_strih_lx_shape_with_the_key_moved_out() {
    let (c, out, err) = run(
        REMOTEOS_LIB,
        &[],
        "remoteos_mcp_unit_text newlevel 1000 desktop",
    );
    assert_eq!(c, 0, "stderr={err}");
    let want = "[Unit]
Description=RemoteOS MCP control agent (camera-box scripts/lib/remoteos-mcp.sh, venv)
After=network.target graphical-session.target
Wants=network.target
[Service]
Type=simple
User=newlevel
Environment=DISPLAY=:0
Environment=XDG_RUNTIME_DIR=/run/user/1000
EnvironmentFile=/etc/remoteos-mcp/remoteos-mcp.env
ExecStart=/opt/remoteos-mcp-venv/bin/python -m remoteos --transport streamable-http --enable-all --host 0.0.0.0 --port 8092
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal
[Install]
WantedBy=multi-user.target
";
    assert_eq!(out, want);
}

#[test]
fn remoteos_headless_unit_runs_as_root_without_a_display() {
    let (c, out, err) = run(REMOTEOS_LIB, &[], "remoteos_mcp_unit_text root 0 headless");
    assert_eq!(c, 0, "stderr={err}");
    assert!(out.contains("User=root\n"), "{out}");
    assert!(out.contains("After=network.target\n"), "{out}");
    assert!(
        !out.contains("DISPLAY") && !out.contains("XDG_RUNTIME_DIR") && !out.contains("graphical"),
        "{out}"
    );
    assert!(
        out.contains("EnvironmentFile=/etc/remoteos-mcp/remoteos-mcp.env\n"),
        "{out}"
    );
    assert!(!out.contains("--auth-key"), "{out}");
}

#[test]
fn remoteos_unit_text_refuses_bad_arguments() {
    for bad in [
        "remoteos_mcp_unit_text newlevel 1000 gui",
        "remoteos_mcp_unit_text '' 1000 desktop",
        "remoteos_mcp_unit_text 'a b' 1000 desktop",
        "remoteos_mcp_unit_text newlevel x1 desktop",
        "remoteos_mcp_unit_text newlevel '' headless",
    ] {
        let (c, out, _e) = run(REMOTEOS_LIB, &[], bad);
        assert_ne!(c, 0, "`{bad}` must refuse");
        assert!(out.is_empty(), "`{bad}` must print no unit: {out}");
    }
}

#[test]
fn remoteos_paths_follow_their_overrides() {
    let (c, out, err) = run(
        REMOTEOS_LIB,
        &[
            ("REMOTEOS_MCP_VENV", "/x/venv"),
            ("REMOTEOS_MCP_CONFIG_DIR", "/x/etc"),
        ],
        "remoteos_mcp_unit_text root 0 headless",
    );
    assert_eq!(c, 0, "stderr={err}");
    assert!(
        out.contains("ExecStart=/x/venv/bin/python -m remoteos "),
        "{out}"
    );
    assert!(
        out.contains("EnvironmentFile=/x/etc/remoteos-mcp.env\n"),
        "{out}"
    );
}

#[test]
fn remoteos_source_is_the_github_api_tarball_of_main_by_default() {
    let (_c, out, _e) = run(REMOTEOS_LIB, &[], "remoteos_mcp_source_url");
    assert_eq!(
        out,
        "https://api.github.com/repos/zbynekdrlik/remoteos-mcp/tarball/main"
    );
    let (_c, out, _e) = run(
        REMOTEOS_LIB,
        &[("REMOTEOS_MCP_REF", "v1.2")],
        "remoteos_mcp_source_url",
    );
    assert!(out.ends_with("/tarball/v1.2"), "{out}");
}

#[test]
fn remoteos_key_files_carry_the_key_in_the_upstream_shapes() {
    let (_c, out, _e) = run(REMOTEOS_LIB, &[], "remoteos_mcp_env_text abcDEF123");
    assert_eq!(out, "REMOTEOS_AUTH_KEY=abcDEF123\n");
    let (_c, out, _e) = run(REMOTEOS_LIB, &[], "remoteos_mcp_config_json abcDEF123");
    assert_eq!(
        out,
        "{\n  \"port\": 8092,\n  \"auth_key\": \"abcDEF123\",\n  \"host\": \"0.0.0.0\"\n}\n"
    );
}

#[test]
fn remoteos_key_is_read_back_from_config_json_and_a_legacy_unit() {
    let (_c, out, _e) = run(
        REMOTEOS_LIB,
        &[(
            "T",
            "{\n  \"port\": 8092,\n  \"auth_key\": \"Key42abc\",\n  \"host\": \"0.0.0.0\"\n}\n",
        )],
        "remoteos_mcp_key_from_config_text \"$T\"",
    );
    assert_eq!(out, "Key42abc");
    let (c, out, _e) = run(
        REMOTEOS_LIB,
        &[("T", "not json")],
        "remoteos_mcp_key_from_config_text \"$T\"",
    );
    assert_eq!((c, out.as_str()), (0, ""));
    let (_c, out, _e) = run(
        REMOTEOS_LIB,
        &[(
            "T",
            "[Service]\nExecStart=/opt/remoteos-mcp-venv/bin/python -m remoteos --transport streamable-http --enable-all --host 0.0.0.0 --port 8092 --auth-key LegacyKey9\nRestart=always\n",
        )],
        "remoteos_mcp_key_from_unit_text \"$T\"",
    );
    assert_eq!(out, "LegacyKey9");
    let (c, out, _e) = run(
        REMOTEOS_LIB,
        &[("T", "[Service]\nExecStart=/x -m remoteos\n")],
        "remoteos_mcp_key_from_unit_text \"$T\"",
    );
    assert_eq!((c, out.as_str()), (0, ""));
}

/// The key order: REMOTEOS_MCP_AUTH_KEY, else the existing config.json, else a legacy unit, else
/// nothing (the caller generates). A bad env key refuses; a bad on-box key is skipped.
#[test]
fn remoteos_key_resolution_order() {
    let cases: [(&str, i32, &str); 6] = [
        (
            "remoteos_mcp_resolve_key EnvKey1 CfgKey2 UnitKey3",
            0,
            "EnvKey1",
        ),
        ("remoteos_mcp_resolve_key '' CfgKey2 UnitKey3", 0, "CfgKey2"),
        ("remoteos_mcp_resolve_key '' '' UnitKey3", 0, "UnitKey3"),
        (
            "remoteos_mcp_resolve_key '' 'bad key!' UnitKey3",
            0,
            "UnitKey3",
        ),
        ("remoteos_mcp_resolve_key '' '' ''", 0, ""),
        ("remoteos_mcp_resolve_key 'bad$key' CfgKey2 ''", 2, ""),
    ];
    for (body, want_rc, want_out) in cases {
        let (c, out, _e) = run(REMOTEOS_LIB, &[], body);
        assert_eq!((c, out.as_str()), (want_rc, want_out), "`{body}`");
    }
}

// ------------------------------------------------------------------------------------------------
// G2 -- the orchestrator, end to end on stubs
// ------------------------------------------------------------------------------------------------

const CURL_STUB: &str = r#"#!/bin/bash
out=""; url=""; hstdin=0
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[$i]}" in
    -o) out="${args[$((i + 1))]}" ;;
    -H) if [ "${args[$((i + 1))]}" = "@-" ]; then hstdin=1; fi ;;
    http*) url="${args[$i]}" ;;
  esac
done
echo "CURL_ARGV: $*" >> "$STUB_LOG"
if [ "$hstdin" = 1 ]; then echo "CURL_STDIN: $(cat)" >> "$STUB_LOG"; fi
case "$url" in
  */tarball/*)
    if [ "${FAKE_CURL_FAIL:-0}" = 1 ]; then exit 22; fi
    cp "$FAKE_TARBALL" "$out"
    ;;
  *.deb)
    if [ "${FAKE_CURL_FAIL:-0}" = 1 ]; then exit 22; fi
    cp "$FAKE_DEB" "$out"
    ;;
esac
exit 0
"#;

const LOG_STUB: &str = r#"#!/bin/bash
echo "$(basename "$0"): $*" >> "$STUB_LOG"
if [ "$(basename "$0")" = systemctl ] && [ "${1:-}" = is-enabled ]; then echo "${FAKE_ENABLED:-enabled}"; fi
exit 0
"#;

/// The fake venv python: `-c 'import remoteos'` succeeds once pip ran (or `.importable` exists).
const VENV_PYTHON_STUB: &str = r#"#!/bin/bash
here="$(cd "$(dirname "$0")/.." && pwd)"
if [ "${1:-}" = -m ] && [ "${2:-}" = pip ]; then
  echo "PIP: ${PIP_CONSTRAINT:-} $*" >> "$STUB_LOG"
  touch "$here/.importable"
  exit 0
fi
if [ "${1:-}" = -c ] && [ "${2:-}" = 'import remoteos' ]; then
  [ -f "$here/.importable" ]
  exit $?
fi
exit 0
"#;

struct Rig {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let bin = base.join("bin");
        fs::create_dir_all(&bin).unwrap();
        write_exec(&bin.join("curl"), CURL_STUB);
        for tool in ["apt-get", "systemctl"] {
            write_exec(&bin.join(tool), LOG_STUB);
        }
        let venv_bin = base.join("venv/bin");
        fs::create_dir_all(&venv_bin).unwrap();
        write_exec(&venv_bin.join("python"), VENV_PYTHON_STUB);
        fs::create_dir_all(base.join("etc")).unwrap();
        fs::create_dir_all(base.join("units")).unwrap();
        fs::create_dir_all(base.join("tmp")).unwrap();
        // The source tarball: one top dir that carries the commit, like the real API tarball.
        let src = base.join("srcbuild/zbynekdrlik-remoteos-mcp-abc1234");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("pyproject.toml"),
            "[project]\nname = \"remoteos-mcp\"\n",
        )
        .unwrap();
        fs::write(src.join("constraints.txt"), "fastmcp==4.0.3\n").unwrap();
        let st = Command::new("tar")
            .arg("-czf")
            .arg(base.join("src.tgz"))
            .arg("-C")
            .arg(base.join("srcbuild"))
            .arg("zbynekdrlik-remoteos-mcp-abc1234")
            .status()
            .unwrap();
        assert!(st.success());
        fs::write(base.join("stub.log"), "").unwrap();
        Rig { _dir: dir, base }
    }

    fn env(&self) -> Vec<(String, String)> {
        let b = |p: &str| self.base.join(p).display().to_string();
        vec![
            (
                "PATH".into(),
                format!("{}:{}", b("bin"), std::env::var("PATH").unwrap()),
            ),
            ("STUB_LOG".into(), b("stub.log")),
            ("FAKE_TARBALL".into(), b("src.tgz")),
            ("REMOTEOS_MCP_VENV".into(), b("venv")),
            ("REMOTEOS_MCP_CONFIG_DIR".into(), b("etc")),
            (
                "REMOTEOS_MCP_UNIT_PATH".into(),
                b("units/remoteos-mcp.service"),
            ),
            ("REMOTEOS_MCP_HEALTH_SLEEP".into(), "0".into()),
            ("TMPDIR".into(), b("tmp")),
        ]
    }

    fn install(&self, extra: &[(&str, &str)], args: &str) -> (i32, String, String) {
        let mut env: Vec<(String, String)> = self.env();
        for (k, v) in extra {
            env.push(((*k).to_string(), (*v).to_string()));
        }
        let envs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        run(REMOTEOS_LIB, &envs, &format!("remoteos_mcp_install {args}"))
    }

    fn log(&self) -> String {
        fs::read_to_string(self.base.join("stub.log")).unwrap()
    }

    fn clear_log(&self) {
        fs::write(self.base.join("stub.log"), "").unwrap();
    }

    fn file(&self, rel: &str) -> String {
        fs::read_to_string(self.base.join(rel)).unwrap_or_default()
    }

    fn mode(&self, rel: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(self.base.join(rel))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }
}

#[test]
fn remoteos_install_first_run_then_an_idempotent_rerun() {
    let rig = Rig::new();
    let me = whoami();
    let args = format!("{me} desktop restart");
    let (c, out, err) = rig.install(&[("REMOTEOS_MCP_AUTH_KEY", "abcDEF123")], &args);
    assert_eq!(c, 0, "stdout={out}\nstderr={err}\nlog={}", rig.log());
    let log = rig.log();
    assert_eq!(
        log.matches("PIP: ").count(),
        1,
        "pip runs once on a fresh box: {log}"
    );
    assert!(
        log.contains("constraints.txt -m pip install --no-cache-dir --upgrade "),
        "pip uses the project's constraints: {log}"
    );
    assert!(log.contains("systemctl: restart remoteos-mcp"), "{log}");
    assert!(log.contains("systemctl: enable remoteos-mcp"), "{log}");
    assert!(
        log.contains("apt-get: install -y xdotool"),
        "desktop mode installs the desktop tools: {log}"
    );
    assert_eq!(
        rig.file("venv/.camera-box-source"),
        "zbynekdrlik-remoteos-mcp-abc1234\n"
    );
    assert_eq!(
        rig.file("etc/remoteos-mcp.env"),
        "REMOTEOS_AUTH_KEY=abcDEF123\n"
    );
    assert_eq!(rig.mode("etc/remoteos-mcp.env"), 0o600);
    assert!(rig
        .file("etc/config.json")
        .contains("\"auth_key\": \"abcDEF123\""));
    assert_eq!(rig.mode("etc/config.json"), 0o600);
    let unit = rig.file("units/remoteos-mcp.service");
    assert!(
        !unit.contains("abcDEF123"),
        "the key never lands in the unit: {unit}"
    );
    assert!(
        unit.contains(&format!("User={me}\n"))
            && unit.contains(&format!("XDG_RUNTIME_DIR=/run/user/{}\n", uid())),
        "{unit}"
    );
    assert_eq!(rig.mode("units/remoteos-mcp.service"), 0o644);

    // A re-run with nothing changed: no pip, no restart, nothing rewritten.
    rig.clear_log();
    let (c, out, err) = rig.install(&[], &args);
    assert_eq!(c, 0, "stdout={out}\nstderr={err}");
    let log = rig.log();
    assert!(
        !log.contains("PIP: "),
        "an unchanged source never re-runs pip: {log}"
    );
    assert!(
        !log.contains("systemctl: restart"),
        "nothing changed, so no restart: {log}"
    );
    assert!(log.contains("systemctl: start remoteos-mcp"), "{log}");
    assert!(
        out.contains("already installed")
            && out.contains("unit unchanged")
            && out.contains("EnvironmentFile unchanged"),
        "{out}"
    );
    assert_eq!(
        rig.file("etc/remoteos-mcp.env"),
        "REMOTEOS_AUTH_KEY=abcDEF123\n",
        "the config.json key is kept"
    );
}

/// A box provisioned by the upstream installer / the strih-lx hand fix has the key only in the
/// unit's ExecStart: the migration must keep THAT key (dev1's .mcp.json holds it).
#[test]
fn remoteos_install_keeps_a_legacy_unit_key() {
    let rig = Rig::new();
    fs::write(
        rig.base.join("units/remoteos-mcp.service"),
        "[Service]\nExecStart=/opt/remoteos-mcp-venv/bin/python -m remoteos --transport streamable-http --enable-all --host 0.0.0.0 --port 8092 --auth-key LegacyKey9\n",
    )
    .unwrap();
    let (c, out, err) = rig.install(&[], &format!("{} headless enable-only", whoami()));
    assert_eq!(c, 0, "stdout={out}\nstderr={err}");
    assert_eq!(
        rig.file("etc/remoteos-mcp.env"),
        "REMOTEOS_AUTH_KEY=LegacyKey9\n"
    );
    assert!(!rig
        .file("units/remoteos-mcp.service")
        .contains("LegacyKey9"));
}

#[test]
fn remoteos_install_generates_a_key_on_a_bare_box() {
    let rig = Rig::new();
    let (c, out, err) = rig.install(&[], &format!("{} headless enable-only", whoami()));
    assert_eq!(c, 0, "stdout={out}\nstderr={err}");
    let env = rig.file("etc/remoteos-mcp.env");
    let key = env
        .trim_end()
        .strip_prefix("REMOTEOS_AUTH_KEY=")
        .unwrap_or_default()
        .to_string();
    assert_eq!(key.len(), 32, "{env}");
    assert!(key.chars().all(|c| c.is_ascii_alphanumeric()), "{env}");
    assert!(out.contains("generated a new auth key"), "{out}");
}

/// The cam-box convention: enable for the next boot, never start the service now.
#[test]
fn remoteos_enable_only_never_starts_the_service() {
    let rig = Rig::new();
    let (c, out, err) = rig.install(
        &[("REMOTEOS_MCP_AUTH_KEY", "abc123")],
        &format!("{} headless enable-only", whoami()),
    );
    assert_eq!(c, 0, "stdout={out}\nstderr={err}");
    let log = rig.log();
    assert!(log.contains("systemctl: enable remoteos-mcp"), "{log}");
    assert!(
        !log.contains("systemctl: start") && !log.contains("systemctl: restart"),
        "{log}"
    );
    assert!(
        !log.contains("xdotool"),
        "a headless box gets no desktop tools: {log}"
    );
}

/// is-enabled is compared LITERALLY: `static` is not started at boot.
#[test]
fn remoteos_install_fails_when_the_unit_does_not_enable() {
    let rig = Rig::new();
    let (c, _out, err) = rig.install(
        &[
            ("REMOTEOS_MCP_AUTH_KEY", "abc123"),
            ("FAKE_ENABLED", "static"),
        ],
        &format!("{} headless enable-only", whoami()),
    );
    assert_ne!(c, 0);
    assert!(err.contains("not enabled"), "{err}");
}

#[test]
fn remoteos_install_refuses_a_bad_env_key_and_bad_arguments() {
    let rig = Rig::new();
    let me = whoami();
    let (c, _o, err) = rig.install(
        &[("REMOTEOS_MCP_AUTH_KEY", "bad key$(id)")],
        &format!("{me} headless enable-only"),
    );
    assert_ne!(c, 0);
    assert!(err.contains("REMOTEOS_MCP_AUTH_KEY"), "{err}");
    assert!(
        rig.file("etc/remoteos-mcp.env").is_empty(),
        "nothing written on a refused key"
    );
    let (c, _o, _e) = rig.install(
        &[("REMOTEOS_MCP_AUTH_KEY", "abc123")],
        &format!("{me} headless now"),
    );
    assert_ne!(c, 0, "an unknown policy refuses");
    let (c, _o, _e) = rig.install(
        &[("REMOTEOS_MCP_AUTH_KEY", "abc123")],
        "no-such-user-1361 headless enable-only",
    );
    assert_ne!(c, 0, "an unknown user refuses");
}

/// GH_TOKEN goes to curl on STDIN (`-H @-`), never on its argv; without it the fetch is anonymous.
#[test]
fn remoteos_fetch_sends_the_token_on_stdin_never_argv() {
    let rig = Rig::new();
    let (c, out, err) = rig.install(
        &[
            ("REMOTEOS_MCP_AUTH_KEY", "abc123"),
            ("GH_TOKEN", "tokSECRET9"),
        ],
        &format!("{} headless enable-only", whoami()),
    );
    assert_eq!(c, 0, "stdout={out}\nstderr={err}");
    let log = rig.log();
    let argv: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("CURL_ARGV:"))
        .collect();
    assert!(!argv.is_empty(), "{log}");
    assert!(
        argv.iter().all(|l| !l.contains("tokSECRET9")),
        "the token must never be an argv: {log}"
    );
    assert!(
        argv.iter().any(|l| l.contains("-H @-")
            && l.contains("api.github.com/repos/zbynekdrlik/remoteos-mcp/tarball/main")),
        "{log}"
    );
    assert!(
        log.contains("CURL_STDIN: Authorization: token tokSECRET9"),
        "{log}"
    );

    let rig = Rig::new();
    let (c, _o, err) = rig.install(
        &[("REMOTEOS_MCP_AUTH_KEY", "abc123")],
        &format!("{} headless enable-only", whoami()),
    );
    assert_eq!(c, 0, "stderr={err}");
    let log = rig.log();
    assert!(
        !log.contains("-H @-") && !log.contains("CURL_STDIN"),
        "no token, no header: {log}"
    );
}

/// A failed fetch never breaks a working box (a re-run on a network hiccup keeps the installed
/// venv and still refreshes the unit/key files); on a box with nothing installed it fails loud.
#[test]
fn remoteos_fetch_failure_keeps_a_working_install_else_fails() {
    let rig = Rig::new();
    fs::write(rig.base.join("venv/.importable"), "").unwrap();
    let (c, out, err) = rig.install(
        &[("REMOTEOS_MCP_AUTH_KEY", "abc123"), ("FAKE_CURL_FAIL", "1")],
        &format!("{} headless enable-only", whoami()),
    );
    assert_eq!(c, 0, "stdout={out}\nstderr={err}");
    assert!(
        err.contains("WARNING") && err.contains("keeping the installed"),
        "{err}"
    );
    assert_eq!(
        rig.file("etc/remoteos-mcp.env"),
        "REMOTEOS_AUTH_KEY=abc123\n"
    );

    let rig = Rig::new();
    let (c, _o, err) = rig.install(
        &[("REMOTEOS_MCP_AUTH_KEY", "abc123"), ("FAKE_CURL_FAIL", "1")],
        &format!("{} headless enable-only", whoami()),
    );
    assert_ne!(c, 0);
    assert!(err.contains("no working install"), "{err}");
}

// ------------------------------------------------------------------------------------------------
// G2 -- the grader
// ------------------------------------------------------------------------------------------------

const GOOD_UNIT: &str = "[Service]
User=newlevel
EnvironmentFile=/etc/remoteos-mcp/remoteos-mcp.env
ExecStart=/opt/remoteos-mcp-venv/bin/python -m remoteos --transport streamable-http --enable-all --host 0.0.0.0 --port 8092
";

#[test]
fn remoteos_verdict_grades_every_fact() {
    let v = |unit: &str, env_stat: &str, import: &str, en: &str, act: &str| {
        run(
            REMOTEOS_LIB,
            &[
                ("U", unit),
                ("S", env_stat),
                ("I", import),
                ("E", en),
                ("A", act),
            ],
            "remoteos_mcp_verdict \"$U\" \"$S\" \"$I\" \"$E\" \"$A\"",
        )
    };
    let (c, out, _e) = v(GOOD_UNIT, "600 root", "1", "enabled", "active");
    assert_eq!(c, 0, "{out}");
    assert!(out.starts_with("ok "), "{out}");
    let legacy = GOOD_UNIT.replace("--port 8092\n", "--port 8092 --auth-key K\n");
    let system_python = GOOD_UNIT.replace("/opt/remoteos-mcp-venv/bin/python", "/usr/bin/python3");
    let no_envfile = GOOD_UNIT.replace("EnvironmentFile=/etc/remoteos-mcp/remoteos-mcp.env\n", "");
    for (unit, st, imp, en, act, why) in [
        ("", "600 root", "1", "enabled", "active", "unit missing"),
        (
            system_python.as_str(),
            "600 root",
            "1",
            "enabled",
            "active",
            "venv",
        ),
        (
            legacy.as_str(),
            "600 root",
            "1",
            "enabled",
            "active",
            "auth key",
        ),
        (
            no_envfile.as_str(),
            "600 root",
            "1",
            "enabled",
            "active",
            "EnvironmentFile",
        ),
        (
            GOOD_UNIT,
            "644 root",
            "1",
            "enabled",
            "active",
            "want 600 root",
        ),
        (GOOD_UNIT, "", "1", "enabled", "active", "missing"),
        (
            GOOD_UNIT,
            "600 root",
            "0",
            "enabled",
            "active",
            "cannot import",
        ),
        (
            GOOD_UNIT,
            "600 root",
            "1",
            "static",
            "active",
            "not enabled",
        ),
        (
            GOOD_UNIT,
            "600 root",
            "1",
            "enabled",
            "failed",
            "not active",
        ),
    ] {
        let (c, out, _e) = v(unit, st, imp, en, act);
        assert_eq!(c, 1, "{why}: {out}");
        assert!(
            out.starts_with("FAIL: ") && out.contains(why),
            "{why}: {out}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// G2 -- wiring: the three setup scripts + verify-strih
// ------------------------------------------------------------------------------------------------

#[test]
fn every_setup_script_installs_remoteos_through_the_shared_lib() {
    for (script, call) in [
        (
            "scripts/setup-strih.sh",
            "remoteos_mcp_install \"$DESKTOP_USER\" desktop restart",
        ),
        (
            "scripts/setup-imag.sh",
            "remoteos_mcp_install \"$DESKTOP_USER\" desktop restart",
        ),
        (
            "scripts/setup-device.sh",
            "remoteos_mcp_install root headless enable-only",
        ),
    ] {
        let s = read(script);
        assert!(
            on_code_line(&s, "lib/remoteos-mcp.sh\""),
            "{script} must source scripts/lib/remoteos-mcp.sh"
        );
        assert!(on_code_line(&s, call), "{script} must call `{call}`");
        assert!(
            !on_code_line(&s, "install-linux.sh"),
            "{script} must not run the upstream system-pip installer"
        );
        assert!(
            !s.contains("raw.githubusercontent.com/zbynekdrlik/remoteos-mcp"),
            "{script}: no raw installer URL"
        );
        assert!(
            !s.contains("remoteos-mcp.git"),
            "{script}: never a bare pip of the agent"
        );
    }
}

#[test]
fn setup_device_installs_remoteos_in_the_rw_window() {
    let s = read("scripts/setup-device.sh");
    let call = s
        .find("remoteos_mcp_install root headless enable-only")
        .expect("STEP 17b call");
    let step = s.find("STEP 17b").expect("STEP 17b");
    let ro = s.find("STEP 18: Configure read-only").expect("STEP 18");
    assert!(
        step < call && call < ro,
        "the install writes /opt + /etc, so it runs before the ro-root flip"
    );
}

#[test]
fn verify_strih_grades_remoteos_through_the_shared_verdict() {
    let v = read("scripts/verify-strih.sh");
    assert!(
        on_code_line(&v, "lib/remoteos-mcp.sh\""),
        "verify-strih must source the shared lib"
    );
    assert!(
        on_code_line(&v, "remoteos_mcp_verdict "),
        "item 8 must grade with remoteos_mcp_verdict"
    );
    let item = v.find("# 8) remoteos-mcp").expect("item 8");
    let next = v[item..]
        .find("\n# 9)")
        .map(|e| item + e)
        .expect("item 9 follows");
    let body = &v[item..next];
    assert!(
        body.contains("bad \""),
        "a box without the venv agent FAILs item 8: {body}"
    );
    assert!(
        !body.contains("note \"remoteos-mcp not active"),
        "item 8 is graded, no longer a NOTE: {body}"
    );
}
