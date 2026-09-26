//! issue 1357 (scope A) — ONE shared OBS-box appliance baseline for every Linux OBS box.
//!
//! The imag notebook's box-level provisioning (the owner's reference starting position) moved
//! VERBATIM out of `scripts/setup-imag.sh` into `scripts/lib/obs-box-baseline.sh` (+ its kiosk half
//! `scripts/lib/obs-box-kiosk.sh`), and BOTH `setup-imag.sh` and `setup-strih.sh` call it, so strih-lx
//! gets the imag appliance by construction. `scripts/lib/obs-box-baseline-verify.sh` is the ONE grader
//! `verify-imag.sh` and `verify-strih.sh` both run. These tests pin:
//!
//! - the EQUIVALENCE anchors: setup-imag.sh runs every baseline item in the step that used to carry
//!   its body, with the imag box facts, so it writes what it always wrote;
//! - the pure helpers (release series, CPU plan, GPU detect, crash-popup units, kiosk menu + preamble);
//! - the shared grader's verdict over fixtures (every item fails closed) and its gather/verdict
//!   key-set parity;
//! - the design constraint that the baseline never grants rtprio.
//!
//! Tier-0: pure bash sourcing + static reads (the functions that touch /etc need root and a box, so
//! their CONTENT is pinned statically, as the existing setup-imag guards always did).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const BASELINE: &str = "scripts/lib/obs-box-baseline.sh";
const KIOSK: &str = "scripts/lib/obs-box-kiosk.sh";
const VERIFY_LIB: &str = "scripts/lib/obs-box-baseline-verify.sh";
const SETUP_IMAG: &str = "scripts/setup-imag.sh";
const SETUP_STRIH: &str = "scripts/setup-strih.sh";

/// Source `lib` (a repo-relative path) under the callers' `set -uo pipefail` with a caller-style
/// `fail()` and run `body`. Returns (exit, stdout, stderr).
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

/// The body of one baseline function (`NAME() {` .. its column-0 `}` + blank line).
fn baseline_fn(name: &str) -> String {
    let libs = format!("{}\n{}\n", read(BASELINE), read(KIOSK));
    let head = format!("\n{name}() {{\n");
    let start = libs
        .find(&head)
        .unwrap_or_else(|| panic!("the baseline must define {name}()"));
    let end = start
        + 1
        + libs[start + 1..]
            .find("\n}\n\n")
            .unwrap_or_else(|| panic!("{name}() must close with a column-0 `}}` + a blank line"));
    libs[start..end].to_string()
}

/// The text of setup-imag.sh between the `step N "` banner and the next `step` banner.
fn imag_step(n: u32) -> String {
    let s = read(SETUP_IMAG);
    let head = format!("\nstep {n} \"");
    let start = s
        .find(&head)
        .unwrap_or_else(|| panic!("setup-imag.sh must have a step {n} banner"));
    let rest = &s[start + 1..];
    let end = rest[1..]
        .find("\nstep ")
        .map(|e| e + 1)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

// ------------------------------------------------------------------------------------------------
// structure
// ------------------------------------------------------------------------------------------------

/// The libs are source-only: sourcing defines the functions and runs nothing (no stderr noise).
#[test]
fn the_baseline_libs_are_source_only() {
    let (c, out, err) = run(
        VERIFY_LIB,
        &[],
        "for f in obs_box_network_tuning obs_box_max_performance obs_box_never_sleep \
         obs_box_boot_safety_net obs_box_lowlatency_kernel obs_box_cpu_affinity obs_box_nvidia_prime \
         obs_box_dejitter obs_box_crash_popups_off obs_box_kiosk obs_box_power_envelope obs_box_touchpad \
         obs_box_maxperf_persistence obs_box_cpu_latency obs_box_cpu_latency_bound_us \
         obs_box_openbox_menu_xml obs_box_openbox_autostart_preamble \
         obs_box_baseline_gather_snippet obs_box_baseline_verdict \
         obs_box_apt_lock_timeout_conf obs_box_apt_lock_timeout \
         obs_box_apt_update_lock_held obs_box_apt_update; do \
           type -t \"$f\" >/dev/null || echo \"MISSING $f\"; done",
    );
    assert_eq!(c, 0, "stderr={err}");
    assert!(
        out.is_empty(),
        "every baseline function must be defined: {out}"
    );
    assert!(err.is_empty(), "sourcing must be silent: {err}");
}

/// Each lib stays readable (the repo's ~1000-line budget) -- the reason the kiosk half is its own file.
#[test]
fn each_baseline_lib_stays_under_the_line_budget() {
    for lib in [BASELINE, KIOSK, VERIFY_LIB] {
        let n = read(lib).lines().count();
        assert!(
            n < 1000,
            "{lib} has {n} lines -- split it before it grows past ~1000"
        );
    }
}

/// The design constraint (issue comment 5793075833): the render-tick SCHED_FIFO pin assumed a
/// reserved core and its FIFO + affinity leaked to every NDI thread, so the baseline NEVER grants
/// rtprio. (imag's own issue-484 line stays in setup-imag.sh, outside the shared baseline.)
#[test]
fn the_baseline_never_grants_rtprio() {
    for lib in [BASELINE, KIOSK, VERIFY_LIB] {
        let text = read(lib);
        assert!(
            !text.contains("rtprio   20") && !text.contains("limits.d/95-"),
            "{lib} must never write an rtprio limits.d grant"
        );
    }
}

/// setup-imag.sh's missing-tool tests source it with an EMPTY PATH, so every lib must locate its
/// sibling by pure parameter expansion -- a `dirname`/`cd` subshell dies there (review finding).
#[test]
fn the_libs_source_with_an_empty_path() {
    for lib in [VERIFY_LIB, BASELINE, KIOSK, SETUP_IMAG] {
        let out = Command::new("/usr/bin/bash")
            .arg("-c")
            .arg("PATH=; . \"$LIB\" && type -t obs_box_openbox_menu_xml")
            .env("LIB", manifest_dir().join(lib))
            .output()
            .expect("run bash");
        assert!(
            out.status.success(),
            "{lib} must source with PATH empty: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "function\n", "{lib}");
    }
    for lib in [VERIFY_LIB, BASELINE, SETUP_IMAG] {
        let text = read(lib);
        let code: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("obs-box-") && l.contains("dirname"))
            .collect();
        assert!(
            code.is_empty(),
            "{lib}: no dirname in a lib source line: {code:?}"
        );
    }
}

/// The de-jitter's per-user helper is a FILE-level function (a nested definition leaks globally in
/// bash anyway) and it reads the desktop user's uid itself.
#[test]
fn u_systemctl_is_file_level_and_resolves_its_own_uid() {
    assert!(
        !baseline_fn("obs_box_dejitter").contains("u_systemctl() {"),
        "no nested function inside obs_box_dejitter"
    );
    let helper = baseline_fn("u_systemctl");
    assert!(
        helper.contains("uid=\"$(id -u \"$DESKTOP_USER\")\"")
            && helper.contains("DBUS_SESSION_BUS_ADDRESS=\"unix:path=/run/user/${uid}/bus\""),
        "u_systemctl resolves the uid + bus itself:\n{helper}"
    );
    let kiosk = baseline_fn("obs_box_kiosk");
    let local = kiosk
        .find("local GNOME_PURGE_PKGS GNOME_TO_PURGE p svc")
        .expect("the kiosk declares its locals");
    let first_loop = kiosk.find("for svc in ").expect("the service loop");
    assert!(
        local < first_loop,
        "the locals are declared before the first loop that uses them"
    );
}

/// The lowlatency item prints the release it ran on, never the 24.04-era "6.17" kernel line.
#[test]
fn the_lowlatency_item_is_release_neutral() {
    let body = baseline_fn("obs_box_lowlatency_kernel");
    let echo = body
        .lines()
        .find(|l| l.contains("lowlatency-kernel config installed"))
        .expect("the install echo");
    assert!(
        echo.contains("${SERIES}") && !echo.contains("6.17"),
        "release-neutral message: {echo}"
    );
    let lib = read(BASELINE);
    let def = lib.find("\nobs_box_lowlatency_kernel() {").unwrap();
    let doc = &lib[lib[..def].rfind("\n\n").unwrap()..def];
    assert!(
        doc.contains("NVIDIA DKMS module") && doc.contains("NEWER kernel"),
        "the item's doc comment names the newer-series image + DKMS rebuild:\n{doc}"
    );
}

/// The power envelope's thermal step-down wattage is a per-box argument (strih-lx must never fall to
/// imag's 25 W iGPU clamp), defaulting to the caller's env / imag's 25 W.
#[test]
fn the_power_envelope_takes_a_per_box_stepdown() {
    let body = baseline_fn("obs_box_power_envelope");
    assert!(
        body.contains("local IMAG_PL1_STEPDOWN_W=\"${3:-${IMAG_PL1_STEPDOWN_W:-25}}\""),
        "the optional third argument sets the step-down:\n{body}"
    );
    assert!(
        body.contains("Environment=IMAG_PL1_STEPDOWN_W=${IMAG_PL1_STEPDOWN_W:-25}"),
        "the guard unit bakes the step-down in"
    );
}

// ------------------------------------------------------------------------------------------------
// equivalence anchors: setup-imag.sh runs every item, in the step that used to hold its body,
// with the imag box facts (so it writes what it always wrote)
// ------------------------------------------------------------------------------------------------

#[test]
fn setup_imag_runs_every_baseline_item_in_its_original_step() {
    for (step, call) in [
        (2, "obs_box_network_tuning \"$NIC\" imag"),
        (4, "obs_box_max_performance \"$NIC\" imag"),
        (5, "obs_box_never_sleep \"$DESKTOP_USER\" imag"),
        (6, "obs_box_boot_safety_net \"$IMAG_KERNEL_SERIES\" imag"),
        (7, "obs_box_lowlatency_kernel \"$IMAG_KERNEL_SERIES\""),
        (8, "obs_box_cpu_affinity imag"),
        (9, "obs_box_nvidia_prime imag"),
        (14, "obs_box_dejitter \"$DESKTOP_USER\" imag \"$OBS_CFG\""),
        (15, "obs_box_kiosk \"$DESKTOP_USER\" imag"),
        (
            22,
            "obs_box_power_envelope \"${IMAG_PL1_W:-45}\" imag_fetch_repo_file",
        ),
        (25, "obs_box_touchpad imag"),
        (26, "obs_box_maxperf_persistence imag"),
        (26, "obs_box_cpu_latency imag_fetch_repo_file"),
    ] {
        let body = imag_step(step);
        assert!(
            body.contains(call),
            "setup-imag.sh step {step} must run `{call}`:\n{body}"
        );
    }
    assert!(
        read(SETUP_IMAG).contains("TOTAL_STEPS=28"),
        "imag's step count is unchanged by the move"
    );
}

/// setup-imag.sh sources the baseline BEFORE its source guard, and its imag_* helper names (called by
/// verify-imag.sh / recording-e2e.sh / the unit tests) delegate to the shared implementations.
#[test]
fn setup_imag_sources_the_baseline_and_delegates_its_helpers() {
    let s = read(SETUP_IMAG);
    let source = s
        .find(". \"${_OBS_BOX_HERE}/lib/obs-box-baseline.sh\"")
        .expect("setup-imag.sh must source the shared baseline");
    let guard = s
        .find("if [ \"${BASH_SOURCE[0]}\" != \"${0}\" ]; then")
        .expect("setup-imag.sh keeps its source guard");
    assert!(
        source < guard,
        "the baseline must be sourced before the source guard"
    );
    for (imag, shared) in [
        ("imag_cpu_isolation_plan", "obs_box_cpu_isolation_plan"),
        ("imag_has_discrete_nvidia", "obs_box_has_discrete_nvidia"),
        ("imag_same_unit", "obs_box_same_unit"),
    ] {
        assert!(
            s.contains(&format!("{imag}() {{ {shared} \"$@\"; }}")),
            "{imag} must delegate to {shared}"
        );
    }
    let (c, out, err) = run(
        SETUP_IMAG,
        &[],
        "printf '0 0-1\\n1 0-1\\n2 2-3\\n3 2-3\\n4 4-5\\n5 4-5\\n' | imag_cpu_isolation_plan",
    );
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(
        out, "2,3,4,5\n4,5\n0,1\n",
        "the delegated plan answers exactly as before"
    );
}

/// Every box-named file the baseline writes carries the BOX prefix, so imag keeps its historical
/// names and strih-lx gets its own -- never an `imag` literal on strih-lx.
#[test]
fn every_box_named_file_is_derived_from_the_box_argument() {
    let libs = format!("{}\n{}", read(BASELINE), read(KIOSK));
    for templated in [
        "\"/etc/systemd/logind.conf.d/99-${BOX}-no-sleep.conf\"",
        "\"/etc/apt/apt.conf.d/51${BOX}-kernel-lockdown\"",
        "\"/etc/${BOX}-isolated-cpus.conf\"",
        "\"/etc/default/grub.d/98-${BOX}-isolation.cfg\"",
        "\"/usr/local/bin/${BOX}-igpu-maxperf.sh\"",
        "\"/etc/systemd/system/apt-daily-upgrade.timer.d/${BOX}-offhours.conf\"",
        "\"/etc/lightdm/lightdm.conf.d/50-${BOX}-autologin.conf\"",
        "\"/usr/local/sbin/${BOX}-maxperf.sh\"",
        "\"/etc/systemd/system/${BOX}-maxperf.service\"",
        "\"/etc/udev/rules.d/99-${BOX}-maxperf-pm.rules\"",
    ] {
        assert!(
            libs.contains(templated),
            "the baseline must write {templated}"
        );
    }
    for literal in [
        "99-imag-no-sleep.conf",
        "51imag-kernel-lockdown",
        "/etc/imag-isolated-cpus.conf",
        "imag-offhours.conf",
        "50-imag-autologin.conf",
        "imag-maxperf",
        "imag-igpu-maxperf",
        "linux-lowlatency-hwe-24.04",
        "generic-hwe-24.04",
    ] {
        // code only: the moved bodies keep their historical imag comments verbatim
        let code_hit = libs
            .lines()
            .find(|l| !l.trim_start().starts_with('#') && l.contains(literal));
        assert!(
            code_hit.is_none(),
            "the shared baseline must not hard-code the imag value `{literal}`: {code_hit:?}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// pure helpers
// ------------------------------------------------------------------------------------------------

#[test]
fn kernel_series_is_the_os_release_version_id_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    for (id, body) in [
        ("24.04", "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\n"),
        (
            "26.04",
            "NAME=\"Ubuntu\"\nVERSION_ID=\"26.04\"\nVERSION_CODENAME=resolute\n",
        ),
    ] {
        let f = dir.path().join(format!("os-release-{id}"));
        std::fs::write(&f, body).unwrap();
        let (c, out, err) = run(
            BASELINE,
            &[("F", f.to_str().unwrap())],
            "obs_box_kernel_series \"$F\"",
        );
        assert_eq!(
            (c, out.as_str()),
            (0, format!("{id}\n").as_str()),
            "stderr={err}"
        );
    }
    let bad = dir.path().join("os-release-bad");
    std::fs::write(&bad, "NAME=\"Debian\"\nVERSION_ID=\"13\"\n").unwrap();
    for f in [bad.to_str().unwrap(), "/nonexistent/os-release"] {
        let (c, out, err) = run(BASELINE, &[("F", f)], "obs_box_kernel_series \"$F\"");
        assert_eq!(c, 1, "{f} must fail closed");
        assert!(out.is_empty(), "never a guessed series: {out:?}");
        assert!(err.contains("VERSION_ID"), "{err}");
    }
}

#[test]
fn crash_popup_units_cover_apport_whoopsie_and_the_coredump_hook_template() {
    let (c, out, _e) = run(BASELINE, &[], "obs_box_crash_popup_units");
    assert_eq!(c, 0);
    let units: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        units,
        [
            "apport.service",
            "apport-coredump-hook@.service",
            "whoopsie.service"
        ]
    );
    let item = baseline_fn("obs_box_crash_popups_off");
    assert!(
        item.contains("systemctl mask \"$CRASH_UNIT\"")
            && item.contains("|| fail \"could not mask ${CRASH_UNIT}")
            && item.contains("apt-get install -y systemd-coredump"),
        "every unit is masked fail-loud and systemd-coredump stays the core collector"
    );
    assert!(
        baseline_fn("obs_box_dejitter").contains("\nobs_box_crash_popups_off\n"),
        "the crash-popup item rides with the de-jitter item on every box"
    );
}

/// The --user units the verify grades are exactly the ones the de-jitter masks.
#[test]
fn dejitter_user_units_are_the_units_the_dejitter_masks() {
    let (c, out, _e) = run(KIOSK, &[], "obs_box_dejitter_user_units");
    assert_eq!(c, 0);
    let units: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(units.len(), 9, "{units:?}");
    for want in [
        "tracker-miner-fs-3.service",
        "tracker-xdg-portal-3.service",
        "evolution-source-registry.service",
        "evolution-alarm-notify.service",
    ] {
        assert!(
            units.contains(&want),
            "the list must carry {want}: {units:?}"
        );
    }
    let body = baseline_fn("obs_box_dejitter");
    assert!(
        body.contains("u_systemctl mask $(obs_box_dejitter_user_units)"),
        "the de-jitter masks exactly the listed units (one source of truth):\n{body}"
    );
    let literal = body
        .lines()
        .find(|l| !l.trim_start().starts_with('#') && l.contains("evolution-source-registry"));
    assert!(
        literal.is_none(),
        "no second literal copy of the list: {literal:?}"
    );
}

#[test]
fn holds_generic_kernel_needs_a_generic_kernel_package() {
    for (holds, want) in [
        ("linux-generic-hwe-26.04 lowlatency-kernel", true),
        ("linux-image-7.0.0-31-generic", true),
        ("linux-headers-6.17.0-19-generic obs-studio", true),
        (
            "lowlatency-kernel linux-lowlatency-hwe-26.04 obs-studio",
            false,
        ),
        ("", false),
    ] {
        let (c, _o, _e) = run(
            VERIFY_LIB,
            &[("H", holds)],
            "obs_box_holds_generic_kernel \"$H\"",
        );
        assert_eq!(c == 0, want, "holds `{holds}` must be {want}");
    }
}

#[test]
fn crash_popup_member_ok_grades_templates_by_is_enabled_and_units_by_both_states() {
    let hook = "apport-coredump-hook@.service";
    for (unit, en, act, want) in [
        (hook, "masked", "", true),
        (hook, "masked-runtime", "", true),
        (hook, "not-found", "inactive", true),
        (hook, "", "", true),
        (hook, "static", "inactive", false),
        (hook, "enabled", "", false),
        (hook, "disabled", "", false),
        ("apport.service", "masked", "inactive", true),
        ("apport.service", "masked\nmasked", "inactive", true),
        ("apport.service", "disabled", "failed", true),
        ("apport.service", "masked", "active", false),
        ("apport.service", "masked", "", false),
        ("whoopsie.service", "enabled", "inactive", false),
        ("whoopsie.service", "static", "inactive", false),
    ] {
        let (c, _o, _e) = run(
            VERIFY_LIB,
            &[("U", unit), ("EN", en), ("ACT", act)],
            "obs_box_crash_popup_member_ok \"$U\" \"$EN\" \"$ACT\"",
        );
        assert_eq!(
            c == 0,
            want,
            "member_ok({unit}, {en:?}, {act:?}) must be {want}"
        );
    }
}

#[test]
fn crash_reports_count_counts_only_crash_files_and_tolerates_a_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["_usr_bin_obs.1000.crash", "_usr_bin_x.0.crash", "notes.txt"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    std::fs::create_dir(dir.path().join("sub.crash")).unwrap();
    let (c, out, _e) = run(
        VERIFY_LIB,
        &[("D", dir.path().to_str().unwrap())],
        "obs_box_crash_reports_count \"$D\"",
    );
    assert_eq!((c, out.as_str()), (0, "2"));
    let (c, out, _e) = run(
        VERIFY_LIB,
        &[],
        "obs_box_crash_reports_count /nonexistent/crash",
    );
    assert_eq!((c, out.as_str()), (0, "0"));
}

/// The kiosk root menu is ONE printer for every box; all three args are required.
#[test]
fn openbox_menu_printer_is_parameterised_and_fails_without_its_args() {
    let (c, out, err) = run(
        KIOSK,
        &[],
        "obs_box_openbox_menu_xml strih-lx 'systemctl --user start strih-obs.service' /usr/local/bin/strih-obs-stop.sh",
    );
    assert_eq!(c, 0, "stderr={err}");
    for want in [
        "<menu id=\"root-menu\" label=\"strih-lx\">",
        "<command>systemctl --user start strih-obs.service</command>",
        "<item label=\"Zastav OBS (korektne)\">",
        "<command>/usr/local/bin/strih-obs-stop.sh</command>",
        "<command>x-terminal-emulator -e btop</command>",
        "<command>systemctl poweroff</command>",
    ] {
        assert!(out.contains(want), "menu must carry `{want}`:\n{out}");
    }
    for args in ["", "strih-lx", "strih-lx start-cmd", "'' start stop"] {
        let (c, out, _e) = run(KIOSK, &[], &format!("obs_box_openbox_menu_xml {args}"));
        assert_eq!(c, 1, "`{args}` must be refused");
        assert!(out.is_empty(), "no partial menu: {out}");
    }
}

/// The preamble is the kiosk contract the shared verify greps -- and imag's own (test-pinned) step-16
/// autostart heredoc carries both lines VERBATIM, so imag passes the same autostart item.
#[test]
fn openbox_autostart_preamble_is_what_imag_already_writes() {
    let (c, out, _e) = run(KIOSK, &[], "obs_box_openbox_autostart_preamble");
    assert_eq!(c, 0);
    assert_eq!(
        out,
        "xset s off -dpms s noblank 2>/dev/null || true\n\
         rm -rf \"$HOME/.config/obs-studio/.sentinel\"/* 2>/dev/null || true\n"
    );
    let step16 = imag_step(16);
    for line in out.lines() {
        assert!(
            step16.lines().any(|l| l == line),
            "imag's step-16 autostart must carry the preamble line `{line}`"
        );
    }
    assert!(
        step16.contains("systemctl --user start imag-obs.service"),
        "imag's autostart starts its supervised OBS unit (the autostart item's third fact)"
    );
}

/// The kiosk item installs a real Xorg server + xrandr/xset (26.04 no longer ships Xorg with the
/// Wayland-only GNOME desktop) and purges GNOME only AFTER lightdm is the display manager.
#[test]
fn the_kiosk_item_installs_xorg_and_switches_the_dm_before_the_gnome_purge() {
    let k = baseline_fn("obs_box_kiosk");
    let install = k
        .find("apt-get install -y openbox lightdm feh wmctrl btop xserver-xorg x11-xserver-utils")
        .expect("the kiosk must install openbox + lightdm + the Xorg server");
    let switch = k
        .find("ln -sf /lib/systemd/system/lightdm.service /etc/systemd/system/display-manager.service")
        .expect("the DM switch");
    let purge = k
        .find("apt-get purge -y $GNOME_TO_PURGE")
        .expect("the GNOME purge");
    assert!(
        install < switch && switch < purge,
        "install -> DM switch -> purge (the #504 order)"
    );
    assert!(
        k.contains("autologin-session=openbox"),
        "the kiosk autologins into openbox, never a GNOME session"
    );
}

/// issue 1361 (G1): `python3-websocket` rides the shared baseline's package install, so EVERY OBS box
/// has it (strih-obs-start.sh refuses to launch OBS without it; imag_scenes.py imports it too). It was
/// only a hand install on strih-lx; setup-imag.sh's own step-11b install stays (a no-op then).
#[test]
fn the_kiosk_package_install_carries_python3_websocket_1361() {
    let k = baseline_fn("obs_box_kiosk");
    let line = k
        .lines()
        .find(|l| l.contains("apt-get install -y openbox lightdm"))
        .expect("the kiosk package install line");
    assert!(
        line.split_whitespace().any(|w| w == "python3-websocket"),
        "the shared baseline must install python3-websocket: {line}"
    );
}

// ------------------------------------------------------------------------------------------------
// the shared grader
// ------------------------------------------------------------------------------------------------

const GOOD_FACTS: &str = "sysctl_conf=1
rmem_max=134217728
nic_hook=1
cpu_perf_unit=enabled
rc_local_eee=1
rc_local_active=active
governors=performance
maxperf_active=active
maxperf_udev=1
ppd=masked
cstate_unit=enabled
cstate_active=active
cstate_bound=150
cstate_states=64
cstate_deep=C3_ACPI:1048us
cstate_deep_delta=0
sleep_target=masked
logind_nosleep=1
logind_powerkey=1
kernel_lockdown=1
initrd_hook=1
holds=linux-generic-hwe-26.04 linux-image-generic-hwe-26.04 lowlatency-kernel linux-lowlatency-hwe-26.04
cmdline=BOOT_IMAGE=/vmlinuz ro quiet splash preempt=full rcu_nocbs=all
lowlatency_cfg=1
isolated_cpus=2,3,4,5,6,7,8,9,10,11
dgpu=1
prime=nvidia
igpu_unit=
oomd=masked
offhours=1
process_priority=1
user_masked=9/9
crash_unit=apport.service|masked|inactive
crash_unit=apport-coredump-hook@.service|masked|inactive
crash_unit=whoopsie.service|not-found|inactive
coredump=install ok installed
lightdm=install ok installed
openbox=install ok installed
dm=/usr/lib/systemd/system/lightdm.service
dm_lightdm=/usr/lib/systemd/system/lightdm.service
autologin=1
gdm3=deinstall ok config-files
gnome_shell=
pyws=install ok installed
pyws_import=1
brightness_helper=1
brightness_rule=1
brightness_keys=1
brightness_group=1
autostart_exec=1
autostart_xset=1
autostart_sentinel=1
autostart_unit=1
thermald=
pe_unit=enabled
pe_guard=enabled
touchpad=1
gather_done=1
";

const ITEMS: [&str; 16] = [
    "net",
    "perf",
    "cstate",
    "nosleep",
    "boot",
    "kernel",
    "affinity",
    "gpu",
    "dejitter",
    "crash",
    "kiosk",
    "websocket",
    "brightness",
    "autostart",
    "power",
    "touchpad",
];

fn verdict(facts: &str) -> (i32, Vec<(String, String)>) {
    let (c, out, err) = run(
        VERIFY_LIB,
        &[("FACTS", facts)],
        "obs_box_baseline_verdict <<<\"$FACTS\"",
    );
    assert!(
        err.is_empty(),
        "the verdict must be silent on stderr: {err}"
    );
    let rows = out
        .lines()
        .map(|l| {
            let mut it = l.splitn(3, '|');
            (
                it.next().unwrap_or_default().to_string(),
                it.next().unwrap_or_default().to_string(),
            )
        })
        .collect();
    (c, rows)
}

#[test]
fn verdict_passes_a_fully_provisioned_box_with_one_row_per_item() {
    let (c, rows) = verdict(GOOD_FACTS);
    assert_eq!(c, 0, "{rows:?}");
    let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names, ITEMS,
        "one row per baseline item, in provisioning order"
    );
    assert!(rows.iter().all(|(_, st)| st == "OK"), "{rows:?}");
}

/// Every item FAILs when its own fact is broken -- and only that item.
#[test]
fn verdict_fails_each_item_on_its_own_broken_fact() {
    for (item, from, to) in [
        ("net", "rmem_max=134217728", "rmem_max=212992"),
        (
            "perf",
            "governors=performance",
            "governors=performance powersave",
        ),
        ("perf", "maxperf_active=active", "maxperf_active=inactive"),
        ("perf", "rc_local_eee=1", "rc_local_eee=0"),
        ("perf", "ppd=masked", "ppd=enabled"),
        ("cstate", "cstate_active=active", "cstate_active=inactive"),
        ("cstate", "cstate_deep_delta=0", "cstate_deep_delta=5"),
        ("nosleep", "sleep_target=masked", "sleep_target=static"),
        ("boot", "initrd_hook=1", "initrd_hook=0"),
        (
            "boot",
            "holds=linux-generic-hwe-26.04 linux-image-generic-hwe-26.04 ",
            "holds=",
        ),
        ("kernel", " lowlatency-kernel ", " "),
        ("kernel", "preempt=full rcu", "preempt=full_debug rcu"),
        (
            "affinity",
            "isolated_cpus=2,3,4,5,6,7,8,9,10,11",
            "isolated_cpus=",
        ),
        (
            "affinity",
            "preempt=full rcu",
            "preempt=full isolcpus=2-11 rcu",
        ),
        ("gpu", "prime=nvidia", "prime=on-demand"),
        ("dejitter", "oomd=masked", "oomd=enabled"),
        ("dejitter", "process_priority=1", "process_priority=0"),
        ("dejitter", "user_masked=9/9", "user_masked=7/9"),
        ("dejitter", "user_masked=9/9", "user_masked=0/0"),
        (
            "crash",
            "crash_unit=apport-coredump-hook@.service|masked|inactive",
            "crash_unit=apport-coredump-hook@.service|static|inactive",
        ),
        ("crash", "coredump=install ok installed", "coredump="),
        (
            "kiosk",
            "dm=/usr/lib/systemd/system/lightdm.service",
            "dm=/usr/lib/systemd/system/gdm3.service",
        ),
        ("kiosk", "gnome_shell=", "gnome_shell=install ok installed"),
        ("kiosk", "lightdm=install ok installed", "lightdm="),
        (
            "kiosk",
            "openbox=install ok installed",
            "openbox=deinstall ok config-files",
        ),
        ("websocket", "pyws=install ok installed", "pyws="),
        ("websocket", "pyws_import=1", "pyws_import=0"),
        ("brightness", "brightness_helper=1", "brightness_helper=0"),
        ("brightness", "brightness_rule=1", "brightness_rule=0"),
        ("brightness", "brightness_keys=1", "brightness_keys=0"),
        ("brightness", "brightness_group=1", "brightness_group=0"),
        ("autostart", "autostart_unit=1", "autostart_unit=0"),
        ("power", "thermald=", "thermald=install ok installed"),
        ("touchpad", "touchpad=1", "touchpad=0"),
    ] {
        assert!(GOOD_FACTS.contains(from), "fixture must carry `{from}`");
        let (c, rows) = verdict(&GOOD_FACTS.replacen(from, to, 1));
        assert_eq!(c, 1, "{item}: `{to}` must fail the verdict");
        for (name, st) in &rows {
            let want = if name == item { "FAIL" } else { "OK" };
            assert_eq!(st, want, "`{to}`: item {name} must be {want}: {rows:?}");
        }
    }
}

#[test]
fn verdict_grades_an_igpu_only_box_by_its_max_frequency_pin() {
    let facts = GOOD_FACTS
        .replacen("dgpu=1", "dgpu=0", 1)
        .replacen("prime=nvidia", "prime=", 1)
        .replacen("igpu_unit=\n", "igpu_unit=enabled\n", 1);
    let (c, rows) = verdict(&facts);
    assert_eq!(c, 0, "{rows:?}");
    let unpinned = facts.replacen("igpu_unit=enabled\n", "igpu_unit=disabled\n", 1);
    let (c, rows) = verdict(&unpinned);
    assert_eq!(
        c, 1,
        "an iGPU box without its frequency pin fails: {rows:?}"
    );
    for (name, st) in &rows {
        let want = if name == "gpu" { "FAIL" } else { "OK" };
        assert_eq!(st, want, "item {name} must be {want}: {rows:?}");
    }
}

/// A gather that never completed (ssh died, snippet aborted) FAILs every item -- never a pass.
#[test]
fn verdict_fails_every_item_when_the_gather_did_not_complete() {
    for facts in [GOOD_FACTS.replace("gather_done=1\n", ""), String::new()] {
        let (c, rows) = verdict(&facts);
        assert_eq!(c, 1);
        assert_eq!(rows.len(), ITEMS.len());
        assert!(rows.iter().all(|(_, st)| st == "FAIL"), "{rows:?}");
    }
    // fewer crash units than the list (a gather that read only some) is not a pass either
    let short = GOOD_FACTS.replacen("crash_unit=whoopsie.service|not-found|inactive\n", "", 1);
    let (c, rows) = verdict(&short);
    assert_eq!(c, 1);
    assert!(
        rows.iter().any(|(n, st)| n == "crash" && st == "FAIL"),
        "{rows:?}"
    );
}

/// Every key the verdict reads is a key the gather emits (and vice versa) -- the two halves of the
/// grader can never drift apart silently.
#[test]
fn gather_and_verdict_share_one_key_set() {
    let lib = read(VERIFY_LIB);
    let gather_start = lib.find("obs_box_baseline_gather_snippet() {").unwrap();
    let verdict_start = lib.find("obs_box_baseline_verdict() {").unwrap();
    let gather = &lib[gather_start..verdict_start];
    let verdict = &lib[verdict_start..];
    let mut emitted: Vec<String> = gather
        .split("echo \"")
        .skip(1)
        .filter_map(|s| s.split('=').next())
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .map(str::to_string)
        .collect();
    emitted.sort();
    emitted.dedup();
    let mut read_keys: Vec<String> = verdict
        .split("_obs_box_f ")
        .skip(1)
        .filter_map(|s| s.split(')').next())
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .map(str::to_string)
        .collect();
    read_keys.push("crash_unit".into()); // parsed line-wise, not via _obs_box_f
    read_keys.sort();
    read_keys.dedup();
    assert_eq!(emitted, read_keys, "gather keys vs verdict keys");
}

/// The gather runs anywhere (any user, no root): on a box that is NOT an OBS appliance it still
/// completes and emits every key, and the verdict then FAILs -- a read problem is never a pass.
#[test]
fn gather_snippet_runs_unprivileged_and_always_completes() {
    let (c, out, err) = run(
        VERIFY_LIB,
        &[],
        "bash -c \"$(obs_box_baseline_gather_snippet nosuchbox nosuchuser nosuch.service)\"",
    );
    assert_eq!(c, 0, "stderr={err}");
    assert!(
        out.ends_with("gather_done=1\n"),
        "the gather must run to its last line:\n{out}"
    );
    assert_eq!(
        out.lines().filter(|l| l.starts_with("crash_unit=")).count(),
        3,
        "one crash_unit row per obs_box_crash_popup_units member"
    );
    let (c, rows) = verdict(&out);
    assert_eq!(
        c, 1,
        "a non-appliance host must fail the baseline: {rows:?}"
    );
    let (c, _o, _e) = run(VERIFY_LIB, &[], "obs_box_baseline_gather_snippet imag");
    assert_ne!(c, 0, "BOX USER UNIT are all required");
}

/// Both acceptance gates run the SAME grader with their own box prefix + OBS unit.
#[test]
fn both_verify_scripts_run_the_shared_grader() {
    let imag = read("scripts/verify-imag.sh");
    assert!(
        imag.contains("obs_box_baseline_gather_cmd imag \"$IMAG_USER\" imag-obs.service")
            && imag.contains("obs_box_baseline_verdict <<<\"${BASELINE_FACTS:-}\""),
        "verify-imag.sh must gather over ssh + grade with the shared verdict"
    );
    let bb = imag
        .find("# (bb) the shared OBS-box appliance baseline")
        .unwrap();
    let restart = imag
        .find("ssh_box_timeout \"$IMAG_OBS_RESTART_TIMEOUT\"")
        .unwrap();
    assert!(
        bb < restart,
        "check (bb) is read-only and runs before check (o)'s OBS restart"
    );
    let strih = read("scripts/verify-strih.sh");
    assert!(
        strih.contains("obs_box_baseline_gather_snippet strih \"${STRIH_LX_USER:-newlevel}\" strih-obs.service"),
        "verify-strih.sh must grade the strih box"
    );
}

/// setup-strih.sh runs the SAME items (the strih side is pinned in strih_provision_pure_functions.rs);
/// here: nothing the kiosk supersedes survives there.
#[test]
fn setup_strih_consumes_the_same_baseline() {
    let s = read(SETUP_STRIH);
    for call in [
        "obs_box_network_tuning",
        "obs_box_max_performance",
        "obs_box_never_sleep",
        "obs_box_boot_safety_net",
        "obs_box_lowlatency_kernel",
        "obs_box_cpu_affinity",
        "obs_box_nvidia_prime",
        "obs_box_dejitter",
        "obs_box_kiosk",
        "obs_box_power_envelope",
        "obs_box_touchpad",
        "obs_box_maxperf_persistence",
        "obs_box_cpu_latency",
        "obs_box_openbox_menu_xml",
    ] {
        assert!(
            s.contains(&format!("{call} ")),
            "setup-strih.sh must run {call}"
        );
    }
    let start = read("scripts/strih-obs-start.sh");
    assert!(
        !start.contains("WAYLAND_DISPLAY=%s") && !start.contains("__NV_PRIME_RENDER_OFFLOAD"),
        "strih-obs-start.sh runs on the Xorg kiosk: no Wayland path, no PRIME-offload env"
    );
}

/// Live 23.9.2026 on strih-lx (26.04): step 6 had just held the kernel at 7.0.0-31, and the unpinned
/// `apt-get install linux-lowlatency-hwe-26.04` picked the NEWER meta 7.0.0-34, which depends on
/// `linux-image-generic-hwe-26.04 (= 7.0.0-34)` — apt refused ("Reached two conflicting assignments")
/// and the conversion failed. The meta is config-only by design (never a new image): install it at
/// the SAME version as the installed generic-hwe kernel meta.
#[test]
fn lowlatency_meta_is_pinned_to_the_installed_generic_hwe_version() {
    let f = baseline_fn("obs_box_lowlatency_kernel");
    assert!(
        f.contains("dpkg-query -W -f='${Version}' \"linux-image-generic-hwe-${SERIES}\""),
        "must read the installed generic-hwe version: {f}"
    );
    assert!(
        f.contains("\"linux-lowlatency-hwe-${SERIES}=${_ll_ver}\""),
        "must install the lowlatency meta pinned to that version: {f}"
    );
}

/// power-profiles-daemon (0.30 on 26.04) resets every core's scaling_governor to `powersave` when it
/// starts, so a box whose governor the baseline pinned to `performance` came back `powersave` after
/// the strih-lx conversion reboot (23.9.2026). The max-performance item masks the daemon BEFORE it
/// checks the governor; the maxperf boot script writes platform_profile itself and only asks the
/// daemon when it actually runs.
#[test]
fn max_performance_masks_power_profiles_daemon_before_the_governor_check() {
    let f = baseline_fn("obs_box_max_performance");
    let mask = f
        .find("systemctl mask power-profiles-daemon.service")
        .expect("the max-performance item must mask power-profiles-daemon");
    let check = f
        .find("grep -q performance /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .expect("the governor check");
    assert!(mask < check, "mask the daemon before the governor check");
    assert!(
        f.contains("systemctl disable --now power-profiles-daemon.service"),
        "stop the running daemon too, not only the next boot"
    );
}

#[test]
fn maxperf_script_only_calls_powerprofilesctl_when_the_daemon_runs() {
    let f = baseline_fn("obs_box_maxperf_persistence");
    assert!(
        f.contains("systemctl is-active --quiet power-profiles-daemon.service && { powerprofilesctl set performance"),
        "powerprofilesctl only when the daemon is active (it is masked by the baseline): {f}"
    );
}

/// An absent daemon (not installed) is as good as masked; an enabled one fails the perf row.
#[test]
fn verdict_accepts_an_absent_power_profiles_daemon() {
    let (c, rows) = verdict(&GOOD_FACTS.replacen("ppd=masked", "ppd=", 1));
    assert_eq!(c, 0, "{rows:?}");
}

/// The kiosk disables bluetooth on a box with no Bluetooth input (imag). strih-lx is operated with a
/// Bluetooth mouse (MX Anywhere 2S), so it passes `keep-bluetooth` and the service stays enabled --
/// a box fact, the same disable list otherwise.
#[test]
fn kiosk_keeps_bluetooth_only_when_the_box_asks() {
    let k = baseline_fn("obs_box_kiosk");
    assert!(
        k.contains("[ \"$svc\" = bluetooth ] && [ \"$KEEP_BT\" = keep-bluetooth ] && continue"),
        "the disable loop must skip bluetooth for a keep-bluetooth box: {k}"
    );
    assert!(
        k.contains("KEEP_BT=\"${3:-}\""),
        "keep-bluetooth is the optional third argument"
    );
    let strih = read(SETUP_STRIH);
    assert!(
        strih.contains("obs_box_kiosk \"$DESKTOP_USER\" strih keep-bluetooth"),
        "setup-strih keeps bluetooth (the operator mouse)"
    );
    let imag = read(SETUP_IMAG);
    assert!(
        imag.contains("obs_box_kiosk \"$DESKTOP_USER\" imag\n"),
        "setup-imag keeps the plain call (bluetooth disabled)"
    );
}

// ------------------------------------------------------------------------------------------------
// The dpkg lock wait (issue 1357 dpkg-lock slice). 24.9.2026: the canonical strih-lx genlock deploy
// failed twice at setup-strih step 4 with `E: Could not get lock /var/lib/dpkg/lock-frontend ...
// held by process (apt-get)` -- a periodic apt run held the lock and apt-get's default
// DPkg::Lock::Timeout is 0. The baseline writes ONE apt drop-in so every present and future apt-get
// waits up to 10 min; a real apt failure still fails loud after the wait (every call site keeps its
// `|| fail`). The live box's only lock setting is `Version::2.0::Dpkg::Lock::Timeout`, which binds the
// interactive `apt` front end, never apt-get.
// ------------------------------------------------------------------------------------------------

const APT_LOCK_CONF_TEXT: &str = "DPkg::Lock::Timeout \"600\";\n";

/// The rendered drop-in is exactly the 600 s dpkg lock wait (apt's own config mechanism).
#[test]
fn apt_lock_timeout_conf_is_the_600s_dpkg_lock_wait() {
    let (c, out, err) = run(BASELINE, &[], "obs_box_apt_lock_timeout_conf");
    assert_eq!(c, 0, "stderr={err}");
    assert_eq!(out, APT_LOCK_CONF_TEXT);
    assert!(
        baseline_fn("obs_box_apt_lock_timeout")
            .contains("/etc/apt/apt.conf.d/90camera-box-lock-timeout"),
        "the default target is /etc/apt/apt.conf.d/90camera-box-lock-timeout"
    );
}

/// The write is idempotent: absent -> written, same -> left alone, different -> rewritten; every
/// outcome is logged, and the file always ends up as the rendered text.
#[test]
fn apt_lock_timeout_write_is_idempotent_and_logged() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("90camera-box-lock-timeout");
    let fs = f.to_str().unwrap();
    let (c, out, err) = run(
        BASELINE,
        &[("F", fs)],
        "obs_box_apt_lock_timeout \"$F\" || exit 3\n\
         echo '--'\n\
         obs_box_apt_lock_timeout \"$F\" || exit 4\n\
         echo '--'\n\
         printf 'DPkg::Lock::Timeout \"0\";\\n' > \"$F\"\n\
         obs_box_apt_lock_timeout \"$F\" || exit 5",
    );
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    let logs: Vec<&str> = out.split("--\n").collect();
    assert_eq!(logs.len(), 3, "three runs, three logs: {out}");
    assert!(
        logs[0].contains(&format!("{fs} written")),
        "absent -> written: {out}"
    );
    assert!(
        logs[1].contains(&format!("{fs} already in place")),
        "same -> unchanged: {out}"
    );
    assert!(
        !logs[1].contains("written"),
        "an unchanged file is never rewritten: {out}"
    );
    assert!(
        logs[2].contains(&format!("{fs} written")),
        "different -> rewritten: {out}"
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), APT_LOCK_CONF_TEXT);
}

/// Under a strict root umask the drop-in must still be world-readable (apt run by a non-root user
/// errors on an unreadable conf file), and no temp file is ever left in apt.conf.d (a `.tmp` sibling
/// makes apt print an "invalid filename extension" notice on every run).
#[test]
fn apt_lock_timeout_write_is_0644_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("90camera-box-lock-timeout");
    let fs = f.to_str().unwrap();
    let (c, out, err) = run(
        BASELINE,
        &[("F", fs)],
        "umask 077\nobs_box_apt_lock_timeout \"$F\" || exit 3\nstat -c %a \"$F\"",
    );
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    assert!(out.ends_with("644\n"), "the drop-in must be 0644: {out}");
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["90camera-box-lock-timeout".to_string()],
        "only the drop-in itself may remain"
    );
}

/// A write that cannot land fails loud (the caller's fail()), never a silent skip.
#[test]
fn apt_lock_timeout_write_failure_fails_loud() {
    let (c, out, err) = run(
        BASELINE,
        &[],
        "obs_box_apt_lock_timeout /nonexistent-dir-1357/90camera-box-lock-timeout; echo survived",
    );
    assert_eq!(c, 1, "an unwritable target must exit via fail(): {err}");
    assert!(err.contains("FAIL:"), "{err}");
    assert!(!out.contains("survived"), "{out}");
}

/// The executed provisioning flow of a setup script: everything after its source-guard.
fn executed_flow(rel: &str) -> String {
    let s = read(rel);
    let guard = "if [ \"${BASH_SOURCE[0]}\" != \"${0}\" ]; then";
    let at = s
        .find(guard)
        .unwrap_or_else(|| panic!("{rel} must have the source-guard"));
    s[at..].to_string()
}

/// Byte offset of the first non-comment line of `text` matching `pred`.
fn first_code_line(text: &str, pred: impl Fn(&str) -> bool) -> Option<usize> {
    let mut off = 0;
    for line in text.split_inclusive('\n') {
        if !line.trim_start().starts_with('#') && pred(line) {
            return Some(off);
        }
        off += line.len();
    }
    None
}

/// The lock wait is the FIRST action of every OBS-box provisioning run: both setup scripts call
/// the writer (a plain statement, once), after the root check and before their first apt-get and
/// step 1.
#[test]
fn apt_lock_timeout_is_written_before_the_first_apt_get() {
    for script in [SETUP_STRIH, SETUP_IMAG] {
        let flow = executed_flow(script);
        let is_call = |l: &str| l.trim() == "obs_box_apt_lock_timeout";
        let calls = flow
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && is_call(l))
            .count();
        assert_eq!(
            calls, 1,
            "{script} must call obs_box_apt_lock_timeout exactly once"
        );
        let call = first_code_line(&flow, is_call).unwrap();
        let apt = first_code_line(&flow, |l| l.contains("apt-get"))
            .unwrap_or_else(|| panic!("{script} runs apt-get"));
        let step1 = flow
            .find("\nstep 1 \"")
            .unwrap_or_else(|| panic!("{script} has a step 1 banner"));
        let root = flow
            .find("fail \"run as root (sudo)\"")
            .unwrap_or_else(|| panic!("{script} has the root check"));
        assert!(
            call < apt,
            "{script}: the lock wait must be written before the first apt-get"
        );
        assert!(call < step1, "{script}: the lock wait precedes step 1");
        assert!(root < call, "{script}: written only once running as root");
    }
}

// ------------------------------------------------------------------------------------------------
// The package-LISTS lock wait (issue 1357, main ruling 5821855428). DPkg::Lock::Timeout does not
// govern `apt-get update`, which takes /var/lib/apt/lists/lock and fails at once when apt-daily's list
// refresh holds it. `obs_box_apt_update` is the ONE update call on both provisioning paths: it retries
// ONLY while that lock is HELD (bounded, 600 s total by default, a log line per wait) and fails loud
// on anything else or on timeout. The held-lock text below was captured live from apt 2.8.3 (dev1)
// and apt 3.2.0 (strih-lx).
// ------------------------------------------------------------------------------------------------

const LISTS_LOCK_HELD: &str = "E: Could not get lock /var/lib/apt/lists/lock. It is held by process 1794555 (apt-get)\nE: Unable to lock directory /var/lib/apt/lists/";

/// The retry decision: ONLY a held package-lists lock is waited out.
#[test]
fn apt_update_retries_only_on_a_held_lists_lock() {
    let cases: &[(&str, bool)] = &[
        (LISTS_LOCK_HELD, true),
        // the permission failure is NOT a held lock (a different message: "Could not open lock file")
        (
            "E: Could not open lock file /var/lib/apt/lists/lock - open (13: Permission denied)\nE: Unable to lock directory /var/lib/apt/lists/",
            false,
        ),
        // the dpkg lock is the drop-in's job, never this retry
        (
            "E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 5 (apt-get)",
            false,
        ),
        (
            "E: Failed to fetch http://archive.ubuntu.com/ubuntu/dists/noble/InRelease  404  Not Found",
            false,
        ),
        ("W: Some index files failed to download.", false),
        ("", false),
    ];
    for (text, want) in cases {
        let (c, _out, err) = run(
            BASELINE,
            &[("TEXT", text)],
            "obs_box_apt_update_lock_held \"$TEXT\"",
        );
        assert_eq!(
            c == 0,
            *want,
            "lock_held({text:?}) must be {want} (exit {c}, stderr={err})"
        );
    }
}

/// A fake `apt-get` on PATH: logs its argv to `$CALLS`, prints `LISTS_LOCK_HELD` + exits 100 for the
/// first `locked_calls` invocations, then either succeeds or fails with `final_err`.
fn fake_apt_get(dir: &std::path::Path, locked_calls: u32, final_err: Option<&str>) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let last = match final_err {
        Some(e) => format!("printf '%s\\n' '{e}' >&2\nexit 100"),
        None => "exit 0".to_string(),
    };
    let script = format!(
        "#!/bin/bash\nset -euo pipefail\necho \"$*\" >> \"$CALLS\"\nn=$(wc -l < \"$CALLS\")\n\
         if [ \"$n\" -le {locked_calls} ]; then printf '%s\\n' '{LISTS_LOCK_HELD}' >&2; exit 100; fi\n\
         {last}\n"
    );
    let p = bin.join("apt-get");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

/// A held lists lock is waited out (one log line per wait), then the update succeeds.
#[test]
fn apt_update_waits_out_a_held_lists_lock_then_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_apt_get(dir.path(), 2, None);
    let calls = dir.path().join("calls");
    let (c, out, err) = run(
        BASELINE,
        &[
            ("PATH", &path),
            ("CALLS", calls.to_str().unwrap()),
            ("OBS_BOX_APT_UPDATE_POLL_S", "0"),
        ],
        "obs_box_apt_update",
    );
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    let log = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec!["update -qq"; 3],
        "two locked attempts then one success: {log}"
    );
    let waits = format!("{out}{err}")
        .lines()
        .filter(|l| l.contains("apt update: package lists locked"))
        .count();
    assert_eq!(waits, 2, "one loud wait line per held attempt: {out}{err}");
}

/// Any error that is not a held lists lock fails loud at once -- one attempt, the apt error shown.
#[test]
fn apt_update_fails_loud_at_once_on_any_other_error() {
    let dir = tempfile::tempdir().unwrap();
    let fetch = "E: Failed to fetch http://archive.ubuntu.com/ubuntu/dists/noble/InRelease 404";
    let path = fake_apt_get(dir.path(), 0, Some(fetch));
    let calls = dir.path().join("calls");
    let (c, out, err) = run(
        BASELINE,
        &[
            ("PATH", &path),
            ("CALLS", calls.to_str().unwrap()),
            ("OBS_BOX_APT_UPDATE_POLL_S", "0"),
        ],
        "obs_box_apt_update; echo survived",
    );
    assert_eq!(c, 1, "must exit via fail(): stdout={out} stderr={err}");
    assert!(err.contains("FAIL:"), "{err}");
    assert!(err.contains(fetch), "the apt error must be shown: {err}");
    assert!(!out.contains("survived"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap().lines().count(),
        1,
        "no retry on a non-lock error"
    );
}

/// A lists lock that outlives the budget fails loud (bounded wait, never an endless loop); the
/// production budget is 600 s.
#[test]
fn apt_update_fails_loud_when_the_lock_outlives_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_apt_get(dir.path(), 1000, None);
    let calls = dir.path().join("calls");
    let (c, out, err) = run(
        BASELINE,
        &[
            ("PATH", &path),
            ("CALLS", calls.to_str().unwrap()),
            ("OBS_BOX_APT_UPDATE_POLL_S", "0"),
            ("OBS_BOX_APT_UPDATE_BUDGET_S", "0"),
        ],
        "obs_box_apt_update; echo survived",
    );
    assert_eq!(c, 1, "must exit via fail(): stdout={out} stderr={err}");
    assert!(err.contains("FAIL:"), "{err}");
    assert!(
        err.contains("/var/lib/apt/lists/lock"),
        "the timeout names the lock: {err}"
    );
    assert!(!out.contains("survived"), "{out}");
    assert!(
        baseline_fn("obs_box_apt_update").contains("${OBS_BOX_APT_UPDATE_BUDGET_S:-600}"),
        "the production wait budget is 600 s"
    );
}

/// The call-site sweep: no bare `apt-get update` (or `apt update`) is left on the setup-strih /
/// setup-imag paths -- the scripts and every lib they source. The ONE real update is inside
/// `obs_box_apt_update`, and `add-apt-repository` never refreshes the lists itself (`-n`).
#[test]
fn no_bare_apt_get_update_on_the_obs_box_provisioning_paths() {
    let wrapper = baseline_fn("obs_box_apt_update");
    for rel in [
        SETUP_STRIH,
        SETUP_IMAG,
        BASELINE,
        KIOSK,
        "scripts/lib/strih-box-facts.sh",
        "scripts/lib/strih-provision.sh",
        "scripts/lib/strih-drm-output.sh",
        "scripts/lib/genlock-markers.sh",
        "scripts/lib/ndi-discovery.sh",
        "scripts/lib/ndi-runtime.sh",
        "scripts/lib/imag-power-envelope.sh",
        "scripts/lib/rig-grandmaster.sh",
        // issue 1361: the shared remoteos-mcp install + the Downstream Keyer plugin libs
        "scripts/lib/remoteos-mcp.sh",
        "scripts/lib/obs-downstream-keyer.sh",
        // sourced one level down (strih-box-facts.sh / ndi-discovery.sh)
        "scripts/lib/obs-fleet.sh",
        "scripts/camera-set.sh",
    ] {
        let mut text = read(rel);
        if rel == BASELINE {
            text = text.replacen(&wrapper, "", 1);
        }
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            assert!(
                !runs_apt_update(line),
                "{rel}:{}: a bare apt update -- use obs_box_apt_update: {line}",
                i + 1
            );
            assert!(
                !line.contains("add-apt-repository") || line.contains(" -n "),
                "{rel}:{}: add-apt-repository must pass -n (no hidden list refresh): {line}",
                i + 1
            );
        }
    }
    for rel in [SETUP_IMAG, BASELINE, KIOSK] {
        let calls = read(rel)
            .lines()
            .filter(|l| l.trim() == "obs_box_apt_update")
            .count();
        assert!(
            calls >= 1,
            "{rel} must refresh the lists via obs_box_apt_update"
        );
    }
    assert!(
        wrapper.contains("LC_ALL=C apt-get update -qq"),
        "the wrapper runs the real update in the C locale (the lock match reads apt's English text)"
    );
}

/// True when a shell line runs `apt`/`apt-get` with an `update` sub-command anywhere after it in the
/// same command segment -- `apt-get update`, `apt-get -qq update`, `apt update`, `/usr/bin/apt-get
/// update` -- so a reordered flag cannot slip past the sweep.
fn runs_apt_update(line: &str) -> bool {
    line.split(['|', ';', '&']).any(|segment| {
        let words: Vec<&str> = segment
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| "\"'$()`{}".contains(c)))
            .collect();
        words.iter().enumerate().any(|(i, w)| {
            let tool = w.rsplit(['/', '(', '=']).next().unwrap_or("");
            (tool == "apt-get" || tool == "apt") && words[i + 1..].contains(&"update")
        })
    })
}

#[test]
fn the_apt_update_matcher_catches_every_update_shape() {
    for hit in [
        "apt-get update -qq",
        "    apt-get -qq update",
        "apt update",
        "sudo /usr/bin/apt-get update && apt-get install -y x",
        "x=$(apt-get update 2>&1)",
    ] {
        assert!(runs_apt_update(hit), "must catch: {hit}");
    }
    for miss in [
        "apt-get install -y curl",
        "obs_box_apt_update",
        "echo update the lists; apt-get install -y x",
        "add-apt-repository -y -n ppa:x",
    ] {
        assert!(!runs_apt_update(miss), "must not flag: {miss}");
    }
}

/// A fake `apt-get` on PATH running `body` (bash); every invocation appends its argv to `$CALLS`,
/// and `n` is the 1-based call number.
fn fake_apt_get_body(dir: &std::path::Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/bash\nset -euo pipefail\necho \"$*\" >> \"$CALLS\"\nn=$(wc -l < \"$CALLS\")\n{body}\n"
    );
    let p = bin.join("apt-get");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

/// apt's messages are translated (Slovak ships), and an ssh session forwards the operator's locale.
/// The held-lock match reads apt's ENGLISH text, so the wrapper runs apt in the C locale -- a
/// translated held-lock message would otherwise fail at once instead of being waited out.
#[test]
fn apt_update_reads_apt_in_the_c_locale() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_apt_get_body(
        dir.path(),
        &format!(
            "if [ \"$n\" -le 1 ]; then\n\
             if [ \"${{LC_ALL:-}}\" = C ]; then printf '%s\\n' '{LISTS_LOCK_HELD}' >&2;\n\
             else printf '%s\\n' 'E: Nepodarilo sa ziskat zamok /var/lib/apt/lists/lock.' >&2; fi\n\
             exit 100\nfi\nexit 0"
        ),
    );
    let calls = dir.path().join("calls");
    let (c, out, err) = run(
        BASELINE,
        &[
            ("PATH", &path),
            ("CALLS", calls.to_str().unwrap()),
            ("OBS_BOX_APT_UPDATE_POLL_S", "0"),
            ("LC_ALL", ""),
            ("LANG", "sk_SK.UTF-8"),
        ],
        "obs_box_apt_update",
    );
    assert_eq!(
        c, 0,
        "the held lock must be waited out: stdout={out} stderr={err}"
    );
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap().lines().count(),
        2,
        "one held attempt, then one success"
    );
}

/// apt can print `W:` lines BEFORE the held-lock error; the wait log names the LOCK line, and a
/// zero poll still sleeps at least 1 s (never a busy loop of apt-get calls).
#[test]
fn apt_update_logs_the_lock_line_and_never_busy_loops() {
    let dir = tempfile::tempdir().unwrap();
    let path = fake_apt_get_body(
        dir.path(),
        &format!(
            "if [ \"$n\" -le 1 ]; then\n\
             printf '%s\\n' 'W: Target Packages is configured multiple times' '{LISTS_LOCK_HELD}' >&2\n\
             exit 100\nfi\nexit 0"
        ),
    );
    let calls = dir.path().join("calls");
    let (c, out, err) = run(
        BASELINE,
        &[
            ("PATH", &path),
            ("CALLS", calls.to_str().unwrap()),
            ("OBS_BOX_APT_UPDATE_POLL_S", "0"),
        ],
        "obs_box_apt_update",
    );
    assert_eq!(c, 0, "stdout={out} stderr={err}");
    let log = format!("{out}{err}");
    let wait = log
        .lines()
        .find(|l| l.contains("apt update: package lists locked"))
        .unwrap_or_else(|| panic!("a wait line: {log}"));
    assert!(
        wait.contains("Could not get lock /var/lib/apt/lists/lock"),
        "the wait line names the lock: {wait}"
    );
    assert!(
        !wait.contains("configured multiple times"),
        "never a leading warning: {wait}"
    );
    assert!(
        wait.contains("waiting 1s"),
        "a zero poll still waits 1 s: {wait}"
    );
}
