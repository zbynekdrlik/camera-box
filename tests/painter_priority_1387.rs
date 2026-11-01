//! Issue 1387: the cam2 painter outranks camera-box's JPEG XS encoder on the general cores.
//!
//! With the JPEG XS sender on cam2 (31.10.2026 09:20), the painter at nice 0 painted ~47 fps
//! beside camera-box's `jxs-feed` threads at the unit's `Nice=-10`. The QPSK marker index is the
//! paint frame id, so the stream program-audio guard read FOREIGN and every full-path E2E stopped
//! at its `[0/8]` program-audio preflight. `run_paint_only` now raises every painter thread below
//! camera-box's unit nice before it spawns anything.
#![cfg(target_os = "linux")]

use camera_box::painter_priority::{
    parse_task_nice, set_all_threads_nice, CAMERA_BOX_UNIT_NICE, PAINTER_NICE,
};
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
    assert!(
        PAINTER_NICE < CAMERA_BOX_UNIT_NICE,
        "the painter ({PAINTER_NICE}) must run below camera-box's unit nice ({CAMERA_BOX_UNIT_NICE})"
    );
    assert!((-20..=19).contains(&PAINTER_NICE));
}

fn thread_nices() -> Vec<(String, i32)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir("/proc/self/task").unwrap() {
        let p = e.unwrap().path();
        // a parallel test's thread may end between the listing and the read
        let Ok(stat) = std::fs::read_to_string(p.join("stat")) else {
            continue;
        };
        let nice = parse_task_nice(&stat).expect("a nice field");
        out.push((p.file_name().unwrap().to_string_lossy().into_owned(), nice));
    }
    out
}

/// A non-root test can only LOWER its priority, so this sets a positive nice: the mechanism
/// (every existing thread, and a thread created afterwards inherits it) is the same one
/// `raise_painter_priority` uses with PAINTER_NICE as root on cam2.
#[test]
fn every_thread_of_the_process_gets_the_nice_and_a_later_thread_inherits_it() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let earlier = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        go_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();

    let (set, refused) = set_all_threads_nice(7);
    assert_eq!(refused, 0);
    assert!(
        set >= 2,
        "the main thread and the earlier thread at least, got {set}"
    );
    for (tid, nice) in thread_nices() {
        assert!(nice >= 7, "thread {tid} kept nice {nice}");
    }
    let later = std::thread::spawn(|| {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
        parse_task_nice(&stat).unwrap()
    })
    .join()
    .unwrap();
    assert!(
        later >= 7,
        "a thread created afterwards runs at nice {later}"
    );
    go_tx.send(()).unwrap();
    earlier.join().unwrap();
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
