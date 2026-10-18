//! Issue 1425 (E3 slice S4v, design 6225701358) — the vendored `vlc-video` plugin stamps on the OBS
//! media clock, and the genlock bundle builds, ships and verifies it.
//!
//! Stock `vlc-video` stamped `libvlc_clock()` relative to plugin load: not direct, so libobs latched a
//! `timing_adjust`, and on Windows (VLC's clock is QPC, the mixer the dantesync-disciplined system-time
//! rate since issue 1372) stream's `NL playlist` drifted ~3.6 ms/h per ppm until its audio was re-placed
//! 70 ms at a time. The plugin now maps each `libvlc_clock()` value onto `os_gettime_ns()` by its AGE on
//! VLC's own clock (the issue-1372 mapper `os_foreign_clock_ns_to_gettime_ns`, `util/windows/
//! qpc-timestamp.h`); on Linux `libvlc_clock()` is CLOCK_MONOTONIC, the clock `os_gettime_ns()` reads,
//! so the mapping is the identity.
//!
//! - The mapping: `vlcs_media_clock_ns` lifted VERBATIM from `vlc-video-source.c` with the issue-1372
//!   mapper lifted verbatim from `qpc-timestamp.h`, compiled under `-Werror` for the Windows branch
//!   (`_WIN32` defined after the system headers) and the Linux one, against a truth table.
//! - The bundle: every genlock configure builds the plugin (`-DENABLE_VLC=ON`), the Windows and both
//!   Linux stage steps fail when it is missing, and the FULL fleet deploy backs the box's copy up and
//!   byte-verifies the new one (FAST, obs.dll only, never touches it).
//!
//! Std-only: it runs under `cargo test` AND standalone with plain rustc. FAILS LOUDLY (never skips)
//! when no C compiler or no bash is present.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const VLC_SOURCE: &str = "vendor/obs-studio/plugins/vlc-video/vlc-video-source.c";
const QPC_TIMESTAMP_H: &str = "vendor/obs-studio/libobs/util/windows/qpc-timestamp.h";
const PLUGIN_DEPLOY: &str = "scripts/lib/genlock-plugin-deploy.sh";
const WORKFLOWS: [&str; 3] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
    ".github/workflows/linux-genlock.yml",
];

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    let path = repo(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// One function of `src`, from its signature through its closing `\n}\n`; it must occur exactly once.
fn lift_fn(src: &str, signature: &str) -> String {
    assert_eq!(
        src.matches(signature).count(),
        1,
        "issue 1425: `{signature}` must occur exactly once"
    );
    let a = src.find(signature).expect("signature");
    let b = src[a..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("issue 1425: `{signature}` does not end"));
    src[a..a + b + 3].to_string()
}

static RUNS: AtomicUsize = AtomicUsize::new(0);

fn compile_and_run(c_src: &str, tag: &str) -> Vec<String> {
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!(
        "{tag}_{}_{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).expect("scratch dir");
    let c = dir.join("t.c");
    let bin = dir.join("t.bin");
    fs::write(&c, c_src).expect("write the lift");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
        ])
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1425: could not run the C compiler `{cc}` ({e}); this gate must FAIL, never skip")
        });
    assert!(
        out.status.success(),
        "issue 1425: the lifted VLC media-clock mapping does NOT COMPILE under -Werror:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run the lift");
    assert!(run.status.success(), "issue 1425: the lift exited non-zero");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

/// VLC's clock "now" (us) and OBS's (ns) at the mapping, and the stamps (us) the vectors map.
const VLC_NOW_US: i64 = 5_000_000_000;
const OBS_NOW_NS: u64 = 86_400_000_000_000;
const STAMPS_US: [i64; 10] = [
    VLC_NOW_US - 10_000, // 10 ms ago (a video frame displayed, an audio packet just played)
    VLC_NOW_US,          // now
    VLC_NOW_US + 5_000,  // 5 ms ahead (VLC hands audio over before its presentation date)
    VLC_NOW_US + 1_990_000, // just under libobs's 2 s direct-stamp window (review round 1)
    VLC_NOW_US + 2_010_000, // just over it: the mapping keeps the lead, libobs decides the basis
    VLC_NOW_US - 60_000_000, // the 60 s edge of the foreign-clock mapper
    VLC_NOW_US - 60_000_001, // past it: not on VLC's clock, passed through
    VLC_NOW_US + 60_000_001, // far ahead: passed through
    0,                   // no date
    -5,                  // a negative date: no date
];

fn mapping_lift(windows: bool) -> String {
    let vlc = read(VLC_SOURCE);
    let qpc = read(QPC_TIMESTAMP_H);
    let define = qpc
        .lines()
        .find(|l| l.starts_with("#define OS_FOREIGN_STAMP_MAX_AGE_NS "))
        .expect("issue 1425: qpc-timestamp.h no longer defines OS_FOREIGN_STAMP_MAX_AGE_NS");
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdbool.h>\n#include <stdio.h>\n\
         static int64_t h_vlc_now_us;\nstatic uint64_t h_obs_now_ns;\n\
         static uint64_t os_gettime_ns(void) { return h_obs_now_ns; }\n",
    );
    if windows {
        c.push_str("#define _WIN32 1\n");
    }
    c.push_str(define);
    c.push('\n');
    c.push_str(&lift_fn(
        &qpc,
        "static inline uint64_t os_foreign_clock_map_ns(",
    ));
    c.push_str(&lift_fn(
        &qpc,
        "static inline uint64_t os_foreign_clock_ns_to_gettime_ns(",
    ));
    c.push_str(&lift_fn(
        &vlc,
        "static inline uint64_t vlcs_media_clock_ns(",
    ));
    c.push_str("int main(void)\n{\n\t(void)os_gettime_ns;\n");
    c.push_str(&format!(
        "\th_vlc_now_us = {VLC_NOW_US}ll;\n\th_obs_now_ns = {OBS_NOW_NS}ull;\n"
    ));
    for s in STAMPS_US {
        c.push_str(&format!(
            "\tprintf(\"%llu\\n\", (unsigned long long)vlcs_media_clock_ns({s}ll, h_vlc_now_us));\n"
        ));
    }
    c.push_str("\treturn 0;\n}\n");
    c
}

#[test]
fn a_vlc_stamp_lands_on_the_obs_media_clock_by_its_age_1425() {
    // Windows: the age on VLC's clock, subtracted from (or ahead added to) OBS's now; beyond 60 s the
    // mapper passes the value through (it is not on VLC's clock); no date stays 0
    let n = OBS_NOW_NS;
    let us = |v: i64| (v as u64) * 1000;
    let want_windows: Vec<String> = [
        n - 10_000_000,
        n,
        n + 5_000_000,
        n + 1_990_000_000,
        n + 2_010_000_000,
        n - 60_000_000_000,
        us(VLC_NOW_US - 60_000_001),
        us(VLC_NOW_US + 60_000_001),
        0,
        0,
    ]
    .iter()
    .map(u64::to_string)
    .collect();
    assert_eq!(
        compile_and_run(&mapping_lift(true), "vlc_media_clock_win_1425"),
        want_windows,
        "issue 1425: a Windows VLC stamp must land on os_gettime_ns() by its age on VLC's clock"
    );
    // Linux: libvlc_clock() is CLOCK_MONOTONIC = os_gettime_ns(): the identity (us -> ns)
    let want_linux: Vec<String> = STAMPS_US
        .iter()
        .map(|&s| if s > 0 { us(s) } else { 0 }.to_string())
        .collect();
    assert_eq!(
        compile_and_run(&mapping_lift(false), "vlc_media_clock_linux_1425"),
        want_linux,
        "issue 1425: a Linux VLC stamp is already on the monotonic clock OBS reads"
    );
}

#[test]
fn every_genlock_build_builds_and_stages_vlc_video_1425() {
    for wf in WORKFLOWS {
        let text = read(wf);
        assert!(
            !text.contains("-DENABLE_VLC=OFF"),
            "issue 1425: {wf} builds OBS without the vlc-video plugin again"
        );
    }
    // the configure argument lines themselves (a comment may name the flag too)
    let on = |wf: &str| {
        read(wf)
            .lines()
            .filter(|l| l.trim() == "-DENABLE_VLC=ON")
            .count()
    };
    assert_eq!(on(WORKFLOWS[0]), 1, "issue 1425: the full Windows build");
    assert_eq!(
        on(WORKFLOWS[1]),
        2,
        "issue 1425: both fast-workflow configures (the obs.dll build and the compile gate)"
    );
    assert_eq!(
        on(WORKFLOWS[2]),
        2,
        "issue 1425: both Linux builds (imag parity and strih)"
    );
    assert!(
        read(WORKFLOWS[0]).contains(
            "if (!(Test-Path \"stage/obs-plugins/64bit/vlc-video.dll\")) { Write-Error \"vlc-video.dll not built (issue 1425)"
        ),
        "issue 1425: the Windows stage step must fail when vlc-video.dll is missing"
    );
    let linux = read(WORKFLOWS[2]);
    assert_eq!(
        linux
            .matches("if [ ! -f \"$plugin_lib_dir/vlc-video.so\" ]; then echo \"ERROR: vlc-video.so not built (issue 1425)")
            .count(),
        2,
        "issue 1425: both Linux stage steps must fail when vlc-video.so is missing"
    );
    let libvlc = linux
        .lines()
        .any(|l| l.trim_start().starts_with("libsndio-dev") && l.contains("libvlc-dev"));
    assert!(
        libvlc,
        "issue 1425: the Linux runners need libvlc-dev (the vlc/libvlc.h header the plugin builds against)"
    );
}

fn plugin_ps(func: &str, mode: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -euo pipefail; . \"$1\"; \"$2\" \"$3\"")
        .arg("plugin-deploy")
        .arg(repo(PLUGIN_DEPLOY))
        .arg(func)
        .arg(mode)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1425: could not run bash ({e}); this gate must FAIL, never skip")
        });
    assert!(
        out.status.success(),
        "issue 1425: {func} {mode} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

#[test]
fn the_full_deploy_backs_up_and_verifies_vlc_video_1425() {
    let backup = plugin_ps("genlock_plugin_backup_ps", "full");
    assert!(
        backup.contains(
            "Copy-Item -Force (Join-Path $obsDir 'obs-plugins\\64bit\\vlc-video.dll') (Join-Path $backupDir 'vlc-video.dll.pre-789')"
        ),
        "issue 1425: the FULL deploy keeps the box's current vlc-video.dll for rollback:\n{backup}"
    );
    let deploy = plugin_ps("genlock_plugin_deploy_ps", "full");
    for needle in [
        "$pfVlc = Join-Path $obsDir 'obs-plugins\\64bit\\vlc-video.dll'",
        "'(^|/)vlc-video\\.dll$'",
        "(Get-FileHash -Algorithm SHA256 $pfVlc -ErrorAction Stop).Hash.ToLower()",
        "VERIFY vlc-video.dll want=$wantL got=$gotL match=$matchL",
        "VERIFY FAIL: deployed vlc-video.dll bytes do not match the bundle manifest",
        "the bundle carries no vlc-video.dll (built before issue 1425)",
    ] {
        assert!(
            deploy.contains(needle),
            "issue 1425: the FULL deploy no longer byte-verifies vlc-video.dll (`{needle}`):\n{deploy}"
        );
    }
    for func in ["genlock_plugin_backup_ps", "genlock_plugin_deploy_ps"] {
        let fast = plugin_ps(func, "fast");
        assert!(
            !fast.contains("Copy-Item") && !fast.contains("VERIFY vlc-video.dll"),
            "issue 1425: a FAST (obs.dll-only) deploy never touches vlc-video:\n{fast}"
        );
    }
}

/// The audio lead log (`vlcs_audio_lead_log`, lifted VERBATIM): the first lead is printed at once, then
/// the largest lead of each 60 s window, never more often (review round 1: the live acceptance reads how
/// close VLC's audio presentation date comes to libobs's 2 s direct-stamp window).
#[test]
fn the_audio_lead_is_logged_once_a_minute_with_its_window_max_1425() {
    let vlc = read(VLC_SOURCE);
    let define = vlc
        .lines()
        .find(|l| l.starts_with("#define VLCS_AUDIO_LEAD_LOG_NS "))
        .expect("issue 1425: vlc-video-source.c no longer defines VLCS_AUDIO_LEAD_LOG_NS");
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdbool.h>\n#include <stdio.h>\n#include <stdarg.h>\n\
         #define LOG_INFO 300\n\
         typedef struct obs_source obs_source_t;\n\
         struct vlc_source { obs_source_t *source; int64_t audio_lead_max_us; bool audio_lead_seen; uint64_t audio_lead_log_ns; };\n\
         static const char *obs_source_get_name(const obs_source_t *s) { (void)s; return \"NL playlist\"; }\n\
         __attribute__((format(printf, 2, 3))) static void blog(int level, const char *fmt, ...)\n\
         { va_list ap; va_start(ap, fmt); printf(\"%d \", level); vprintf(fmt, ap); printf(\"\\n\"); va_end(ap); }\n",
    );
    c.push_str(define);
    c.push('\n');
    c.push_str(&lift_fn(&vlc, "static void vlcs_audio_lead_log("));
    // (lead in us, OBS now in ns): first call at t = 5 s, more leads inside the minute, then 60 s later
    let calls: [(i64, u64); 6] = [
        (40_000, 5_000_000_000),
        (1_990_000, 20_000_000_000),
        (-3_000, 64_999_999_999),
        (12_000, 65_000_000_000),
        (8_000, 90_000_000_000),
        (9_000, 125_000_000_000),
    ];
    c.push_str("int main(void)\n{\n\tstruct vlc_source v = {0};\n");
    for (lead, now) in calls {
        c.push_str(&format!(
            "\tvlcs_audio_lead_log(&v, {lead}ll, {now}ull);\n\tprintf(\"-\\n\");\n"
        ));
    }
    c.push_str("\treturn 0;\n}\n");
    let line =
        |ms: &str| format!("300 [vlc_source: 'NL playlist'] audio lead_max_ms={ms} (issue 1425)");
    let want = vec![
        line("40.0"),
        "-".to_string(),
        "-".to_string(),
        "-".to_string(),
        // 60 s after the first line: the window's largest lead (1.99 s), the current one included
        line("1990.0"),
        "-".to_string(),
        "-".to_string(),
        // the next window: 8 ms and 9 ms, printed 60 s after the previous line
        line("9.0"),
        "-".to_string(),
    ];
    assert_eq!(
        compile_and_run(&c, "vlc_audio_lead_1425"),
        want,
        "issue 1425: the VLC audio lead is logged once a minute with the window's largest lead"
    );
}
