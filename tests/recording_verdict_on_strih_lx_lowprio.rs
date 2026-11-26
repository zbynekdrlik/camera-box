//! issue 1351 (item, 22.9.2026) — the in-place strih-snv recording decode (`[8/8a]`,
//! `scripts/recording-verdict-on-strih-snv.sh`) must run at IDLE priority pinned to the Intel
//! hybrid E-cores so it never starves the live OBS receivers (the post-run relock storms of issue
//! 1354: cam1 +763, cam4 +799, cam5 +832, cam7 +782 relocks during the `[7/8]`+`[8/8a]` window,
//! 0 during the recording). A multi-core QR/pixel sweep over a 1080p60 5-min recording at nice 0,
//! unpinned, competes with OBS's `ndir:video` decode threads on the P-cores.
//!
//! These tests pin the pure encoding `strih_lx_lowprio_prefix <sysfs_root>` (fixture-driven) and a
//! static anchor that STEP 2's ssh-run line invokes `$ONSTRIHLX_CMD` through that prefix. STEP 2
//! is the remote-shell replica of the same contract (the E-core range is read ON the box so a
//! replacement notebook resolves its own cores). Implementing the main design (comment 5781015909,
//! Prístup 1).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn onstrihlx_script() -> PathBuf {
    let s = manifest_dir().join("scripts/recording-verdict-on-strih-snv.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn script_text() -> String {
    fs::read_to_string(onstrihlx_script()).expect("read recording-verdict-on-strih-snv.sh")
}

/// Source the launcher and call `strih_lx_lowprio_prefix <root>`. Returns trimmed stdout (the
/// helper uses `printf` with no trailing newline). Mirrors harness_strih_platform_1351.rs's
/// source-and-call shape.
fn lowprio_prefix(root: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; strih_lx_lowprio_prefix \"$2\"")
        .arg("bash")
        .arg(onstrihlx_script())
        .arg(root)
        .output()
        .expect("run strih_lx_lowprio_prefix");
    assert!(
        out.status.success(),
        "strih_lx_lowprio_prefix exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// Build a fake sysfs root; when `cpus` is `Some`, write `<root>/devices/cpu_atom/cpus` with that
/// content (a `None` leaves the file absent — a non-hybrid box).
fn fake_sysfs(cpus: Option<&str>) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    if let Some(c) = cpus {
        let atom = dir.path().join("devices/cpu_atom");
        fs::create_dir_all(&atom).expect("mkdir cpu_atom");
        fs::write(atom.join("cpus"), c).expect("write cpus");
    }
    dir
}

// ================================================================================================
// strih_lx_lowprio_prefix — the pure fallback contract.
// ================================================================================================

#[test]
fn hybrid_box_pins_the_e_cores_at_idle_priority() {
    // strih-snv (measured live 22.9.2026): 16 CPUs, cpu_atom = 12-15 (E), cpu_core = 0-11 (P).
    let root = fake_sysfs(Some("12-15\n"));
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19 taskset -c 12-15",
        "a hybrid box must run the decode at nice 19 pinned to its E-cores"
    );
}

#[test]
fn cpu_atom_file_absent_falls_back_to_nice_only() {
    let root = fake_sysfs(None);
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19",
        "a non-hybrid box (no cpu_atom) just runs deprioritised, no taskset"
    );
}

#[test]
fn empty_cpu_atom_file_never_emits_an_empty_taskset() {
    let root = fake_sysfs(Some(""));
    let got = lowprio_prefix(root.path().to_str().unwrap());
    assert_eq!(
        got, "nice -n 19",
        "an empty cpu_atom/cpus must fall back, never pin to nothing"
    );
    assert!(
        !got.contains("taskset"),
        "must never emit a bare `taskset -c \"\"`: {got:?}"
    );
}

#[test]
fn whitespace_only_cpu_atom_file_falls_back() {
    // A file that is present but holds only whitespace is not a usable range.
    let root = fake_sysfs(Some("   \n\t\n"));
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19",
        "whitespace-only cpu_atom/cpus must fall back to nice-only"
    );
}

#[test]
fn a_trailing_newline_in_the_range_is_stripped() {
    // The sysfs file always ends with a newline; the pin must be the clean range, no newline.
    let root = fake_sysfs(Some("8-15\n"));
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19 taskset -c 8-15",
        "the sysfs trailing newline must not leak into the taskset argument"
    );
}

// ================================================================================================
// STEP 2 static anchor — the on-box decode is launched through the low-priority prefix.
// ================================================================================================

#[test]
fn step2_ssh_run_launches_onstrihlx_cmd_through_the_lowprio_prefix() {
    let s = script_text();

    // The nice base + the E-core taskset arm, resolved on the box (single-quoted for the ssh side).
    assert!(
        s.contains(r#"LP="nice -n 19""#),
        "STEP 2 must set the idle-priority base `nice -n 19`"
    );
    assert!(
        s.contains("[ -s /sys/devices/cpu_atom/cpus ]"),
        "STEP 2 must guard the taskset arm on the Intel-hybrid E-core sysfs file"
    );
    assert!(
        s.contains("taskset -c $(cat /sys/devices/cpu_atom/cpus)"),
        "STEP 2 must pin to the E-cores read ON the box (so a replacement notebook uses its own map)"
    );

    // The on-box verdict command must be launched THROUGH the prefix: `$LP $ONSTRIHLX_CMD`.
    assert!(
        s.contains(r#"\$LP $ONSTRIHLX_CMD"#),
        "the ssh-run line must invoke $ONSTRIHLX_CMD through the low-priority prefix $LP"
    );
}

#[test]
fn the_lowprio_snippet_is_single_quoted_so_dev1_never_expands_the_cat() {
    // The `$(cat …)` and `$LP` inside the snippet must be evaluated by the strih-snv shell, not by
    // dev1 — so the snippet is assigned with SINGLE quotes on the dev1 side.
    let s = script_text();
    assert!(
        s.contains(r#"LOWPRIO_SNIPPET='LP="nice -n 19";"#),
        "the low-priority snippet must be single-quoted on the dev1 side (remote evaluation)"
    );
}

#[test]
fn the_probe_scp_and_partial_pull_are_unchanged() {
    // The change is confined to STEP 2's launch prefix — the binary deploy and the partial/pixels
    // pull-back stay byte-identical (small files, seconds).
    let s = script_text();
    assert!(
        s.contains(r#"sshpass -p "$STRIH_PW" scp "${SSH_OPTS[@]}" "$VERDICT_BIN" "${TARGET}:${REMOTE_BIN}""#),
        "the binary scp must be unchanged"
    );
    assert!(
        s.contains(r#"sshpass -p "$STRIH_PW" scp "${SSH_OPTS[@]}" "${TARGET}:${OUT_PARTIAL}" "$local_partial""#),
        "the partial pull-back must be unchanged"
    );
}

// ================================================================================================
// issue 1425 (finding 7178516822): on a box with housekeeping CPUs the decode runs on the OBS mask
// at idle priority (SCHED_IDLE + nice 19), never in rig-background.slice (= the one housekeeping
// task CPU, which also carries rig-NIC IRQs). A box without housekeeping keeps today's prefix.
// ================================================================================================

// Finding 7187104711 (26.11.2026): the whole OBS mask under SCHED_IDLE still shared every OBS core's SMT
// sibling, L2/L3 and memory bandwidth (the OBS dlink pump woke late for the decode's 8 minutes). So the
// decode takes ONE whole physical core at the TOP of the mask: the highest CPU of the mask plus its SMT
// siblings, read on the box from `<sysfs>/devices/system/cpu/cpu<N>/topology/thread_siblings_list`; a
// siblings value that is not a cpulist falls back to the single highest CPU.
const HK_PREFIX_TOP_15: &str = "nice -n 19 ionice -c 3 chrt --idle 0 taskset -c 15";

/// A fake box: `<root>/devices/cpu_atom/cpus` (sysfs, optional), `<root>/etc/strih-housekeeping-cpus.conf`
/// and `<root>/etc/strih-isolated-cpus.conf` (optional), an executable runner and a `sudo` stub
/// that allows it (`sudo -n -l` exits 0), so the runner arm is live and the housekeeping arm must beat it.
struct FakeBox {
    dir: TempDir,
}

impl FakeBox {
    fn new(atom: Option<&str>, housekeeping: Option<&str>, mask: Option<&str>) -> FakeBox {
        let dir = TempDir::new().expect("tempdir");
        let p = dir.path();
        if let Some(c) = atom {
            fs::create_dir_all(p.join("devices/cpu_atom")).unwrap();
            fs::write(p.join("devices/cpu_atom/cpus"), c).unwrap();
        }
        fs::create_dir_all(p.join("etc")).unwrap();
        if let Some(h) = housekeeping {
            fs::write(p.join("etc/strih-housekeeping-cpus.conf"), h).unwrap();
        }
        if let Some(m) = mask {
            fs::write(p.join("etc/strih-isolated-cpus.conf"), m).unwrap();
        }
        fs::create_dir_all(p.join("bin")).unwrap();
        for (name, body) in [
            ("sudo", "#!/bin/sh\nexit 0\n"),
            ("run", "#!/bin/sh\nexit 0\n"),
        ] {
            let f = p.join("bin").join(name);
            fs::write(&f, body).unwrap();
            let mut perm = fs::metadata(&f).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
            fs::set_permissions(&f, perm).unwrap();
        }
        FakeBox { dir }
    }
    /// Write `<root>/devices/system/cpu/cpu<cpu>/topology/thread_siblings_list` (the box's SMT map).
    fn siblings(self, cpu: u32, list: &str) -> FakeBox {
        let d = self
            .dir
            .path()
            .join(format!("devices/system/cpu/cpu{cpu}/topology"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("thread_siblings_list"), list).unwrap();
        self
    }
    fn root(&self) -> String {
        self.dir.path().to_str().unwrap().to_string()
    }
    fn path_env(&self) -> String {
        format!(
            "{}/bin:{}",
            self.root(),
            std::env::var("PATH").unwrap_or_default()
        )
    }
    /// The pure contract with the runner allowed.
    fn pure(&self) -> String {
        let out = Command::new("bash")
            .arg("-c")
            .arg(". \"$1\"; strih_lx_lowprio_prefix \"$2\" \"$2/bin/run\" \"$2\"")
            .arg("bash")
            .arg(onstrihlx_script())
            .arg(self.root())
            .env("PATH", self.path_env())
            .output()
            .expect("run strih_lx_lowprio_prefix");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }
    /// The REMOTE snippet as shipped, its box paths moved into the fixture, run in bash; prints $LP.
    fn remote(&self) -> String {
        let s = script_text();
        let start = s.find("LOWPRIO_SNIPPET='").expect("snippet") + "LOWPRIO_SNIPPET='".len();
        let end = start + s[start..].find("'\n").expect("snippet end");
        let r = self.root();
        let snippet = s[start..end]
            .replace("/sys/", &format!("{r}/"))
            .replace("/etc/", &format!("{r}/etc/"))
            .replace(
                "/usr/local/sbin/obs-box-background-run",
                &format!("{r}/bin/run"),
            );
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!("{snippet} printf '%s' \"$LP\""))
            .env("PATH", self.path_env())
            .output()
            .expect("run the remote snippet");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

#[test]
fn housekeeping_box_runs_the_decode_on_the_obs_mask_at_idle_priority_1425() {
    // A two-CPU housekeeping box (strih-snv's shape until ROZHODNUTE 7183885100 moved it to 0-2 /
    // mask 3-15): housekeeping 0-1 (task CPU 0 on file), OBS mask 2-15, no cpu_atom (AMD).
    // No topology file in this fixture: the single highest CPU of the mask (finding 7187104711).
    let b = FakeBox::new(None, Some("0\n"), Some("2-15\n"));
    assert_eq!(b.pure(), HK_PREFIX_TOP_15, "pure contract");
    assert_eq!(
        b.remote(),
        HK_PREFIX_TOP_15,
        "the remote snippet must agree with the pure contract"
    );
    assert!(!b.remote().contains("obs-box-background-run") && !b.remote().contains("sudo"));
}

#[test]
fn housekeeping_beats_the_e_cores_too_1425() {
    let b = FakeBox::new(Some("12-15\n"), Some("0-1\n"), Some("2-11\n"));
    let want = "nice -n 19 ionice -c 3 chrt --idle 0 taskset -c 11";
    assert_eq!(b.pure(), want);
    assert_eq!(b.remote(), want);
}

#[test]
fn no_housekeeping_file_keeps_todays_runner_prefix_1425() {
    let b = FakeBox::new(Some("12-15\n"), None, Some("2-11\n"));
    let want = format!("sudo -n {}/bin/run", b.root());
    assert_eq!(
        b.pure(),
        want,
        "a box without housekeeping keeps the rig-background.slice runner"
    );
    assert_eq!(b.remote(), want);
}

#[test]
fn an_empty_or_malformed_obs_mask_keeps_todays_prefix_1425() {
    for mask in [
        None,
        Some(""),
        Some("  \n"),
        Some("2-15; rm -rf /\n"),
        Some("all\n"),
        Some("2-\n"),
        Some("-\n"),
        Some(",,\n"),
        Some("2-15,\n"),
        Some("٢-١٥\n"),
    ] {
        let b = FakeBox::new(None, Some("0\n"), mask);
        let want = format!("sudo -n {}/bin/run", b.root());
        assert_eq!(b.pure(), want, "mask {mask:?}");
        assert_eq!(b.remote(), want, "mask {mask:?}");
    }
}

#[test]
fn an_empty_housekeeping_file_is_no_housekeeping_1425() {
    let b = FakeBox::new(None, Some(""), Some("2-15\n"));
    assert_eq!(b.pure(), format!("sudo -n {}/bin/run", b.root()));
    assert_eq!(b.remote(), format!("sudo -n {}/bin/run", b.root()));
}

// ================================================================================================
// Finding 7187104711: ONE whole physical core at the top of the OBS mask, never the whole mask.
// ================================================================================================

#[test]
fn strih_snv_decodes_on_the_top_core_14_15_of_its_obs_mask_7187104711() {
    // strih-snv: housekeeping 0-3 (task CPU 0 on file), OBS mask 4-15, Zen 5 adjacent SMT pairs.
    let b = FakeBox::new(None, Some("0\n"), Some("4-15\n")).siblings(15, "14-15\n");
    let want = "nice -n 19 ionice -c 3 chrt --idle 0 taskset -c 14-15";
    assert_eq!(b.pure(), want, "pure contract");
    assert_eq!(b.remote(), want, "the shipped snippet");
}

#[test]
fn stream_snv_decodes_on_the_top_p_core_10_11_of_its_obs_mask_7187104711() {
    // stream-snv: i7-12700H, P-core pairs 0-11, E-cores 12-19 (housekeeping with 0-1), mask 2-11.
    let b = FakeBox::new(Some("12-19\n"), Some("0\n"), Some("2-11\n")).siblings(11, "10-11\n");
    let want = "nice -n 19 ionice -c 3 chrt --idle 0 taskset -c 10-11";
    assert_eq!(b.pure(), want);
    assert_eq!(b.remote(), want);
}

#[test]
fn the_top_cpu_is_the_highest_number_of_any_cpulist_shape_7187104711() {
    for (mask, top) in [
        ("4-7,9,12-15\n", 15),
        ("12-15,4-7\n", 15),
        ("1,9,2,10,3,11\n", 11),
        ("7\n", 7),
        ("3-15\n", 15),
    ] {
        let b = FakeBox::new(None, Some("0\n"), Some(mask)).siblings(top, &format!("{top}\n"));
        let want = format!("nice -n 19 ionice -c 3 chrt --idle 0 taskset -c {top}");
        assert_eq!(b.pure(), want, "mask {mask:?}");
        assert_eq!(b.remote(), want, "mask {mask:?}");
    }
}

#[test]
fn comma_form_siblings_are_kept_as_the_kernel_prints_them_7187104711() {
    // A box whose SMT pairs are N,N+8 (strih-pp's shape) prints `7,15`.
    let b = FakeBox::new(None, Some("0,8\n"), Some("1-7,9-15\n")).siblings(15, "7,15\n");
    let want = "nice -n 19 ionice -c 3 chrt --idle 0 taskset -c 7,15";
    assert_eq!(b.pure(), want);
    assert_eq!(b.remote(), want);
}

#[test]
fn unreadable_or_malformed_siblings_fall_back_to_the_single_top_cpu_7187104711() {
    for sib in [
        None,
        Some(""),
        Some("  \n"),
        Some("14-15; rm -rf /\n"),
        Some("all\n"),
        Some("14-\n"),
        Some("\u{0661}\u{0664}-\u{0661}\u{0665}\n"),
    ] {
        let mut b = FakeBox::new(None, Some("0\n"), Some("4-15\n"));
        if let Some(v) = sib {
            b = b.siblings(15, v);
        }
        assert_eq!(b.pure(), HK_PREFIX_TOP_15, "siblings {sib:?}");
        assert_eq!(b.remote(), HK_PREFIX_TOP_15, "siblings {sib:?}");
    }
}

#[test]
fn the_decode_never_takes_the_whole_obs_mask_7187104711() {
    let s = script_text();
    assert!(
        !s.contains("chrt --idle 0 taskset -c $M\""),
        "the housekeeping arm must pin the top core, never the whole mask $M"
    );
    assert!(
        s.contains("/topology/thread_siblings_list"),
        "the snippet reads the SMT siblings on the box"
    );
}
