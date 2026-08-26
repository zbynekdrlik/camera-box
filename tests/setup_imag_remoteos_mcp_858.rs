//! #858 — `setup-imag.sh` must PROVISION the RemoteOS MCP control-channel agent so a freshly
//! hardware'd imag notebook comes up with a working `linux-imag-nb` MCP surface, instead of the
//! agent surviving only as a hand-install on the one original box.
//!
//! The agent's real home is the SEPARATE `zbynekdrlik/remoteos-mcp` project (documented in the
//! `ops` skill, #555): camera-box does NOT re-implement or re-pin it — it INVOKES that project's
//! own canonical `install-linux.sh`, matching the standing "use the installer, never a bare pip
//! command" discipline. The `--auth-key` is a full-shell-RCE bearer token, so it is sourced from
//! an env var (like this script's other secrets `CAM_PW`/`GH_TOKEN`) or generated on-box by the
//! installer — NEVER committed to this repo.
//!
//! Same convention as the other setup-imag guards (`tests/setup_imag_guards.rs`): read the REAL
//! script and assert its REAL contract via `body.contains(...)`.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const SETUP: &str = "scripts/setup-imag.sh";

fn body() -> String {
    let p = manifest_dir().join(SETUP);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The new provisioning step must exist AND `TOTAL_STEPS` must count it, or the step banner would
/// print a wrong `[N/TOTAL]` and a dropped step would go unnoticed.
#[test]
fn setup_imag_provisions_remoteos_mcp_step_858() {
    let body = body();
    assert!(
        body.contains("TOTAL_STEPS=27"),
        "{SETUP}: TOTAL_STEPS must count the remoteos-mcp provisioning step (#858) — now 27 after #764 (imag-obs-watchdog), #779 (touchpad usability), #791 (imag-maxperf) and issue 1146 (picom vsync compositor) added steps 24, 25, 26 and 27"
    );
    assert!(
        body.contains("step 23 \""),
        "{SETUP}: a `step 23` banner must announce the remoteos-mcp control-channel agent provisioning (#858)"
    );
}

/// It must delegate to the CANONICAL installer of the separate remoteos-mcp project — never
/// re-implement the install here (the #555 discipline).
#[test]
fn setup_imag_uses_canonical_remoteos_installer_not_inline_pip_858() {
    let body = body();
    assert!(
        body.contains("install-linux.sh"),
        "{SETUP}: must invoke the canonical remoteos-mcp `install-linux.sh` (#555: use the installer)"
    );
    assert!(
        body.contains("zbynekdrlik/remoteos-mcp"),
        "{SETUP}: must name the canonical `zbynekdrlik/remoteos-mcp` project as the install source (#858)"
    );
    assert!(
        !body.contains("remoteos-mcp.git"),
        "{SETUP}: must NOT inline a bare `pip install git+...remoteos-mcp.git` — that belongs in the \
         canonical installer, not here (#555: never a bare pip command; never re-pin the agent in camera-box)"
    );
}

/// The bearer token must come from an env var / be box-generated, and land only in a chmod-600
/// config file — never committed to the repo.
#[test]
fn setup_imag_remoteos_auth_key_is_env_sourced_and_never_committed_858() {
    let body = body();
    assert!(
        body.contains("REMOTEOS_MCP_AUTH_KEY"),
        "{SETUP}: the remoteos-mcp auth key must be sourced from the REMOTEOS_MCP_AUTH_KEY env var \
         (same env-secret convention as CAM_PW/GH_TOKEN), never hardcoded (#858, security-boundary)"
    );
    assert!(
        body.contains("/etc/remoteos-mcp/config.json"),
        "{SETUP}: the auth key must be written to /etc/remoteos-mcp/config.json (the installer's config), never the repo (#858)"
    );
    assert!(
        body.contains("chmod 600"),
        "{SETUP}: the pre-seeded remoteos-mcp config must be chmod 600 — it holds a full-shell bearer token (#858)"
    );
}

/// Provisioning must fail loud if the agent is not actually serving afterwards (script-failure
/// policy) — a silently-dead MCP surface is exactly the #858 gap.
#[test]
fn setup_imag_asserts_remoteos_service_active_after_install_858() {
    let body = body();
    assert!(
        body.contains("systemctl is-active --quiet remoteos-mcp"),
        "{SETUP}: after install, must assert `systemctl is-active --quiet remoteos-mcp` and `fail` if the \
         service is not up — provisioning must not report success with a dead :8092 MCP surface (#858)"
    );
}

/// The config pre-seed MUST come BEFORE the installer runs — `install-linux.sh` only REUSES an
/// existing key, so a reversed order would silently generate a FRESH key and break dev1's pinned
/// `.mcp.json` while the `systemctl is-active` gate still passes (#858, review 🔵).
#[test]
fn setup_imag_seeds_remoteos_config_before_running_installer_858() {
    let body = body();
    let seed = body.find("\"auth_key\"").expect(
        "must pre-seed /etc/remoteos-mcp/config.json with an auth_key field before install",
    );
    let run = body
        .find("bash \"$REMOTEOS_MCP_INSTALLER_TMP\"")
        .expect("must run the fetched canonical installer via bash");
    assert!(
        seed < run,
        "{SETUP}: the config pre-seed (idx {seed}) must precede the installer invocation (idx {run}) \
         so the installer reuses the known key instead of generating a fresh one (#858)"
    );
}
