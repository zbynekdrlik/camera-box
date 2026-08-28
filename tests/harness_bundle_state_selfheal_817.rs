//! #817 — offline guard for `scripts/lib/bundle-state-selfheal.sh`: the E2E `[0/8]`
//! version-integrity gate's SELF-HEAL of a dead strih/stream `:8899` `BundleStateServer`.
//!
//! Before #817, `recording-e2e.sh`'s `fetch_box_state` simply `curl`ed `:8899` and, on failure,
//! printed a MISLEADING note ("the win-* MCP holder must write the drift-guard observed values ...")
//! then handed an absent state file to `version-integrity-gate.sh` → the box read UNKNOWN → exit 11.
//! That misdirected workers to hunt a genlock parity/drift problem (issue-817 comment 2026-08-02,
//! three wasted CI runs). This proves the residual fix:
//!   (a) the honest one-line DOWN message names the fault (`bundle-state-server DOWN on <box>
//!       (nothing on :<port>)`), and never re-uses the retired "win-* MCP holder must write" wording;
//!   (b) a restart-then-refetch that RECOVERS returns 0, populates the state file, and issued the
//!       SAME session-agnostic `schtasks /run /tn "BundleStateServer"` the issue-732 watchdog uses
//!       — NEVER the desktop-session `/it` form;
//!   (c) a still-down box returns non-zero AND prints the honest DOWN message AFTER attempting the
//!       restart;
//!   (d) `recording-e2e.sh` wires the self-heal in and no longer prints the misleading note.
//!
//! Fully offline + deterministic: `sshpass` and `curl` are PATH-shimmed (a temp `bin/` prepended to
//! PATH); the curl shim fails its first N calls (a counter file) then succeeds, so a "recovers after
//! restart" run and an "always down" run are both exact; the sshpass shim records its argv. Retry
//! bounds are shrunk via env so the test is instant. Same shim + tempdir pattern as
//! `harness_bundle_state_watchdog_732.rs` (`.claude/rules/ci-testing-gotchas.md` #836/#975).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/bundle-state-selfheal.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn read(rel: &str) -> String {
    fs::read_to_string(manifest_dir().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

struct Run {
    ok: bool,
    out: String,
    sshpass_argv: String,
    dest: String,
}

/// Source the lib and call `bundle_state_selfheal_fetch strih <dest> 8899 newlevel secret` with a
/// temp `bin/` (sshpass + curl shims) on PATH. `curl_fail_first`: the curl shim FAILS its first
/// `curl_fail_first` invocations, then succeeds (writing a JSON body to its `-o` dest). A value
/// at or above the retry count means "always down". Returns exit-ok, combined stdout+stderr, the
/// recorded sshpass argv, and the dest file contents.
fn run_selfheal(curl_fail_first: i64) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let counter = dir.path().join("curl.count");
    let sshlog = dir.path().join("sshpass.argv");
    let dest = dir.path().join("state.json");
    fs::write(&counter, "0").unwrap();

    // curl shim: increments a counter; fails (exit 22) for the first `curl_fail_first` calls, then
    // writes a JSON body to the `-o <dest>` argument and exits 0. `-o <dest>` is parsed positionally.
    write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
cf='{counter}'; thr={thr}\n\
n=$(cat \"$cf\"); n=$((n+1)); printf '%s' \"$n\" > \"$cf\"\n\
dest=''; prev=''\n\
for a in \"$@\"; do [ \"$prev\" = '-o' ] && dest=\"$a\"; prev=\"$a\"; done\n\
if [ \"$n\" -le \"$thr\" ]; then exit 22; fi\n\
[ -n \"$dest\" ] && printf '{{\"obs_version\":\"32.1.2\"}}' > \"$dest\"\n\
exit 0\n",
            counter = counter.display(),
            thr = curl_fail_first
        ),
    );
    // sshpass shim: record the FULL argv (so a test can assert the exact session-agnostic restart
    // command and the absence of `/it`) and exit 0.
    write_exec(
        &bin.join("sshpass"),
        &format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> '{log}'\nexit 0\n",
            log = sshlog.display()
        ),
    );

    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let snippet = format!(
        ". '{lib}'\nbundle_state_selfheal_fetch strih '{dest}' 8899 newlevel secret\n",
        lib = lib().display(),
        dest = dest.display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&snippet)
        .env("PATH", path_env)
        .env("BUNDLE_STATE_SELFHEAL_TRIES", "4")
        .env("BUNDLE_STATE_SELFHEAL_SLEEP_S", "0")
        .env("BUNDLE_STATE_SELFHEAL_SSH_TIMEOUT", "5")
        .current_dir(manifest_dir())
        .output()
        .unwrap();
    let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    Run {
        ok: out.status.success(),
        out: combined,
        sshpass_argv: fs::read_to_string(&sshlog).unwrap_or_default(),
        dest: fs::read_to_string(&dest).unwrap_or_default(),
    }
}

/// Call `bundle_state_down_message <box> <port>` in a lib-sourcing bash snippet, return its stdout.
fn down_message(box_: &str, port: &str) -> String {
    let snippet = format!(
        ". '{lib}'\nbundle_state_down_message {box_} {port}\n",
        lib = lib().display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&snippet)
        .current_dir(manifest_dir())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

// (a) the honest DOWN message names the fault and retires the misleading wording.
#[test]
fn down_message_names_the_fault_and_retires_misleading_wording_817() {
    let msg = down_message("strih", "8899");
    assert!(
        msg.contains("bundle-state-server DOWN on strih (nothing on :8899)"),
        "#817: the DOWN message must name the fault in one line. got: {msg:?}"
    );
    assert!(
        !msg.contains("win-* MCP holder must write"),
        "#817: the retired misleading wording must NOT appear. got: {msg:?}"
    );
}

// (b) restart-then-refetch that RECOVERS returns 0, populates the state file, and issued the
//     session-agnostic schtasks /run restart.
#[test]
fn selfheal_recovers_when_the_server_comes_back_817() {
    let r = run_selfheal(2); // fail 2 polls, succeed on the 3rd (within TRIES=4)
    assert!(
        r.ok,
        "#817: self-heal must return 0 once :8899 answers. out: {}",
        r.out
    );
    assert!(
        r.dest.contains("obs_version"),
        "#817: the re-fetched state file must be populated. dest: {:?}",
        r.dest
    );
    assert!(
        r.sshpass_argv
            .contains("schtasks /run /tn \"BundleStateServer\"")
            || r.sshpass_argv
                .contains("schtasks /run /tn BundleStateServer"),
        "#817: the self-heal must issue the schtasks /run restart. argv: {:?}",
        r.sshpass_argv
    );
}

// (c) the restart is the session-agnostic /run form the issue-732 watchdog uses — NEVER /it.
#[test]
fn selfheal_uses_session_agnostic_run_never_it_817() {
    let r = run_selfheal(1000); // always down → restart attempted, still fails
    assert!(
        r.sshpass_argv.contains("schtasks /run /tn"),
        "#817: a restart must be attempted before giving up. argv: {:?}",
        r.sshpass_argv
    );
    assert!(
        r.sshpass_argv.contains("BundleStateServer"),
        "#817: the restarted task must be BundleStateServer. argv: {:?}",
        r.sshpass_argv
    );
    assert!(
        !r.sshpass_argv.contains("/it"),
        "#817: NEVER the desktop-session /it form (a dead end on these boxes). argv: {:?}",
        r.sshpass_argv
    );
}

// (d) a still-down box returns non-zero AND prints the honest DOWN message (after attempting restart).
#[test]
fn selfheal_reports_honest_down_and_fails_when_still_down_817() {
    let r = run_selfheal(1000); // always down
    assert!(
        !r.ok,
        "#817: self-heal must FAIL when :8899 never comes back. out: {}",
        r.out
    );
    assert!(
        r.out
            .contains("bundle-state-server DOWN on strih (nothing on :8899)"),
        "#817: it must print the honest one-line fault when it gives up. out: {}",
        r.out
    );
    assert!(
        !r.out.contains("win-* MCP holder must write"),
        "#817: the retired misleading wording must NOT appear. out: {}",
        r.out
    );
}

// recording-e2e.sh's version-integrity gate wires the self-heal in and no longer prints the
// misleading note.
#[test]
fn fetch_budget_covers_the_measured_gather_cost_1218() {
    // 2026-08-28 live refusal pair: BOTH :8899 fetch sites used `--max-time 10` while a HEALTHY
    // strih server answered /bundle-state.json in a measured 11.6-13.6 s (each GET spawns ~5
    // cold `powershell -NoProfile` subprocesses + DLL sha256 hashing -- a structural cost that
    // always sat near the old 10 s cap). Every curl burned its full budget, the self-heal
    // restart then KILLED the healthy-but-slow server, and the version-integrity gate refused
    // two runs in a row with a false "bundle-state-server DOWN". The fetch budget must exceed
    // the measured gather cost with margin (>= 2x worst measured): 30 s at both sites.
    for rel in [
        "scripts/lib/bundle-state-selfheal.sh",
        "scripts/recording-e2e.sh",
    ] {
        let s = read(rel);
        assert!(
            s.contains("--max-time 30"),
            "{rel}: the /bundle-state.json curl budget must be 30 s (measured healthy gather \
             11.6-13.6 s; 10 s misclassified a healthy-slow server as DOWN)"
        );
        assert!(
            !s.contains("--max-time 10"),
            "{rel}: no :8899 fetch may keep the under-budgeted 10 s cap that misread a \
             healthy-slow gather as DOWN"
        );
    }
}

#[test]
fn recording_e2e_wires_selfheal_and_drops_the_misleading_note_817() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("lib/bundle-state-selfheal.sh"),
        "#817: recording-e2e.sh must source the self-heal lib"
    );
    assert!(
        s.contains("bundle_state_selfheal_fetch"),
        "#817: recording-e2e.sh's version-integrity gate must call the self-heal on a fetch failure"
    );
    assert!(
        !s.contains("win-* MCP holder must write the drift-guard observed values"),
        "#817: the pre-#817 misleading version-drift note must be gone"
    );
}
