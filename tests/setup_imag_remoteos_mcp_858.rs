//! #858 — `setup-imag.sh` must PROVISION the RemoteOS MCP control-channel agent so a freshly
//! hardware'd imag notebook comes up with a working `linux-imag-nb` MCP surface, instead of the
//! agent surviving only as a hand-install on the one original box.
//!
//! The agent's real home is the SEPARATE `zbynekdrlik/remoteos-mcp` project (documented in the
//! `ops` skill, #555). Since issue 1361 the install is ONE shared lib, `scripts/lib/remoteos-mcp.sh`
//! (setup-strih step 10, this step 23, setup-device STEP 17b): the project's own source + its own
//! constraints.txt pip-installed into a venv at `/opt/remoteos-mcp-venv` — the shape the production
//! strih-lx box runs. The upstream `install-linux.sh` pip-installed into the SYSTEM python
//! (`--break-system-packages`), which the fleet boxes no longer do. The `--auth-key` is a
//! full-shell-RCE bearer token: it comes from the REMOTEOS_MCP_AUTH_KEY env var (like this script's
//! other secrets `CAM_PW`/`GH_TOKEN`), the box's existing config, or is generated on-box — NEVER
//! committed to this repo, and it lands only in 0600 files.
//!
//! Same convention as the other setup-imag guards (`tests/setup_imag_guards.rs`): read the REAL
//! script (and the lib it calls) and assert its REAL contract via `body.contains(...)`. The
//! behaviour of the lib itself is exercised in `tests/fresh_install_gaps_1361.rs`.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const SETUP: &str = "scripts/setup-imag.sh";
const LIB: &str = "scripts/lib/remoteos-mcp.sh";

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn body() -> String {
    read(SETUP)
}

fn on_code_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// The new provisioning step must exist AND `TOTAL_STEPS` must count it, or the step banner would
/// print a wrong `[N/TOTAL]` and a dropped step would go unnoticed.
#[test]
fn setup_imag_provisions_remoteos_mcp_step_858() {
    let body = body();
    assert!(
        body.contains("TOTAL_STEPS=28"),
        "{SETUP}: TOTAL_STEPS must count the remoteos-mcp provisioning step (#858) — now 28 after #764 (imag-obs-watchdog), #779 (touchpad usability), #791 (imag-maxperf), issue 1146 (picom vsync compositor) and issue 1299 (imag :8899 bundle-state-server) added steps 24, 25, 26, 27 and 28"
    );
    assert!(
        body.contains("step 23 \""),
        "{SETUP}: a `step 23` banner must announce the remoteos-mcp control-channel agent provisioning (#858)"
    );
}

/// It installs through the ONE shared lib (issue 1361) — never the upstream system-pip installer
/// and never a bare pip of the agent in this script.
#[test]
fn setup_imag_installs_remoteos_through_the_shared_venv_lib_858() {
    let body = body();
    assert!(
        on_code_line(&body, ". \"$_RG_HERE/lib/remoteos-mcp.sh\""),
        "{SETUP}: must source scripts/lib/remoteos-mcp.sh (issue 1361)"
    );
    assert!(
        on_code_line(&body, "remoteos_mcp_install \"$DESKTOP_USER\" desktop restart"),
        "{SETUP}: step 23 must run the shared install as the desktop user, desktop mode, restart policy"
    );
    assert!(
        !on_code_line(&body, "install-linux.sh"),
        "{SETUP}: the upstream install-linux.sh (system pip, --break-system-packages) is retired (issue 1361)"
    );
    assert!(
        !body.contains("remoteos-mcp.git"),
        "{SETUP}: must NOT inline a bare `pip install git+...remoteos-mcp.git` (#555)"
    );
    let lib = read(LIB);
    assert!(
        lib.contains("zbynekdrlik/remoteos-mcp"),
        "{LIB}: must name the canonical `zbynekdrlik/remoteos-mcp` project as the install source"
    );
}

/// The bearer token must come from an env var / the box / be box-generated, and land only in 0600
/// files — never committed to the repo, never in the unit's ExecStart.
#[test]
fn setup_imag_remoteos_auth_key_is_env_sourced_and_never_committed_858() {
    let lib = read(LIB);
    assert!(
        lib.contains("REMOTEOS_MCP_AUTH_KEY"),
        "{LIB}: the remoteos-mcp auth key must be sourced from the REMOTEOS_MCP_AUTH_KEY env var (#858, security-boundary)"
    );
    assert!(
        lib.contains("config.json"),
        "{LIB}: the key is kept in /etc/remoteos-mcp/config.json (the upstream key store) (#858)"
    );
    assert!(
        lib.contains("600 \"config.json\"") && lib.contains("600 \"EnvironmentFile\""),
        "{LIB}: both key files must be written 0600 — they hold a full-shell bearer token (#858)"
    );
}

/// Provisioning must fail loud if the agent is not actually serving afterwards (script-failure
/// policy) — a silently-dead MCP surface is exactly the #858 gap.
#[test]
fn setup_imag_asserts_remoteos_service_active_after_install_858() {
    let lib = read(LIB);
    assert!(
        lib.contains("systemctl is-active --quiet remoteos-mcp"),
        "{LIB}: the restart policy must require the service active + /health before returning success (#858)"
    );
    let body = body();
    assert!(
        body.contains("|| fail \"#858:"),
        "{SETUP}: a failed install must `fail` step 23 (#858)"
    );
}

/// The key is RESOLVED (env, else the box's existing config / unit) BEFORE any key file is written,
/// so a re-run never replaces the key dev1's `.mcp.json` pins (#858, review 🔵).
#[test]
fn setup_imag_seeds_remoteos_config_before_running_installer_858() {
    let lib = read(LIB);
    let install = lib
        .find("remoteos_mcp_install() {")
        .expect("the install function");
    let resolve = install
        + lib[install..]
            .find("remoteos_mcp_resolve_key \"${REMOTEOS_MCP_AUTH_KEY:-}\"")
            .expect("the key resolution");
    let write = install
        + lib[install..]
            .find("_remoteos_mcp_write \"$(remoteos_mcp_env_file)\"")
            .expect("the EnvironmentFile write");
    assert!(
        resolve < write,
        "{LIB}: the key must be resolved (idx {resolve}) before the key files are written (idx {write}) (#858)"
    );
}
