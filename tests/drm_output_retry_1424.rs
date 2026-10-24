//! issue 1424 (owner 24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni monitora aj
//! po zapnuti!!!"; main design 6386467303, layer 3) -- a vk-direct HDMI output never stays dark until
//! the next OBS start:
//!
//! * a DORMANT output (its start failed: the acquire refused, no Vulkan display drives the connector
//!   because its monitor is off, ...) retries its whole start with a bounded backoff (2 s for the first
//!   attempts, then 5 s; 30 s while X RandR reads the connector disconnected, and at once when it
//!   reads connected again), its failure lines logged once per reason change;
//! * a LIVE output whose presentation goes out of date (an unplugged or switched-off monitor) KEEPS
//!   its display acquired and keeps rebuilding the presentation (1 s for ten tries, then every 5 s)
//!   while another display of the process is open -- NVIDIA refuses to re-acquire a display this
//!   process released while the shared VkInstance lives on; alone, it gives up after the fast tries,
//!   so the supervisor releases it and the dormant retry opens it on a fresh instance;
//! * a LIVE output whose present loop died anyway is released by the supervisor and retried;
//! * never an OBS restart.
//!
//! Facet A pins the wiring with std-only source anchors. Facet B is the C-vs-Rust parity gate: the
//! PURE header `obs-drm-output-retry.h` is compiled for real (`-Wall -Wextra -Wconversion -Werror`)
//! into a harness that runs scripted supervisor scenarios, and the Rust twin
//! `camera_box::drm_output_retry` must print the same lines. It FAILS LOUDLY when no C compiler is
//! present (never skips).

use camera_box::drm_output_retry as r;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

const RETRY_H: &str = "vendor/obs-studio/libobs/obs-drm-output-retry.h";
const RETRY_C: &str = "vendor/obs-studio/libobs/obs-drm-output-retry.c";
const OUTPUTS_C: &str = "vendor/obs-studio/libobs/obs-drm-output-outputs.c";
const BACKEND_C: &str = "vendor/obs-studio/libobs/obs-drm-output-backend.c";
const INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-internal.h";
const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
const VK_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h";
const LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";
const FRONTEND: &str = "vendor/obs-studio/frontend/components/DrmOutputView.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of the C function whose DEFINITION starts with `sig` (up to the first `\n}\n`).
fn body_of_c(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1424: `{sig}` not found"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("closing brace");
    src[start..end].to_string()
}

// ------------------------------------------------------------------------------------------------
// Facet A -- the wiring
// ------------------------------------------------------------------------------------------------

#[test]
fn the_retry_header_is_pure_std_only_c() {
    let h = file(RETRY_H);
    for line in h.lines().filter(|l| l.trim_start().starts_with("#include")) {
        assert!(
            line.contains("<stdbool.h>") || line.contains("<stdint.h>"),
            "issue 1424: the pure retry header may include only stdbool/stdint, found {line:?}"
        );
    }
    for f in [
        "drm_output_retry_delay_ns",
        "drm_output_retry_step",
        "drm_output_retry_released",
        "drm_output_retry_attempted",
        "drm_output_rebuild_next",
        "drm_output_rebuild_log_kind",
    ] {
        assert!(
            h.contains("static inline") && h.contains(&format!("{f}(")),
            "issue 1424: the header must define {f}"
        );
    }
}

#[test]
fn the_supervisor_tu_is_linux_only_and_built_by_libobs() {
    let c = file(RETRY_C);
    assert!(
        c.contains("#if defined(__linux__)"),
        "issue 1424: Linux-only TU"
    );
    assert!(c.contains("#include \"obs-drm-output-retry.h\""));
    let cmake = squish(&file(LINUX_CMAKE));
    assert!(
        cmake.contains("obs-drm-output-retry.c") && cmake.contains("obs-drm-output-retry.h"),
        "issue 1424: os-linux.cmake must build the retry TU and list its header"
    );
}

#[test]
fn the_autostart_starts_the_supervisor_and_the_stop_joins_it_first() {
    let o = file(OUTPUTS_C);
    let auto = body_of_c(&o, "void obs_drm_output_maybe_autostart(void)\n{");
    assert!(
        auto.contains("drm_output_retry_start();"),
        "issue 1424: the autostart must start the retry supervisor after the outputs started"
    );
    let stop = squish(&body_of_c(&o, "void obs_drm_output_stop(void)\n{"));
    let join = stop
        .find("drm_output_retry_stop();")
        .expect("issue 1424: obs_drm_output_stop must stop the retry supervisor");
    let lock = stop
        .find("pthread_mutex_lock(&g_outs_lock);")
        .expect("registry lock");
    assert!(
        join < lock,
        "issue 1424: the supervisor is joined BEFORE the registry lock (it takes that lock itself)"
    );
}

#[test]
fn the_registry_exposes_its_lock_and_the_start_stop_steps_to_the_supervisor() {
    let h = file(INTERNAL_H);
    for decl in [
        "void drm_output_registry_lock(void);",
        "void drm_output_registry_unlock(void);",
        "bool drm_output_inst_start_locked(struct drm_output_inst *inst);",
        "void drm_output_inst_stop_locked(struct drm_output_inst *inst);",
        "bool (*alive)(struct drm_output_inst *inst);",
        "void drm_output_retry_start(void);",
        "void drm_output_retry_stop(void);",
    ] {
        assert!(
            h.contains(decl),
            "issue 1424: internal header must declare `{decl}`"
        );
    }
    let o = file(OUTPUTS_C);
    assert!(!o.contains("static bool drm_output_inst_start_locked("));
    assert!(!o.contains("static void drm_output_inst_stop_locked("));
}

#[test]
fn the_vk_backend_reports_a_dead_present_loop_and_its_active_read_never_blocks() {
    let b = file(BACKEND_C);
    assert!(
        squish(&b).contains(".alive = vkd_alive,"),
        "issue 1424: the vk-direct ops carry an alive() the supervisor reads"
    );
    let alive = body_of_c(&b, "static bool vkd_alive(struct drm_output_inst *inst)\n{");
    assert!(alive.contains("drm_output_vk_wants_frames("));
    // The frontend polls active() on the UI thread; a retry holds k->lock across a whole open
    // (seconds), so the read is a lock-free atomic.
    let active = body_of_c(
        &b,
        "static bool vkd_active(struct drm_output_inst *inst)\n{",
    );
    assert!(
        active.contains("os_atomic_load_bool(") && !active.contains("pthread_mutex_lock"),
        "issue 1424: vkd_active must not take k->lock"
    );
}

#[test]
fn the_present_loop_keeps_the_display_and_backs_off_instead_of_giving_up_after_ten_tries() {
    let v = file(VK_C);
    let reb = body_of_c(
        &v,
        "static bool drm_output_vk_rebuild_or_give_up(struct drm_output_vk_disp *d, VkResult why, unsigned *rebuilds)\n{",
    );
    assert!(
        reb.contains("drm_output_rebuild_next("),
        "issue 1424: the rebuild loop asks the pure decision"
    );
    assert!(
        reb.contains("drm_output_rebuild_log_kind("),
        "issue 1424: the try lines are logged through the pure log decision"
    );
    assert!(
        !v.contains("DRM_OUTPUT_VK_REBUILD_TRIES"),
        "issue 1424: the old fixed give-up after 10 tries is gone"
    );
    assert!(v.contains("#include \"obs-drm-output-retry.h\""));
}

#[test]
fn the_supervisor_logs_one_line_per_state_change_and_quiets_the_attempts() {
    let c = file(RETRY_C);
    for marker in [
        "drm-output: retry: '%s' dormant",
        "drm-output: retry: '%s' LIVE again",
        "drm-output: retry: '%s' lost its present loop",
    ] {
        assert!(
            c.contains(marker),
            "issue 1424: missing log line {marker:?}"
        );
    }
    // the failed attempt's own lines are captured, never printed every 2-5 s
    assert!(c.contains("drm_output_blog("));
    for h in [INTERNAL_H, VK_INTERNAL_H] {
        assert!(
            file(h).contains("#define blog drm_output_blog"),
            "issue 1424: {h} routes the module's blog through the capture"
        );
    }
}

/// Independent review: the X reads take their errors (an X error a reply call is not handed stays in the
/// never-read event queue and grows it), and the supervisor reads `started` under the registry lock (the
/// exported `obs_drm_output_start` writes it under that lock too).
#[test]
fn the_supervisor_frees_its_x_errors_and_reads_started_under_the_registry_lock() {
    let c = file(RETRY_C);
    let conn = squish(&body_of_c(
        &c,
        "static bool drm_output_retry_connected(xcb_connection_t **xc, const char *name)",
    ));
    assert!(
        conn.matches("&err)").count() >= 2 && conn.matches("free(err)").count() >= 2,
        "issue 1424: both reply calls take and free their X error: {conn}"
    );
    let th = squish(&body_of_c(
        &c,
        "static void *drm_output_retry_thread(void *arg)",
    ));
    let lock = th
        .find("drm_output_registry_lock();")
        .expect("the thread takes the registry lock");
    let read = th
        .find("const bool started = inst->started;")
        .expect("the thread reads started into a local");
    let unlock = th
        .find("drm_output_registry_unlock();")
        .expect("the thread releases the registry lock");
    let x = th
        .find("drm_output_retry_connected(&xc")
        .expect("the X read");
    assert!(
        lock < read && read < unlock && unlock < x,
        "issue 1424: `started` is read under the registry lock, the X read runs outside it: {th}"
    );
}

#[test]
fn the_frontend_reattaches_the_multiview_when_an_output_comes_back() {
    let f = squish(&file(FRONTEND));
    assert!(
        f.contains("QTimer"),
        "issue 1424: the frontend re-syncs the Multiview on a timer (an output that goes LIVE after FINISHED_LOADING)"
    );
    assert!(f.contains("SyncDrmMultiviewIfChanged"));
}

// ------------------------------------------------------------------------------------------------
// Facet B -- the pure decision: Rust truth tables + the C-vs-Rust parity gate
// ------------------------------------------------------------------------------------------------

const S: u64 = 1_000_000_000;

#[test]
fn a_dormant_output_retries_fast_then_slow_and_slowest_while_unplugged() {
    assert_eq!(r::delay_ns(0, true), 2 * S);
    assert_eq!(r::delay_ns(4, true), 2 * S);
    assert_eq!(r::delay_ns(5, true), 5 * S);
    assert_eq!(r::delay_ns(19, true), 5 * S);
    // review finding: a connected-but-off screen (a dark venue) is not retried on a fresh Vulkan
    // instance every 5 s forever -- after 20 failed starts every 30 s (a plug-in / switch-on still
    // pulls the next try in, the X reconnect rule)
    assert_eq!(r::SLOW_TRIES, 20);
    assert_eq!(r::delay_ns(20, true), 30 * S);
    assert_eq!(r::delay_ns(u32::MAX, true), 30 * S);
    assert_eq!(r::delay_ns(0, false), 30 * S);
    assert_eq!(r::delay_ns(9, false), 30 * S);
}

#[test]
fn a_failed_start_goes_dormant_and_retries_on_its_schedule() {
    let mut st = r::Retry::default();
    // the autostart's start failed: the first look sees it not started
    assert_eq!(r::step(&mut st, false, false, true, 10 * S), r::NONE);
    assert_eq!(st.state, r::DORMANT);
    assert_eq!(st.next_ns, 12 * S);
    assert_eq!(r::step(&mut st, false, false, true, 11 * S), r::NONE);
    assert_eq!(r::step(&mut st, false, false, true, 12 * S), r::START);
    r::attempted(&mut st, false, true, 12 * S);
    assert_eq!((st.fails, st.next_ns), (1, 14 * S));
    for _ in 0..4 {
        let t = st.next_ns;
        assert_eq!(r::step(&mut st, false, false, true, t), r::START);
        r::attempted(&mut st, false, true, t);
    }
    assert_eq!(st.fails, 5);
    // the sixth failure waits the slow step
    let t = st.next_ns;
    r::attempted(&mut st, false, true, t);
    assert_eq!(st.next_ns, t + 5 * S);
    // success: LIVE, the count cleared
    let t = st.next_ns;
    assert_eq!(r::step(&mut st, false, false, true, t), r::START);
    r::attempted(&mut st, true, true, t);
    assert_eq!((st.state, st.fails), (r::LIVE, 0));
    assert_eq!(r::step(&mut st, true, true, true, t + S), r::NONE);
}

#[test]
fn an_unplugged_connector_is_tried_rarely_and_at_once_when_it_comes_back() {
    let mut st = r::Retry::default();
    assert_eq!(r::step(&mut st, false, false, false, 0), r::NONE);
    assert_eq!(st.next_ns, 30 * S);
    assert_eq!(r::step(&mut st, false, false, false, 10 * S), r::NONE);
    // the monitor is plugged in / switched on: try now, not 20 s later
    assert_eq!(r::step(&mut st, false, false, true, 11 * S), r::START);
    r::attempted(&mut st, false, true, 11 * S);
    assert_eq!(st.next_ns, 13 * S);
}

#[test]
fn a_live_output_whose_present_loop_died_is_released_and_retried() {
    let mut st = r::Retry::default();
    assert_eq!(r::step(&mut st, true, true, true, 0), r::NONE);
    assert_eq!(st.state, r::LIVE);
    assert_eq!(r::step(&mut st, true, false, true, 5 * S), r::RELEASE);
    r::released(&mut st, true, 5 * S);
    assert_eq!((st.state, st.fails, st.next_ns), (r::DORMANT, 0, 7 * S));
    assert_eq!(r::step(&mut st, false, false, true, 7 * S), r::START);
}

#[test]
fn the_rebuild_keeps_the_display_beside_another_open_display_and_gives_up_alone() {
    for tries in 1..10 {
        assert_eq!(r::rebuild_next(tries, true), Some(S), "tries {tries}");
        assert_eq!(r::rebuild_next(tries, false), Some(S), "tries {tries}");
    }
    assert_eq!(r::rebuild_next(10, true), Some(5 * S));
    assert_eq!(r::rebuild_next(500, true), Some(5 * S));
    assert_eq!(r::rebuild_next(10, false), None);
    assert_eq!(r::rebuild_next(11, false), None);
    // the try lines: one per fast try, ONE switch line, then quiet
    assert_eq!(r::rebuild_log_kind(1), r::REBUILD_LOG_TRY);
    assert_eq!(r::rebuild_log_kind(10), r::REBUILD_LOG_TRY);
    assert_eq!(r::rebuild_log_kind(11), r::REBUILD_LOG_SLOW);
    assert_eq!(r::rebuild_log_kind(12), r::REBUILD_LOG_NONE);
    assert_eq!(r::rebuild_log_kind(u32::MAX), r::REBUILD_LOG_NONE);
}

/// One supervisor scenario event: a look (started, alive, connected, now) followed by what the
/// supervisor then did -- 'a' attempted ok, 'f' attempted failed, 'r' released, '-' nothing.
type Ev = (bool, bool, bool, u64, char);

/// A scenario: its name, the state it starts from (state, fails, next_ns) and its events.
type Scenario = (&'static str, (i32, u32, u64), Vec<Ev>);

fn scenarios() -> Vec<Scenario> {
    vec![
        (
            "boot-dormant-then-monitor",
            (0, 0, 0),
            vec![
                (false, false, true, 0, '-'),
                (false, false, true, 2 * S, 'f'),
                (false, false, true, 4 * S, 'f'),
                (false, false, true, 6 * S, 'f'),
                (false, false, true, 8 * S, 'f'),
                (false, false, true, 10 * S, 'f'),
                (false, false, true, 12 * S, 'f'),
                (false, false, true, 17 * S, 'a'),
                (true, true, true, 18 * S, '-'),
            ],
        ),
        (
            "unplugged-then-plug",
            (0, 0, 0),
            vec![
                (false, false, false, 0, '-'),
                // still unplugged: never pulled in early (only a connected read pulls it in)
                (false, false, false, 10 * S, '-'),
                (false, false, false, 29 * S, '-'),
                (false, false, false, 30 * S, 'f'),
                (false, false, true, 31 * S, 'f'),
                (false, false, false, 32 * S, '-'),
                (false, false, true, 33 * S, 'a'),
            ],
        ),
        (
            "live-dies-released",
            (0, 0, 0),
            vec![
                (true, true, true, 0, '-'),
                (true, false, true, S, 'r'),
                (false, false, true, 2 * S, '-'),
                (false, false, true, 3 * S, 'a'),
                (true, true, true, 4 * S, '-'),
            ],
        ),
        (
            "boundary",
            (0, 0, 0),
            vec![
                (false, false, true, u64::MAX - 10 * S, '-'),
                (false, false, true, u64::MAX - 8 * S, 'f'),
                (false, false, true, u64::MAX - 7 * S, '-'),
            ],
        ),
        (
            // a dormant output whose fail count reached the top: it saturates, never wraps to 0 (which
            // would read as the fast schedule again)
            "saturated",
            (1, u32::MAX, 0),
            vec![
                (false, false, true, S, 'f'),
                (false, false, true, 3 * S, '-'),
            ],
        ),
        (
            // dormant and unplugged with a far next try: a connected read pulls it in, a later
            // disconnected one keeps the pulled-in time
            "far-next-unplugged",
            (1, 7, 40 * S),
            vec![
                (false, false, false, 5 * S, '-'),
                (false, false, true, 6 * S, 'f'),
                (false, false, false, 7 * S, '-'),
            ],
        ),
        (
            // review finding: past 20 failed starts a connected output waits 30 s, yet a screen that
            // leaves X and comes back is still tried at once (the unplug is a tracked transition, not
            // read off the next-try distance)
            "long-backoff-replug",
            (1, 30, 100 * S),
            vec![
                (false, false, true, 71 * S, '-'),
                (false, false, true, 100 * S, 'f'),
                (false, false, true, 101 * S, '-'),
                (false, false, false, 102 * S, '-'),
                (false, false, true, 103 * S, 'f'),
                (false, false, true, 104 * S, '-'),
            ],
        ),
    ]
}

#[test]
fn a_reconnect_pulls_the_try_in_also_after_the_long_backoff() {
    let mut st = r::Retry::default();
    assert_eq!(r::step(&mut st, false, false, true, 0), r::NONE);
    let mut now = 0;
    for _ in 0..25 {
        now = st.next_ns;
        assert_eq!(r::step(&mut st, false, false, true, now), r::START);
        r::attempted(&mut st, false, true, now);
    }
    assert_eq!(
        st.next_ns,
        now + 30 * S,
        "25 failed starts on a connected screen: 30 s"
    );
    // still connected: the long backoff holds
    assert_eq!(r::step(&mut st, false, false, true, now + S), r::NONE);
    assert!(!st.unplugged);
    // the screen leaves X, then comes back: tried at once
    assert_eq!(r::step(&mut st, false, false, false, now + 2 * S), r::NONE);
    assert!(st.unplugged);
    assert_eq!(r::step(&mut st, false, false, true, now + 3 * S), r::START);
    assert!(!st.unplugged);
}

fn rust_lines() -> Vec<String> {
    let mut out = Vec::new();
    for f in [0u32, 1, 4, 5, 6, 19, 20, 21, 100, u32::MAX] {
        for c in [false, true] {
            out.push(format!("delay:{f}:{} {}", c as i32, r::delay_ns(f, c)));
        }
    }
    for t in [0u32, 1, 2, 9, 10, 11, 12, 1000, u32::MAX] {
        for o in [false, true] {
            let (ok, w) = match r::rebuild_next(t, o) {
                Some(w) => (1, w),
                None => (0, 0),
            };
            out.push(format!("rebuild:{t}:{} {ok} {w}", o as i32));
        }
        out.push(format!("logkind:{t} {}", r::rebuild_log_kind(t)));
    }
    for (name, (state, fails, next_ns), evs) in scenarios() {
        let mut st = r::Retry {
            state,
            fails,
            next_ns,
            unplugged: false,
        };
        for (i, &(started, alive, conn, now, then)) in evs.iter().enumerate() {
            let act = r::step(&mut st, started, alive, conn, now);
            match then {
                'a' => r::attempted(&mut st, true, conn, now),
                'f' => r::attempted(&mut st, false, conn, now),
                'r' => r::released(&mut st, conn, now),
                _ => {}
            }
            out.push(format!(
                "sc:{name}:{i} act={act} state={} fails={} next={} unplugged={}",
                st.state, st.fails, st.next_ns, st.unplugged as i32
            ));
        }
    }
    out
}

fn c_harness() -> String {
    let mut src = String::from(
        "#include <stdio.h>\n#include <inttypes.h>\n#include \"obs-drm-output-retry.h\"\n\
         int main(void) {\n uint64_t w = 0; struct drm_output_retry st; int act = 0;\n (void)act;\n",
    );
    for f in [0u32, 1, 4, 5, 6, 19, 20, 21, 100, u32::MAX] {
        for c in [false, true] {
            src.push_str(&format!(
                " printf(\"delay:{f}:{} %\" PRIu64 \"\\n\", drm_output_retry_delay_ns({f}U, {}));\n",
                c as i32, c
            ));
        }
    }
    for t in [0u32, 1, 2, 9, 10, 11, 12, 1000, u32::MAX] {
        for o in [false, true] {
            src.push_str(&format!(
                " {{ w = 0; bool ok = drm_output_rebuild_next({t}U, {o}, &w); printf(\"rebuild:{t}:{} %d %\" PRIu64 \"\\n\", ok ? 1 : 0, ok ? w : (uint64_t)0); }}\n",
                o as i32
            ));
        }
        src.push_str(&format!(
            " printf(\"logkind:{t} %d\\n\", drm_output_rebuild_log_kind({t}U));\n"
        ));
    }
    for (name, (state, fails, next_ns), evs) in scenarios() {
        src.push_str(&format!(
            " st.state = {state}; st.fails = {fails}U; st.next_ns = {next_ns}ULL; st.unplugged = false;\n"
        ));
        for (i, &(started, alive, conn, now, then)) in evs.iter().enumerate() {
            src.push_str(&format!(
                " act = drm_output_retry_step(&st, {started}, {alive}, {conn}, {now}ULL);\n"
            ));
            match then {
                'a' => src.push_str(&format!(
                    " drm_output_retry_attempted(&st, true, {conn}, {now}ULL);\n"
                )),
                'f' => src.push_str(&format!(
                    " drm_output_retry_attempted(&st, false, {conn}, {now}ULL);\n"
                )),
                'r' => src.push_str(&format!(
                    " drm_output_retry_released(&st, {conn}, {now}ULL);\n"
                )),
                _ => {}
            }
            src.push_str(&format!(
                " printf(\"sc:{name}:{i} act=%d state=%d fails=%\" PRIu32 \" next=%\" PRIu64 \" unplugged=%d\\n\", act, st.state, st.fails, st.next_ns, st.unplugged ? 1 : 0);\n"
            ));
        }
    }
    src.push_str(" return 0;\n}\n");
    src
}

fn compile_and_run(src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "drm_output_retry_1424-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).expect("tmp dir");
    let c = dir.join("harness.c");
    let bin = dir.join("harness");
    fs::write(&c, src).expect("write harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wformat=2",
            "-Werror",
            "-I",
        ])
        .arg(repo("vendor/obs-studio/libobs"))
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1424: no C compiler ({cc}): {e} -- the parity gate cannot run")
        });
    if !out.status.success() {
        let _ = fs::remove_dir_all(&dir);
    }
    assert!(
        out.status.success(),
        "issue 1424: the pure retry header must compile cleanly:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(run.status.success(), "issue 1424: harness failed");
    String::from_utf8(run.stdout).expect("utf8")
}

#[test]
fn c_header_and_rust_twin_agree_on_every_vector() {
    let c_out = compile_and_run(&c_harness());
    let c_lines: Vec<&str> = c_out.lines().collect();
    let r_lines = rust_lines();
    assert_eq!(
        c_lines.len(),
        r_lines.len(),
        "issue 1424: line count differs"
    );
    for (c, rl) in c_lines.iter().zip(r_lines.iter()) {
        assert_eq!(*c, rl.as_str(), "issue 1424: C and Rust disagree");
    }
}
