//! Issue 1387: the cam2 painter outranks camera-box's JPEG XS encoder on the general cores.
//!
//! With the JPEG XS sender on cam2 (31.10.2026 09:20), the painter at nice 0 painted ~47 fps
//! beside camera-box's `jxs-feed` threads at the unit's `Nice=-10`. The QPSK marker index is the
//! paint frame id, so the stream program-audio guard read FOREIGN and every full-path E2E stopped
//! at its `[0/8]` program-audio preflight. `run_paint_only` now raises every painter thread below
//! camera-box's unit nice before it spawns anything.
#![cfg(target_os = "linux")]

use camera_box::painter_priority::{parse_task_nice, set_all_threads_nice, CAMERA_BOX_UNIT_NICE};
use std::path::Path;
use std::sync::mpsc;

fn repo(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

/// Every `Nice=` value set in `text` (a unit file or a script holding a unit heredoc).
fn unit_nices(text: &str) -> Vec<i32> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("Nice="))
        .map(|v| v.trim().parse().unwrap())
        .collect()
}

#[test]
fn the_painter_runs_below_both_copies_of_the_camera_box_unit_nice() {
    for (file, text) in [
        (
            "systemd/camera-box.service",
            repo("systemd/camera-box.service"),
        ),
        ("scripts/setup-device.sh", repo("scripts/setup-device.sh")),
    ] {
        let nices = unit_nices(&text);
        assert_eq!(
            nices,
            vec![CAMERA_BOX_UNIT_NICE],
            "{file} must set camera-box's Nice= once, at CAMERA_BOX_UNIT_NICE"
        );
    }
    // PAINTER_NICE < CAMERA_BOX_UNIT_NICE is a compile-time assert in src/painter_priority.rs
}

/// The nice of the CALLING thread.
fn own_nice() -> i32 {
    parse_task_nice(&std::fs::read_to_string("/proc/thread-self/stat").unwrap()).unwrap()
}

/// A non-root test can only LOWER its priority, so this sets a positive nice: the mechanism
/// (every existing thread, and a thread created afterwards inherits it) is the same one
/// `raise_painter_priority` uses with PAINTER_NICE as root on cam2. Only the threads this test
/// owns are read: the harness may be starting a parallel test's thread at that very moment
/// (`frame-probe --paint-only` starts no thread before its raise).
#[test]
fn every_thread_of_the_process_gets_the_nice_and_a_later_thread_inherits_it() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let earlier = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        own_nice()
    });
    ready_rx.recv().unwrap();

    let (set, refused) = set_all_threads_nice(7);
    assert_eq!(refused, 0);
    assert!(
        set >= 2,
        "the main thread and the earlier thread at least, got {set}"
    );
    assert_eq!(own_nice(), 7, "the calling thread");
    let later = std::thread::spawn(own_nice).join().unwrap();
    assert_eq!(later, 7, "a thread created afterwards inherits the nice");
    go_tx.send(()).unwrap();
    assert_eq!(earlier.join().unwrap(), 7, "a thread that existed before");
}

#[test]
fn run_paint_only_raises_the_painter_before_it_spawns_a_thread() {
    let run = repo("src/probe/run.rs");
    let body = &run[run.find("pub fn run_paint_only(").unwrap()..];
    let raise = body
        .find("painter_priority::raise_painter_priority()")
        .expect("run_paint_only raises the painter priority");
    let first_spawn = body
        .find("spawn_painter_wedge_watchdog(")
        .expect("the wedge watchdog spawn");
    assert!(
        raise < first_spawn,
        "the raise must come before the first spawned thread"
    );
}
