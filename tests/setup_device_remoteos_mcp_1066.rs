//! #1066 — `setup-device.sh` (the cam1-4 provisioner) must PROVISION the RemoteOS MCP
//! control-channel agent, and `verify-device.sh` must have an acceptance check proving it — cam1-4
//! parity with the imag fix (issue 858, `setup-imag.sh` step 23). A freshly-provisioned / newly
//! hardware'd cam box must come up with a working `linux-cam1..4` MCP surface instead of the agent
//! (`remoteos-mcp.service` on :8092) surviving only as a hand-install on each live box.
//!
//! The agent's real home is the SEPARATE `zbynekdrlik/remoteos-mcp` project (documented in the
//! `ops` skill, #555). Since issue 1361 every box installs it through the ONE shared lib
//! `scripts/lib/remoteos-mcp.sh`: the project's own source + constraints.txt into a venv at
//! `/opt/remoteos-mcp-venv` (the upstream `install-linux.sh` pip-installed into the system python). The `--auth-key` is a full-shell-RCE bearer token, so it is sourced from
//! an env var (like this script's other secrets `CAM_PW`/`GH_TOKEN`) or generated on-box by the
//! installer — NEVER committed to this repo.
//!
//! Same convention as the sibling imag guards (`tests/setup_imag_remoteos_mcp_858.rs`): read the
//! REAL scripts and assert their REAL contract via `body.contains(...)` / index ordering.
//!
//! The one DELIBERATE imag→device adaptation: `setup-device.sh` follows the strict cam-box
//! "enable-only, effect at next reboot" convention (`.claude/rules/provisioning-scripts.md`), so it
//! gates on `systemctl is-enabled` (the durable reboot-survival property) rather than imag's
//! `is-active`. The LIVE :8092 surface is proven post-reboot by the new `verify-device.sh` (ab)
//! acceptance check — where a cam-box install's liveness belongs.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const SETUP: &str = "scripts/setup-device.sh";
const VERIFY: &str = "scripts/verify-device.sh";
const LIB: &str = "scripts/lib/remoteos-mcp.sh";

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn setup() -> String {
    read(SETUP)
}
fn verify() -> String {
    read(VERIFY)
}

fn on_code_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

// ============================================================================
// setup-device.sh — STEP 17b provisions the agent
// ============================================================================

/// A dedicated provisioning step (lettered STEP 17b, mirroring the existing cam2-only STEP 3b
/// idiom) must announce the remoteos-mcp agent install — a dropped step would go unnoticed.
#[test]
fn setup_device_provisions_remoteos_mcp_step_17b_1066() {
    let body = setup();
    assert!(
        body.contains("STEP 17b"),
        "{SETUP}: a lettered STEP 17b must provision the remoteos-mcp control-channel agent (#1066), \
         mirroring the STEP 3b sub-step idiom without renumbering the numbered backbone"
    );
    assert!(
        body.contains("[17b]"),
        "{SETUP}: STEP 17b must print a `[17b]` progress banner (like STEP 3b's `[3b]`) (#1066)"
    );
}

/// Since issue 1361 it installs through the ONE shared lib `scripts/lib/remoteos-mcp.sh` (the
/// project's own source + constraints.txt into the `/opt/remoteos-mcp-venv` venv, the shape the
/// production strih-lx box runs) — never the upstream system-pip `install-linux.sh` and never a bare
/// pip of the agent in this script (the #555 discipline).
#[test]
fn setup_device_uses_canonical_remoteos_installer_not_inline_pip_1066() {
    let body = setup();
    assert!(
        on_code_line(&body, ". \"$HERE/lib/remoteos-mcp.sh\""),
        "{SETUP}: must source scripts/lib/remoteos-mcp.sh (issue 1361)"
    );
    assert!(
        on_code_line(&body, "remoteos_mcp_install root headless enable-only"),
        "{SETUP}: STEP 17b must run the shared install as root, headless, enable-only (the cam-box convention)"
    );
    assert!(
        !on_code_line(&body, "install-linux.sh"),
        "{SETUP}: the upstream install-linux.sh (system pip, the Debian RECORD conflicts) is retired (issue 1361)"
    );
    assert!(
        read(LIB).contains("zbynekdrlik/remoteos-mcp"),
        "{LIB}: must name the canonical `zbynekdrlik/remoteos-mcp` project as the install source (#1066)"
    );
    assert!(
        !body.contains("remoteos-mcp.git"),
        "{SETUP}: must NOT inline a bare `pip install git+...remoteos-mcp.git` (#555)"
    );
}

/// The bearer token must come from an env var / the box / be box-generated, and land only in 0600
/// files — never committed to the repo (security-boundary).
#[test]
fn setup_device_remoteos_auth_key_is_env_sourced_and_never_committed_1066() {
    let lib = read(LIB);
    assert!(
        lib.contains("REMOTEOS_MCP_AUTH_KEY"),
        "{LIB}: the remoteos-mcp auth key must be sourced from the REMOTEOS_MCP_AUTH_KEY env var \
         (same env-secret convention as CAM_PW/GH_TOKEN), never hardcoded (#1066, security-boundary)"
    );
    assert!(
        lib.contains("config.json"),
        "{LIB}: the key is kept in /etc/remoteos-mcp/config.json (the upstream key store) (#1066)"
    );
    assert!(
        lib.contains("600 \"config.json\"") && lib.contains("600 \"EnvironmentFile\""),
        "{LIB}: both key files must be written 0600 — they hold a full-shell bearer token (#1066)"
    );
}

/// The provisioning gate is `systemctl is-enabled` — the cam-box "enable-only, effect at next
/// reboot" convention's durable property (NOT imag's is-active; setup-device.sh never depends on a
/// live-runtime state during provisioning). It must fail loud if the unit is not enabled.
#[test]
fn setup_device_asserts_remoteos_service_enabled_after_install_1066() {
    let lib = read(LIB);
    assert!(
        lib.contains("systemctl is-enabled remoteos-mcp"),
        "{LIB}: after install, must read `systemctl is-enabled remoteos-mcp` to gate on the unit's \
         reboot-survival — a fresh box must come up with the :8092 MCP surface on next reboot (#1066)"
    );
    assert!(
        lib.contains("= \"enabled\" ]"),
        "{LIB}: the enable gate must compare the LITERAL is-enabled state to `enabled` — \
         `is-enabled --quiet`'s exit code passes for a `static` unit that is NOT pulled in at boot (#1066)"
    );
    assert!(
        setup().contains("|| fail \"remoteos-mcp agent install failed"),
        "{SETUP}: a failed install must `fail` STEP 17b (#1066)"
    );
}

/// The key is RESOLVED (env, else the box's existing config / unit) BEFORE any key file is written,
/// so a re-provision keeps the key dev1's `.mcp.json` pins (the same review 🔵 the imag test pins).
#[test]
fn setup_device_seeds_remoteos_config_before_running_installer_1066() {
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
            .find("_remoteos_mcp_write \"$(remoteos_mcp_config_json_path)\"")
            .expect("the config.json write");
    assert!(
        resolve < write,
        "{LIB}: the key must be resolved (idx {resolve}) before config.json is written (idx {write}) (#1066)"
    );
}

/// The canonical installer writes to /usr + /etc, so STEP 17b MUST run in the rw window — BEFORE
/// STEP 18 flips the root filesystem read-only (a write to a ro root fails).
#[test]
fn setup_device_installs_remoteos_before_readonly_root_flip_1066() {
    let body = setup();
    let step = body
        .find("STEP 17b")
        .expect("STEP 17b remoteos-mcp provisioning must exist");
    let ro = body
        .find("STEP 18: Configure read-only")
        .expect("STEP 18 read-only-root flip banner");
    assert!(
        step < ro,
        "{SETUP}: STEP 17b (idx {step}) must run BEFORE STEP 18's ro-root flip (idx {ro}) — the \
         installer writes to /usr and /etc, which must happen while root is still rw (#1066)"
    );
}

// ============================================================================
// verify-device.sh — the (ab) acceptance check proves the live surface
// ============================================================================

/// verify-device.sh must gain an (ab) acceptance check (the next free two-char letter after (aa),
/// the #782 interkom precedent) documented in all THREE places and inserted BEFORE the (q) block.
#[test]
fn verify_device_has_remoteos_mcp_check_ab_1066() {
    let body = verify();
    // The check letter appears (header list + usage block + exec block all use "(ab)").
    let count = body.matches("(ab)").count();
    assert!(
        count >= 3,
        "{VERIFY}: the (ab) remoteos-mcp check must be documented in all three places (header Checks \
         list, usage() Checks block, exec block) — found {count} occurrences of `(ab)` (#1066)"
    );
    assert!(
        body.contains("remoteos-mcp"),
        "{VERIFY}: the (ab) check must verify the remoteos-mcp agent (#1066)"
    );
    assert!(
        body.contains("8092"),
        "{VERIFY}: the (ab) check must prove the :8092 MCP surface is listening (#1066)"
    );
    assert!(
        body.contains("is-enabled remoteos-mcp")
            || body.contains("is-enabled --quiet remoteos-mcp"),
        "{VERIFY}: the (ab) check must assert the unit is enabled (reboot-survival) (#1066)"
    );
    assert!(
        body.contains("is-active remoteos-mcp") || body.contains("is-active --quiet remoteos-mcp"),
        "{VERIFY}: the (ab) check must assert the unit is live/active on the running box (#1066)"
    );
}

/// The (ab) exec block must sit BEFORE the intentionally-last (q) `.bak cruft drift` check
/// (`.claude/rules/provisioning-scripts.md` — (q) must remain the true last check, or its
/// `runs-to-end-of-file` test folds the new block in and trips its never-`fail()` assertion).
#[test]
fn verify_device_remoteos_check_sits_before_q_1066() {
    let body = verify();
    // The LAST occurrence of the (q) exec-block comment is the intentionally-last check.
    let q = body
        .rfind("# (q) .bak cruft drift")
        .expect("(q) .bak cruft drift exec block");
    // The (ab) exec block reads the live unit over ssh_box — anchor on that read.
    let ab_exec = body
        .find("is-active remoteos-mcp")
        .or_else(|| body.find("is-active --quiet remoteos-mcp"))
        .expect("(ab) exec block must read the live remoteos-mcp unit");
    assert!(
        ab_exec < q,
        "{VERIFY}: the (ab) exec block (idx {ab_exec}) must be inserted BEFORE the (q) block (idx {q}) (#1066)"
    );
}
