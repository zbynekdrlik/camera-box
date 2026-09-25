//! issue 1357: the CPU idle wake-up latency bound on every OBS box (the shared baseline).
//!
//! strih-lx's ACPI idle driver offers POLL / C1_ACPI 1 us / C2_ACPI 127 us / C3_ACPI 1048 us, and C3
//! was entered ~106 M times on cpu0 (finding 5839598285): a ~1 ms wake-up lands on the genlock render
//! tick, the audio thread and the NDI receive threads. The baseline item `obs_box_cpu_latency`
//! (`scripts/lib/obs-box-baseline.sh`) installs `systemd/obs-box-cpu-latency.service`, whose holder
//! `scripts/obs-box-cpu-latency-hold.sh` keeps `/dev/cpu_dma_latency` open with the ONE baseline bound
//! (`obs_box_cpu_latency_bound_us`, 150 us) written, so every cpuidle governor skips the deeper states.
//! The shared grader row `baseline:cstate` grades the unit AND the kernel effect (the usage counters of
//! the states deeper than the bound do not advance over a 1 s sample).
//!
//! Tier-0: pure bash (the holder runs against a scratch file, the gather against a fixture sysfs tree).

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const BASELINE: &str = "scripts/lib/obs-box-baseline.sh";
const VERIFY_LIB: &str = "scripts/lib/obs-box-baseline-verify.sh";
const UNIT: &str = "systemd/obs-box-cpu-latency.service";
const HOLDER: &str = "scripts/obs-box-cpu-latency-hold.sh";

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

/// The body of one baseline function (`NAME() {` .. its column-0 `}`).
fn baseline_fn(name: &str) -> String {
    let lib = format!("{}\n", read(BASELINE));
    let head = format!("\n{name}() {{\n");
    let start = lib
        .find(&head)
        .unwrap_or_else(|| panic!("the baseline must define {name}()"));
    let end = start
        + 1
        + lib[start + 1..]
            .find("\n}\n")
            .unwrap_or_else(|| panic!("{name}() must close with a column-0 `}}`"));
    lib[start..end].to_string()
}

fn bound() -> String {
    let (c, out, err) = run(BASELINE, &[], "obs_box_cpu_latency_bound_us");
    assert_eq!(c, 0, "stderr={err}");
    out
}

// ------------------------------------------------------------------------------------------------
// the ONE bound + the unit
// ------------------------------------------------------------------------------------------------

/// 150 us: keeps C1 (1 us) + C2 (127 us) of the ACPI driver, keeps out C3 (1048 us).
#[test]
fn the_baseline_bound_is_150_us() {
    assert_eq!(bound(), "150");
}

/// The unit carries the SAME bound as the lib (the grader reads it back from the unit's Environment=),
/// passes it to the holder, and runs the holder at the path the installer writes.
#[test]
fn the_unit_holds_the_baseline_bound_through_the_installed_holder() {
    let u = read(UNIT);
    assert!(
        u.lines()
            .any(|l| l == format!("Environment=OBS_BOX_CPU_LATENCY_US={}", bound())),
        "the unit must configure the baseline bound:\n{u}"
    );
    assert!(
        u.lines().any(|l| l
            == "ExecStart=/usr/local/sbin/obs-box-cpu-latency-hold.sh ${OBS_BOX_CPU_LATENCY_US}"),
        "the unit must run the installed holder with the configured bound:\n{u}"
    );
    assert!(
        u.lines().any(|l| l == "Restart=on-failure")
            && u.lines().any(|l| l == "WantedBy=multi-user.target"),
        "a system unit held for every boot and restarted when the holder dies:\n{u}"
    );
    assert!(
        u.lines().any(|l| l == "DeviceAllow=/dev/cpu_dma_latency w")
            && !u.contains("PrivateDevices="),
        "the holder needs /dev/cpu_dma_latency (PrivateDevices would hide it):\n{u}"
    );
    let f = baseline_fn("obs_box_cpu_latency");
    assert!(
        f.contains("holder=/usr/local/sbin/obs-box-cpu-latency-hold.sh")
            && f.contains("unit=/etc/systemd/system/obs-box-cpu-latency.service"),
        "the installer writes the holder + unit at the paths the unit uses: {f}"
    );
}

// ------------------------------------------------------------------------------------------------
// the installer
// ------------------------------------------------------------------------------------------------

/// Both repo files come through the caller's FETCH, the unit is enabled for every boot, applied now
/// (restart on a changed text, else start), and the item fails loud unless the holder is active.
#[test]
fn the_installer_fetches_enables_applies_and_fails_loud() {
    let f = baseline_fn("obs_box_cpu_latency");
    let fetch_holder = f
        .find("\"$FETCH\" scripts/obs-box-cpu-latency-hold.sh \"$holder\"")
        .expect("fetch the holder");
    let fetch_unit = f
        .find("\"$FETCH\" systemd/obs-box-cpu-latency.service \"$unit\"")
        .expect("fetch the unit");
    let reload = f.find("systemctl daemon-reload").expect("daemon-reload");
    let enable = f
        .find("systemctl enable obs-box-cpu-latency.service")
        .expect("enable for every boot");
    let restart = f
        .find("systemctl restart obs-box-cpu-latency.service")
        .expect("restart on a changed text");
    let start = f
        .find("systemctl start obs-box-cpu-latency.service")
        .expect("start when unchanged");
    let active = f
        .find("systemctl is-active --quiet obs-box-cpu-latency.service")
        .expect("the active check");
    assert!(
        fetch_holder < reload
            && fetch_unit < reload
            && reload < enable
            && enable < restart
            && restart < start
            && start < active,
        "fetch -> daemon-reload -> enable -> (re)start -> active check: {f}"
    );
    assert!(
        f.contains("[ -c /dev/cpu_dma_latency ]"),
        "fail loud on a kernel without the PM QoS device: {f}"
    );
    // every systemctl step that matters fails loud through the caller's fail()
    for step in [
        "systemctl enable obs-box-cpu-latency.service >/dev/null 2>&1 \\\n        || fail",
        "systemctl restart obs-box-cpu-latency.service \\\n            || fail",
        "systemctl start obs-box-cpu-latency.service \\\n            || fail",
        "systemctl is-active --quiet obs-box-cpu-latency.service \\\n        || fail",
    ] {
        assert!(f.contains(step), "`{step}` must fail loud: {f}");
    }
}

/// The installer refuses a kernel without /dev/cpu_dma_latency -- run for real with the device test
/// pointed at a path that is not a character device (nothing is fetched or enabled then).
#[test]
fn the_installer_fails_before_touching_anything_without_the_pm_qos_device() {
    let f = baseline_fn("obs_box_cpu_latency").replace(
        "[ -c /dev/cpu_dma_latency ]",
        "[ -c /nonexistent/cpu_dma_latency ]",
    );
    // baseline_fn stops before the closing brace
    let body = format!(
        "{f}\n}}\nfetch() {{ echo FETCHED \"$@\"; }}\nsystemctl() {{ echo SYSTEMCTL \"$@\"; }}\nobs_box_cpu_latency fetch"
    );
    let (c, out, err) = run(BASELINE, &[], &body);
    assert_eq!(c, 1, "stdout={out} stderr={err}");
    assert!(
        err.contains("FAIL: issue 1357: /dev/cpu_dma_latency is missing"),
        "{err}"
    );
    assert!(
        !out.contains("FETCHED") && !out.contains("SYSTEMCTL"),
        "nothing installed: {out}"
    );
}

/// Run the REAL installer under the callers' `set -euo pipefail` (setup-imag.sh / setup-strih.sh),
/// with its /etc + /usr/local/sbin paths and the device test moved into `t`, the caller's FETCH
/// copying the repo files, and a `systemctl` stub that logs every call to `t/calls` and answers
/// `is-active` with `ACTIVE_RC` (default 0). `body` runs after the definitions.
fn run_installer(t: &Path, env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let tp = t.to_str().unwrap();
    let f = baseline_fn("obs_box_cpu_latency")
        .replace("/etc/systemd/system", &format!("{tp}/unit"))
        .replace("/usr/local/sbin", &format!("{tp}/sbin"))
        .replace("[ -c /dev/cpu_dma_latency ]", &format!("[ -e {tp}/qos ]"));
    let harness = format!(
        "set -euo pipefail\nfail() {{ echo \"FAIL: $1\" >&2; exit 1; }}\nYELLOW=''; NC=''\n\
         . \"$LIB\"\n{f}\n}}\n\
         fetch() {{ cp \"$REPO/$1\" \"$2\"; }}\n\
         systemctl() {{ echo \"$*\" >> \"$T/calls\"; \
         if [ \"$1\" = is-active ]; then return \"${{ACTIVE_RC:-0}}\"; fi; }}\n\
         mkdir -p \"$T/unit\"; : > \"$T/qos\"\n{body}"
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("LIB", manifest_dir().join(BASELINE))
        .env("REPO", manifest_dir())
        .env("T", t);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run the installer harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn calls(t: &Path) -> Vec<String> {
    std::fs::read_to_string(t.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A box that never had the holder (strih-lx and imag today): the installer runs to the end under
/// `set -euo pipefail`, installs both files byte-identical to the repo (holder executable), enables
/// the unit and RESTARTS it (the text changed), then checks it is active.
#[test]
fn the_installer_completes_a_first_install_under_errexit() {
    let d = tempfile::tempdir().unwrap();
    let (c, out, err) = run_installer(d.path(), &[], "obs_box_cpu_latency fetch\necho DONE");
    assert_eq!(
        c, 0,
        "a first install must not abort: stdout={out} stderr={err}"
    );
    assert!(out.contains("DONE"), "{out}");
    assert_eq!(
        std::fs::read(d.path().join("sbin/obs-box-cpu-latency-hold.sh")).unwrap(),
        std::fs::read(manifest_dir().join(HOLDER)).unwrap()
    );
    assert_eq!(
        std::fs::read(d.path().join("unit/obs-box-cpu-latency.service")).unwrap(),
        std::fs::read(manifest_dir().join(UNIT)).unwrap()
    );
    let mode = std::fs::metadata(d.path().join("sbin/obs-box-cpu-latency-hold.sh"))
        .unwrap()
        .permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o755
    );
    assert_eq!(
        calls(d.path()),
        [
            "daemon-reload",
            "enable obs-box-cpu-latency.service",
            "restart obs-box-cpu-latency.service",
            "is-active --quiet obs-box-cpu-latency.service",
        ]
    );
}

/// A re-run with the SAME installed text only starts the unit (a no-op when it is active) -- the
/// strih-lx genlock deploy re-runs setup-strih every time and must not bounce the holder.
#[test]
fn the_installer_rerun_starts_without_a_restart() {
    let d = tempfile::tempdir().unwrap();
    let (c, out, err) = run_installer(
        d.path(),
        &[],
        "obs_box_cpu_latency fetch\n: > \"$T/calls\"\nobs_box_cpu_latency fetch\necho DONE",
    );
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    assert!(out.contains("DONE"), "{out}");
    let got = calls(d.path());
    assert!(
        got.contains(&"start obs-box-cpu-latency.service".to_string())
            && !got.iter().any(|l| l.starts_with("restart")),
        "{got:?}"
    );
}

/// Only one of the two files present (an interrupted earlier install): still installs + restarts.
#[test]
fn the_installer_completes_when_only_one_file_is_present() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("unit")).unwrap();
    std::fs::copy(
        manifest_dir().join(UNIT),
        d.path().join("unit/obs-box-cpu-latency.service"),
    )
    .unwrap();
    let (c, out, err) = run_installer(d.path(), &[], "obs_box_cpu_latency fetch\necho DONE");
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    assert!(out.contains("DONE"), "{out}");
    assert!(
        calls(d.path()).contains(&"restart obs-box-cpu-latency.service".to_string()),
        "{:?}",
        calls(d.path())
    );
}

/// A holder that is not running after the start fails the installer loud.
#[test]
fn the_installer_fails_loud_when_the_holder_is_not_active() {
    let d = tempfile::tempdir().unwrap();
    let (c, out, err) = run_installer(
        d.path(),
        &[("ACTIVE_RC", "3")],
        "obs_box_cpu_latency fetch\necho DONE",
    );
    assert_eq!(c, 1, "stdout={out} stderr={err}");
    assert!(!out.contains("DONE"), "{out}");
    assert!(
        err.contains("FAIL: issue 1357: obs-box-cpu-latency.service is not active"),
        "{err}"
    );
}

// ------------------------------------------------------------------------------------------------
// the holder
// ------------------------------------------------------------------------------------------------

fn holder(dev: &Path, args: &[&str]) -> (i32, String, String) {
    holder_env(dev, args, &[])
}

/// Run the holder under a 1 s timeout, outside systemd unless `env` sets NOTIFY_SOCKET.
fn holder_env(dev: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new("timeout");
    cmd.arg("1")
        .arg("bash")
        .arg(manifest_dir().join(HOLDER))
        .args(args)
        .env("OBS_BOX_CPU_LATENCY_DEV", dev)
        .env_remove("NOTIFY_SOCKET");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run the holder");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The bound goes in as a 10-byte hex string (a 4-byte write is read by the kernel as a raw s32),
/// leading zeros are decimal, and the holder then stays alive (timeout 124) holding the fd.
#[test]
fn the_holder_writes_a_ten_byte_hex_bound_and_keeps_running() {
    let d = tempfile::tempdir().unwrap();
    let dev = d.path().join("qos");
    std::fs::write(&dev, "").unwrap();
    for (arg, want) in [
        ("150", "0x00000096"),
        ("0150", "0x00000096"),
        ("0", "0x00000000"),
    ] {
        let (c, out, err) = holder(&dev, &[arg]);
        assert_eq!(
            c, 124,
            "the holder must keep running (killed by timeout): {out} {err}"
        );
        let got = std::fs::read_to_string(&dev).unwrap();
        assert_eq!(got, want, "bound {arg}");
        assert_ne!(
            got.len(),
            4,
            "never a 4-byte write (read as binary by the kernel)"
        );
    }
}

/// While it runs, the long-lived process is `sleep` holding the device open on fd 3.
#[test]
fn the_holder_process_keeps_the_device_open() {
    let d = tempfile::tempdir().unwrap();
    let dev = d.path().join("qos");
    std::fs::write(&dev, "").unwrap();
    let mut child = Command::new("bash")
        .arg(manifest_dir().join(HOLDER))
        .arg("150")
        .env("OBS_BOX_CPU_LATENCY_DEV", &dev)
        .env_remove("NOTIFY_SOCKET")
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn the holder");
    let pid = child.id();
    let fd3 = format!("/proc/{pid}/fd/3");
    let mut target = None;
    for _ in 0..50 {
        if let Ok(t) = std::fs::read_link(&fd3) {
            if std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default() == "sleep\n"
            {
                target = Some(t);
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        target.as_deref(),
        Some(dev.as_path()),
        "sleep must hold the device on fd 3"
    );
}

/// A bad bound or an unopenable device fails loud (non-zero, a message) and never holds anything.
#[test]
fn the_holder_rejects_a_bad_bound_or_device() {
    let d = tempfile::tempdir().unwrap();
    let dev = d.path().join("qos");
    std::fs::write(&dev, "").unwrap();
    let cases: [&[&str]; 5] = [&[], &["15a"], &["-1"], &["2000001"], &["99999999"]];
    for args in cases {
        let (c, _out, err) = holder(&dev, args);
        assert_eq!(c, 2, "{args:?}: usage error, got stderr={err}");
        assert!(err.contains("obs-box-cpu-latency:"), "{args:?}: {err}");
        assert_eq!(
            std::fs::read_to_string(&dev).unwrap(),
            "",
            "{args:?}: nothing written"
        );
    }
    let (c, _out, err) = holder(&d.path().join("no/such/dir/qos"), &["150"]);
    assert_eq!(c, 1, "{err}");
    assert!(err.contains("cannot open"), "{err}");
}

/// A `systemd-notify` stand-in on PATH that records its arguments and the device content at the
/// moment it is called, then exits `rc`.
fn notify_stub(dir: &Path, rc: i32) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("systemd-notify");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/bash\nprintf '%s|%s\\n' \"$*\" \"$(cat \"$OBS_BOX_CPU_LATENCY_DEV\")\" >> \"$MARK\"\nexit {rc}\n"
        ),
    )
    .unwrap();
    let mut p = std::fs::metadata(&stub).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
    std::fs::set_permissions(&stub, p).unwrap();
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Under systemd (NOTIFY_SOCKET set) the holder reports READY only AFTER the bound is written, then
/// keeps running; the unit is Type=notify so `systemctl start` waits for exactly that.
#[test]
fn the_holder_notifies_ready_only_after_the_bound_is_written() {
    let d = tempfile::tempdir().unwrap();
    let dev = d.path().join("qos");
    std::fs::write(&dev, "").unwrap();
    let mark = d.path().join("mark");
    let path = notify_stub(d.path(), 0);
    let (c, out, err) = holder_env(
        &dev,
        &["150"],
        &[
            ("NOTIFY_SOCKET", "/run/fake-notify"),
            ("PATH", &path),
            ("MARK", mark.to_str().unwrap()),
        ],
    );
    assert_eq!(c, 124, "keeps running after READY: {out} {err}");
    assert_eq!(
        std::fs::read_to_string(&mark).unwrap(),
        "--ready|0x00000096\n",
        "one READY, sent after the bound was written"
    );
    let u = read(UNIT);
    assert!(
        u.lines().any(|l| l == "Type=notify") && u.lines().any(|l| l == "NotifyAccess=all"),
        "the unit waits for the holder's READY (systemd-notify is a child process):\n{u}"
    );
}

/// A failed READY fails the holder (the start would otherwise hang to its timeout); outside systemd
/// no notification is attempted at all.
#[test]
fn the_holder_fails_loud_on_a_failed_notify_and_skips_it_outside_systemd() {
    let d = tempfile::tempdir().unwrap();
    let dev = d.path().join("qos");
    std::fs::write(&dev, "").unwrap();
    let mark = d.path().join("mark");
    let path = notify_stub(d.path(), 1);
    let (c, _out, err) = holder_env(
        &dev,
        &["150"],
        &[
            ("NOTIFY_SOCKET", "/run/fake-notify"),
            ("PATH", &path),
            ("MARK", mark.to_str().unwrap()),
        ],
    );
    assert_eq!(c, 1, "{err}");
    assert!(err.contains("systemd-notify --ready failed"), "{err}");
    std::fs::remove_file(&mark).unwrap();
    let (c, _out, err) = holder_env(
        &dev,
        &["150"],
        &[("PATH", &path), ("MARK", mark.to_str().unwrap())],
    );
    assert_eq!(c, 124, "{err}");
    assert!(!mark.exists(), "no NOTIFY_SOCKET -> no systemd-notify call");
}

// ------------------------------------------------------------------------------------------------
// the grader row
// ------------------------------------------------------------------------------------------------

/// A fixture cpuidle tree: per cpu, (name, latency, usage) states.
fn sysfs(root: &Path, cpus: usize, states: &[(&str, u32, u64)]) {
    for c in 0..cpus {
        for (i, (name, lat, usage)) in states.iter().enumerate() {
            let s = root.join(format!("cpu{c}/cpuidle/state{i}"));
            std::fs::create_dir_all(&s).unwrap();
            std::fs::write(s.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(s.join("latency"), format!("{lat}\n")).unwrap();
            std::fs::write(s.join("usage"), format!("{usage}\n")).unwrap();
        }
    }
}

const ACPI: &[(&str, u32, u64)] = &[
    ("POLL", 0, 10),
    ("C1_ACPI", 1, 500),
    ("C2_ACPI", 127, 700),
    ("C3_ACPI", 1048, 105_700_000),
];

/// The real gather's `cstate_*` facts over a fixture sysfs root. Only two things are rewritten: the
/// `_cs_root` path and the 1 s sample sleep, which becomes `between` (a shell command run between the
/// two counter reads -- deterministic, never a race against the gather's own timing).
fn gather_cstate(root: &Path, between: &str) -> Vec<String> {
    let body = "snip=\"$(obs_box_baseline_gather_snippet strih nosuchuser strih-obs.service)\"\n\
         old_root='_cs_root=/sys/devices/system/cpu'\n\
         [ \"${snip//\"$old_root\"/}\" != \"$snip\" ] || { echo ROOT-ANCHOR-MISSING; exit 3; }\n\
         snip=\"${snip//\"$old_root\"/\"_cs_root=$ROOT\"}\"\n\
         nl=$'\\n'\n\
         [ \"${snip//\"${nl}sleep 1${nl}\"/}\" != \"$snip\" ] || { echo SLEEP-ANCHOR-MISSING; exit 3; }\n\
         snip=\"${snip//\"${nl}sleep 1${nl}\"/\"${nl}${BETWEEN}${nl}\"}\"\n\
         bash -c \"$snip\" | grep '^cstate_'";
    let (c, out, err) = run(
        VERIFY_LIB,
        &[("ROOT", root.to_str().unwrap()), ("BETWEEN", between)],
        body,
    );
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    out.lines().map(str::to_string).collect()
}

#[test]
fn the_gather_reads_the_deep_states_and_a_quiet_sample() {
    let d = tempfile::tempdir().unwrap();
    sysfs(d.path(), 2, ACPI);
    let f = gather_cstate(d.path(), "true");
    assert!(f.contains(&"cstate_states=8".to_string()), "{f:?}");
    assert!(
        f.contains(&"cstate_deep=C3_ACPI:1048us".to_string()),
        "{f:?}"
    );
    assert!(f.contains(&"cstate_deep_delta=0".to_string()), "{f:?}");
}

/// A deep state entered during the sample (the bound is NOT held) shows as a positive delta; a
/// shallower state advancing is irrelevant.
#[test]
fn the_gather_sees_a_deep_state_entered_during_the_sample() {
    let d = tempfile::tempdir().unwrap();
    sysfs(d.path(), 2, ACPI);
    let c3 = d.path().join("cpu1/cpuidle/state3/usage");
    let c2 = d.path().join("cpu0/cpuidle/state2/usage");
    let between = format!(
        "echo 105700042 > '{}'; echo 999999 > '{}'",
        c3.display(),
        c2.display()
    );
    let f = gather_cstate(d.path(), &between);
    assert!(f.contains(&"cstate_deep_delta=42".to_string()), "{f:?}");
}

/// No cpuidle state readable -> a zero count, and an unreadable deep counter -> no delta: both grade
/// FAIL (the verdict needs at least one state and a numeric zero delta).
#[test]
fn the_gather_reports_unreadable_cpuidle_as_empty() {
    let d = tempfile::tempdir().unwrap();
    let f = gather_cstate(d.path(), "true");
    assert!(f.contains(&"cstate_states=0".to_string()), "{f:?}");
    let d2 = tempfile::tempdir().unwrap();
    sysfs(d2.path(), 1, ACPI);
    std::fs::write(d2.path().join("cpu0/cpuidle/state3/usage"), "n/a\n").unwrap();
    let f = gather_cstate(d2.path(), "true");
    assert!(f.contains(&"cstate_deep_delta=".to_string()), "{f:?}");
}

const GOOD: &str = "cstate_unit=enabled
cstate_active=active
cstate_bound=150
cstate_states=64
cstate_deep=C3_ACPI:1048us
cstate_deep_delta=0
gather_done=1
";

fn cstate_row(facts: &str) -> (String, String) {
    let (_c, out, err) = run(
        VERIFY_LIB,
        &[("FACTS", facts)],
        "obs_box_baseline_verdict <<<\"$FACTS\"",
    );
    assert!(
        err.is_empty(),
        "the verdict must be silent on stderr: {err}"
    );
    let line = out
        .lines()
        .find(|l| l.starts_with("cstate|"))
        .unwrap_or_else(|| panic!("a cstate row: {out}"))
        .to_string();
    let mut it = line.splitn(3, '|');
    it.next();
    (
        it.next().unwrap().to_string(),
        it.next().unwrap().to_string(),
    )
}

#[test]
fn the_cstate_row_is_ok_only_when_the_bound_is_held_and_honoured() {
    let (st, detail) = cstate_row(GOOD);
    assert_eq!(st, "OK", "{detail}");
    assert!(
        detail.contains("bound=150 us") && detail.contains("entries-in-1s=0"),
        "{detail}"
    );
    for (from, to) in [
        ("cstate_unit=enabled", "cstate_unit=disabled"),
        ("cstate_unit=enabled", "cstate_unit="),
        ("cstate_active=active", "cstate_active=failed"),
        ("cstate_bound=150", "cstate_bound=1000"),
        ("cstate_bound=150", "cstate_bound="),
        ("cstate_states=64", "cstate_states="),
        ("cstate_states=64", "cstate_states=0"),
        ("cstate_deep_delta=0", "cstate_deep_delta=17"),
        ("cstate_deep_delta=0", "cstate_deep_delta="),
    ] {
        let (st, detail) = cstate_row(&GOOD.replacen(from, to, 1));
        assert_eq!(st, "FAIL", "`{to}` must FAIL the cstate row: {detail}");
    }
    // a box with no state deeper than the bound meets it trivially
    let (st, detail) = cstate_row(&GOOD.replacen("cstate_deep=C3_ACPI:1048us", "cstate_deep=", 1));
    assert_eq!(st, "OK", "{detail}");
}
