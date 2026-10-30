//! Issue 1387 -- the in-OBS JPEG XS indicator and the per-source transport tag (owner request
//! 29.10.2026, design 6506884476).
//!
//! The pieces compile only on CI (DistroAV's strih build, the OBS frontend), so this gate proves
//! them offline:
//! 1. DistroAV's process-wide state table (`vendor/distroav/src/jxs-state-table.h`) is compiled
//!    as shipped into a driver and run: publish / frame / window / remove, the JSON snapshot (parsed
//!    here), escaping, concurrent writers, ONE instance shared by two translation units;
//! 2. the frontend report (`widgets/GenlockJxsReport.cpp`, plain std C++) is compiled as shipped and
//!    replayed over scripted tables; every state and reason equals the Rust authority's;
//! 3. the anchors: the proc handler, the fill points in the source and the output, the plugin
//!    registration, the status-bar wiring, the CMake lists, the Sources-dock tag, the JSON keys the
//!    widget reads = the keys the table writes, the `jxs-state:` log family;
//! 4. the pwsh mirrors in BOTH windows-genlock ymls carry every anchor of this file's list.
//!
//! A compiler missing = FAIL, never a skip.

use camera_box::genlock_jxs_state::{
    decide, entry_health, JxsCounts, JxsEntryFacets, JxsHealth, JxsKind,
};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const FE: &str = "vendor/obs-studio/frontend/";
const TABLE: &str = "vendor/distroav/src/jxs-state-table.h";
const SOURCE: &str = "vendor/distroav/src/jxs-source.cpp";
const OUTPUT: &str = "vendor/distroav/src/jxs-output.cpp";
const PLUGIN_MAIN: &str = "vendor/distroav/src/plugin-main.cpp";
const DISTROAV_CMAKE: &str = "vendor/distroav/CMakeLists.txt";
const WIDGET: &str = "vendor/obs-studio/frontend/widgets/GenlockJxsWidget.cpp";
const WIDGET_HPP: &str = "vendor/obs-studio/frontend/widgets/GenlockJxsWidget.hpp";
const REPORT: &str = "vendor/obs-studio/frontend/widgets/GenlockJxsReport.cpp";
const REPORT_HPP: &str = "vendor/obs-studio/frontend/widgets/GenlockJxsReport.hpp";
const STATUSBAR: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.cpp";
const STATUSBAR_HPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.hpp";
const TREE: &str = "vendor/obs-studio/frontend/components/SourceTreeItem.cpp";
const TREE_HPP: &str = "vendor/obs-studio/frontend/components/SourceTreeItem.hpp";
const WIN_YMLS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];

static RUNS: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> tempfile::TempDir {
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    tempfile::Builder::new()
        .prefix(&format!("jxs_indicator_1387-{tag}-{n}-"))
        .tempdir()
        .unwrap()
}

fn cxx() -> String {
    std::env::var("CXX").unwrap_or_else(|_| "g++".to_string())
}

/// Compile the given C++ sources (+ include dirs) and run the binary; its stdout.
fn build_and_run(
    tag: &str,
    sources: &[(&str, String)],
    extra: &[PathBuf],
    includes: &[PathBuf],
) -> String {
    let dir = scratch(tag);
    let bin = dir.path().join("driver.bin");
    let mut cmd = Command::new(cxx());
    cmd.args([
        "-std=c++17",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-O1",
        "-pthread",
    ]);
    for inc in includes {
        cmd.arg("-I").arg(inc);
    }
    for (name, text) in sources {
        let p = dir.path().join(name);
        fs::write(&p, text).unwrap();
        cmd.arg(&p);
    }
    for p in extra {
        cmd.arg(p);
    }
    cmd.arg("-o").arg(&bin);
    let out = cmd.output().unwrap_or_else(|e| {
        panic!(
            "{}: cannot run ({e}); this gate must FAIL, never skip",
            cxx()
        )
    });
    assert!(
        out.status.success(),
        "compile {tag} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().unwrap();
    assert!(
        run.status.success(),
        "{tag} driver failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout).unwrap()
}

// ---------------------------------------------------------------------------------------------
// 1. the state table

const TABLE_DRIVER: &str = r#"
#include "jxs-state-table.h"
#include <cstdio>
#include <thread>
#include <vector>
void *second_tu_table();
static int a, b, c;
int main()
{
	cb_jxs_state_table t;
	printf("%s\n", t.json(1000).c_str());
	t.set_entry(&a, CB_JXS_STATE_INPUT, "JXS cam5 \"src\" \\ end", "UDP 7000, camera 5");
	t.set_entry(&b, CB_JXS_STATE_OUTPUT, "camera-box JPEG XS program", "to 10.77.9.204:7100");
	printf("%s\n", t.json(1000).c_str());
	t.note_frame(&a, 2000000000ULL);
	t.publish_window(&a, 3000, 798, 2, 1, 3, 4, -1);
	t.note_frame(&b, 2400000000ULL);
	t.publish_window(&b, 2999, 800, 0, 0, 1, 0, 1500);
	/* an unknown key is never created by a frame or a window */
	t.note_frame(&c, 2000000000ULL);
	t.publish_window(&c, 1, 1, 1, 1, 1, 1, 1);
	printf("%s\n", t.json(2500000000ULL).c_str());
	/* a clock read before the last frame reads 0, never a wrap */
	printf("%s\n", t.json(1000ULL).c_str());
	/* a rename keeps the counters */
	t.set_entry(&a, CB_JXS_STATE_INPUT, "Cam 5 renamed", "UDP 7001, camera 5");
	t.remove(&b);
	t.set_entry(&c, CB_JXS_STATE_INPUT, std::string("ctl\x01" "x"), "");
	printf("%s\n", t.json(2500000000ULL).c_str());
	printf("size %zu\n", t.size());
	/* a name that is not UTF-8 (a lone lead byte, an overlong form, a surrogate) next to a real
	 * UTF-8 one, and a window delta that wrapped: the snapshot must stay JSON jansson takes */
	cb_jxs_state_table v;
	v.set_entry(&a, CB_JXS_STATE_INPUT, std::string("bad\xff" "x\xc0\xaf" "y\xed\xa0\x80" "z\xc4"), "kamera \xc4\x8d");
	v.note_frame(&a, 1ULL);
	v.publish_window(&a, 3000, 800, ~0ULL, 0, 0, 0, -1);
	printf("%s\n", v.json(1ULL).c_str());
	/* concurrent writers + a reader: every snapshot must stay whole JSON */
	cb_jxs_state_table u;
	std::vector<std::thread> th;
	static int keys[4];
	for (int k = 0; k < 4; k++)
		th.emplace_back([&u, k]() {
			for (int i = 0; i < 2000; i++) {
				u.set_entry(&keys[k], k % 2, "w" + std::to_string(k), "d");
				u.note_frame(&keys[k], (uint64_t)i * 1000000ULL);
				u.publish_window(&keys[k], (uint32_t)i, 800, (uint64_t)i, 0, 0, 0, -1);
				if (i % 3 == 0)
					u.remove(&keys[k]);
			}
		});
	std::vector<std::string> snaps;
	for (int i = 0; i < 300; i++)
		snaps.push_back(u.json(5000000000ULL));
	for (auto &x : th)
		x.join();
	for (auto &s : snaps)
		printf("C %s\n", s.c_str());
	/* the process-wide table: one instance across translation units */
	printf("shared %d\n", (void *)&cb_jxs_state() == second_tu_table() ? 1 : 0);
	return 0;
}
"#;

const TABLE_SECOND_TU: &str = r#"
#include "jxs-state-table.h"
void *second_tu_table() { return &cb_jxs_state(); }
"#;

fn table_output() -> Vec<String> {
    let out = build_and_run(
        "table",
        &[
            ("driver.cpp", TABLE_DRIVER.to_string()),
            ("second.cpp", TABLE_SECOND_TU.to_string()),
        ],
        &[],
        &[repo("vendor/distroav/src")],
    );
    out.lines().map(str::to_string).collect()
}

fn json(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
}

#[test]
fn the_state_table_publishes_rows_and_snapshots_them_as_json() {
    let lines = table_output();
    // empty = an empty list, never an error
    assert_eq!(lines[0], r#"{"v":1,"entries":[]}"#);
    // two rows, no frame yet, sorted input first; the name's quote and backslash escaped
    let j = json(&lines[1]);
    let e = j["entries"].as_array().unwrap();
    assert_eq!(e.len(), 2);
    assert_eq!(e[0]["kind"], "input");
    assert_eq!(e[0]["name"], "JXS cam5 \"src\" \\ end");
    assert_eq!(e[0]["detail"], "UDP 7000, camera 5");
    assert_eq!(e[0]["age_ms"], -1);
    assert_eq!(e[0]["window"], false);
    assert_eq!(e[1]["kind"], "output");
    assert_eq!(e[1]["detail"], "to 10.77.9.204:7100");
    assert_eq!(e[1]["report_age_ms"], -1);
    // a frame + a window: age from the frame, every counter as published
    let j = json(&lines[2]);
    let e = j["entries"].as_array().unwrap();
    assert_eq!(e.len(), 2, "an unknown key must not create a row");
    assert_eq!(e[0]["age_ms"], 500);
    assert_eq!(e[0]["window"], true);
    for (k, v) in [
        ("fps_x100", 3000),
        ("bpp_x100", 798),
        ("lost", 2),
        ("undecodable", 1),
        ("dropped", 3),
        ("errors", 4),
        ("report_age_ms", -1),
    ] {
        assert_eq!(e[0][k], v, "input {k}");
    }
    assert_eq!(e[1]["age_ms"], 100);
    assert_eq!(e[1]["fps_x100"], 2999);
    assert_eq!(e[1]["report_age_ms"], 1500);
    // a clock read before the frame is 0
    let j = json(&lines[3]);
    assert_eq!(j["entries"][0]["age_ms"], 0);
    // rename keeps counters, remove drops, a control character is \u-escaped
    let j = json(&lines[4]);
    let e = j["entries"].as_array().unwrap();
    assert_eq!(e.len(), 2);
    assert_eq!(e[0]["name"], "Cam 5 renamed");
    assert_eq!(e[0]["detail"], "UDP 7001, camera 5");
    assert_eq!(e[0]["lost"], 2);
    assert_eq!(e[1]["name"], "ctl\u{1}x");
    assert!(lines[4].contains("ctl\\u0001x"), "{}", lines[4]);
    assert_eq!(lines[5], "size 2");
    // bytes that are not UTF-8 become '?', real UTF-8 stays; a wrapped counter is clamped to the
    // int64 a JSON parser holds (the frontend's jansson rejects both otherwise)
    let j = json(&lines[6]);
    assert_eq!(j["entries"][0]["name"], "bad?x??y???z?");
    assert_eq!(j["entries"][0]["detail"], "kamera \u{10d}");
    assert_eq!(j["entries"][0]["lost"].as_i64(), Some(i64::MAX));
}

#[test]
fn the_state_table_snapshot_stays_whole_under_concurrent_writers() {
    let lines = table_output();
    let snaps: Vec<&String> = lines.iter().filter(|l| l.starts_with("C ")).collect();
    assert_eq!(snaps.len(), 300);
    for s in snaps {
        let j = json(&s[2..]);
        assert_eq!(j["v"], 1);
        for e in j["entries"].as_array().unwrap() {
            assert!(e["name"].as_str().unwrap().starts_with('w'));
        }
    }
}

#[test]
fn the_source_and_the_output_share_one_process_wide_table() {
    // `inline`, never `static inline`: a static accessor gives each translation unit its own table
    // and the indicator would see only the source's rows (or only the output's)
    let lines = table_output();
    assert_eq!(lines.last().unwrap(), "shared 1");
    let t = squish(&read(TABLE));
    assert!(t.contains("inline cb_jxs_state_table &cb_jxs_state()"));
    assert!(!t.contains("static inline cb_jxs_state_table &cb_jxs_state()"));
    // std only: no OBS call in the table
    for line in read(TABLE)
        .lines()
        .filter(|l| l.trim_start().starts_with("#include"))
    {
        assert!(
            line.contains('<') && !line.contains("<obs") && !line.contains("<util/"),
            "{line}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 2. the frontend report, compiled as shipped

#[derive(Clone)]
struct Row {
    kind: JxsKind,
    name: &'static str,
    detail: &'static str,
    age_ms: i64,
    window: bool,
    fps_x100: u32,
    bpp_x100: u32,
    lost: u64,
    undecodable: u64,
    dropped: u64,
    errors: u64,
    report_age_ms: i64,
}

fn input(name: &'static str) -> Row {
    Row {
        kind: JxsKind::Input,
        name,
        detail: "UDP 7000, camera 5",
        age_ms: 20,
        window: true,
        fps_x100: 3000,
        bpp_x100: 798,
        lost: 0,
        undecodable: 0,
        dropped: 0,
        errors: 0,
        report_age_ms: -1,
    }
}

fn output() -> Row {
    Row {
        kind: JxsKind::Output,
        name: "camera-box JPEG XS program",
        detail: "to 10.77.9.204:7100",
        report_age_ms: 400,
        bpp_x100: 800,
        ..input("x")
    }
}

fn scenarios() -> Vec<(&'static str, Vec<Row>)> {
    vec![
        ("off", vec![]),
        ("ok", vec![input("JXS cam5 src"), output()]),
        (
            "ok-range",
            vec![
                Row {
                    bpp_x100: 600,
                    ..input("a")
                },
                input("b"),
            ],
        ),
        (
            "ok-first-window",
            vec![Row {
                window: false,
                ..input("a")
            }],
        ),
        (
            "output-down",
            vec![
                input("a"),
                Row {
                    age_ms: -1,
                    ..output()
                },
            ],
        ),
        (
            "all-inputs-down",
            vec![
                Row {
                    age_ms: 5000,
                    ..input("a")
                },
                Row {
                    age_ms: -1,
                    ..input("b")
                },
            ],
        ),
        (
            "input-down",
            vec![
                input("a"),
                Row {
                    age_ms: 3001,
                    ..input("b")
                },
            ],
        ),
        (
            "output-no-report",
            vec![
                input("a"),
                Row {
                    report_age_ms: -1,
                    ..output()
                },
            ],
        ),
        (
            "input-lost",
            vec![
                Row {
                    lost: 3,
                    ..input("a")
                },
                input("b"),
            ],
        ),
        (
            "input-short",
            vec![Row {
                fps_x100: 2000,
                ..input("a")
            }],
        ),
    ]
}

fn cpp_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn report_driver() -> String {
    let mut d = String::from(
        "#include \"GenlockJxsReport.hpp\"\n#include <cstdio>\n#include <string>\n\
         static GenlockJxsRow mk(int kind, const char *name, const char *detail, long long age, bool window,\n\
         \tunsigned fps, unsigned bpp, unsigned long long lost, unsigned long long undec, unsigned long long dropped,\n\
         \tunsigned long long errors, long long rep)\n{\n\tGenlockJxsRow r;\n\tr.kind = kind;\n\tr.name = name;\n\
         \tr.detail = detail;\n\tr.age_ms = age;\n\tr.window = window;\n\tr.fps_x100 = fps;\n\tr.bpp_x100 = bpp;\n\
         \tr.lost = lost;\n\tr.undecodable = undec;\n\tr.dropped = dropped;\n\tr.errors = errors;\n\
         \tr.report_age_ms = rep;\n\treturn r;\n}\n\
         static void show(const char *tag, const std::vector<GenlockJxsRow> &rows)\n{\n\
         \tconst GenlockJxsReport r = genlock_jxs_report(rows);\n\tstd::string tip = r.tooltip;\n\
         \tfor (char &ch : tip)\n\t\tif (ch == '\\n')\n\t\t\tch = '|';\n\
         \tprintf(\"%s\\tSTATE %d %d\\n%s\\tLABEL %s\\n%s\\tCOLOUR %s\\n%s\\tOFFENDER %s\\n%s\\tLOG %s\\n%s\\tTIP %s\\n\",\n\
         \t       tag, r.state, r.reason, tag, r.label.c_str(), tag, r.colour.c_str(), tag, r.offender.c_str(), tag,\n\
         \t       r.log_line.c_str(), tag, tip.c_str());\n}\nint main()\n{\n",
    );
    for (tag, rows) in scenarios() {
        d.push_str("\t{\n\t\tstd::vector<GenlockJxsRow> rows;\n");
        for r in rows {
            writeln!(
                d,
                "\t\trows.push_back(mk({}, {}, {}, {}LL, {}, {}u, {}u, {}ULL, {}ULL, {}ULL, {}ULL, {}LL));",
                r.kind as u8,
                cpp_str(r.name),
                cpp_str(r.detail),
                r.age_ms,
                r.window,
                r.fps_x100,
                r.bpp_x100,
                r.lost,
                r.undecodable,
                r.dropped,
                r.errors,
                r.report_age_ms
            )
            .unwrap();
        }
        writeln!(d, "\t\tshow({}, rows);\n\t}}", cpp_str(tag)).unwrap();
    }
    // a table that answered but did not parse
    d.push_str(
        "\t{\n\t\tconst GenlockJxsReport r = genlock_jxs_report(std::vector<GenlockJxsRow>(), true);\n\
         \t\tstd::string tip = r.tooltip;\n\t\tfor (char &ch : tip)\n\t\t\tif (ch == '\\n')\n\t\t\t\tch = '|';\n\
         \t\tprintf(\"unreadable\\tSTATE %d %d\\nunreadable\\tLABEL %s\\nunreadable\\tCOLOUR %s\\nunreadable\\tLOG %s\\nunreadable\\tTIP %s\\n\",\n\
         \t\t       r.state, r.reason, r.label.c_str(), r.colour.c_str(), r.log_line.c_str(), tip.c_str());\n\t}\n",
    );
    // the log change key over a sequence: off, off, ok, ok, input b down, input a down, a down again
    d.push_str(
        "\t{\n\t\tGenlockJxsLogKey key;\n\t\tstd::vector<GenlockJxsRow> ok = {mk(0, \"a\", \"\", 10, true, 3000u, 800u, 0, 0, 0, 0, -1)};\n\
         \t\tstd::vector<GenlockJxsRow> bdown = {mk(0, \"a\", \"\", 10, true, 3000u, 800u, 0, 0, 0, 0, -1), mk(0, \"b\", \"\", -1, false, 0u, 0u, 0, 0, 0, 0, -1)};\n\
         \t\tstd::vector<GenlockJxsRow> adown = {mk(0, \"a\", \"\", -1, false, 0u, 0u, 0, 0, 0, 0, -1), mk(0, \"b\", \"\", 10, true, 3000u, 800u, 0, 0, 0, 0, -1)};\n\
         \t\tconst std::vector<GenlockJxsRow> none;\n\
         \t\tint seq[7] = {genlock_jxs_log_due(key, genlock_jxs_report(none)), genlock_jxs_log_due(key, genlock_jxs_report(none)),\n\
         \t\t\tgenlock_jxs_log_due(key, genlock_jxs_report(ok)), genlock_jxs_log_due(key, genlock_jxs_report(ok)),\n\
         \t\t\tgenlock_jxs_log_due(key, genlock_jxs_report(bdown)), genlock_jxs_log_due(key, genlock_jxs_report(adown)),\n\
         \t\t\tgenlock_jxs_log_due(key, genlock_jxs_report(adown))};\n\
         \t\tprintf(\"logkey\\tDUE %d %d %d %d %d %d %d\\n\", seq[0], seq[1], seq[2], seq[3], seq[4], seq[5], seq[6]);\n\t}\n",
    );
    d.push_str("\treturn 0;\n}\n");
    d
}

#[test]
fn an_unreadable_table_reads_amber_never_off_and_the_log_key_logs_each_change_once() {
    let l = report_lines();
    let f = |tag: &str, key: &str| field(&l, tag, key);
    assert_eq!(f("unreadable", "STATE"), "2 7");
    assert_eq!(
        f("unreadable", "LABEL"),
        "JPEG XS: DEGRADED table unreadable (see the OBS log) (0 in / 0 out)"
    );
    assert_eq!(f("unreadable", "COLOUR"), "#f1c40f");
    assert!(
        f("unreadable", "LOG").starts_with("jxs-state: state=DEGRADED reason=table_unreadable ")
    );
    assert!(f("unreadable", "TIP").contains("did not parse"));
    // a change of state, of reason or of the row behind it logs once; a repeat never does
    assert_eq!(f("logkey", "DUE"), "1 0 1 0 1 1 0");
}

fn report_lines() -> Vec<(String, String, String)> {
    let out = build_and_run(
        "report",
        &[("harness.cpp", report_driver())],
        &[repo(REPORT)],
        &[repo("vendor/obs-studio/frontend/widgets")],
    );
    out.lines()
        .map(|l| {
            let (tag, rest) = l.split_once('\t').unwrap();
            let (key, val) = rest.split_once(' ').unwrap_or((rest, ""));
            (tag.to_string(), key.to_string(), val.to_string())
        })
        .collect()
}

fn field(lines: &[(String, String, String)], tag: &str, key: &str) -> String {
    lines
        .iter()
        .find(|(t, k, _)| t == tag && k == key)
        .unwrap_or_else(|| panic!("{tag} {key} missing"))
        .2
        .clone()
}

#[test]
fn the_report_grades_every_scenario_like_the_rust_authority() {
    let lines = report_lines();
    for (tag, rows) in scenarios() {
        let mut c = JxsCounts::default();
        for r in &rows {
            let h = entry_health(&JxsEntryFacets {
                kind: r.kind,
                age_ms: r.age_ms,
                window: r.window,
                fps_x100: r.fps_x100,
                lost: r.lost,
                undecodable: r.undecodable,
                dropped: r.dropped,
                errors: r.errors,
                report_age_ms: r.report_age_ms,
            });
            let (n, down, degraded) = match r.kind {
                JxsKind::Input => (
                    &mut c.n_inputs,
                    &mut c.n_inputs_down,
                    &mut c.n_inputs_degraded,
                ),
                JxsKind::Output => (
                    &mut c.n_outputs,
                    &mut c.n_outputs_down,
                    &mut c.n_outputs_degraded,
                ),
            };
            *n += 1;
            *down += i32::from(h == JxsHealth::Down);
            *degraded += i32::from(h == JxsHealth::Degraded);
        }
        let (s, r) = decide(&c);
        assert_eq!(
            field(&lines, tag, "STATE"),
            format!("{} {}", s as u8, r as u8),
            "scenario {tag}"
        );
    }
}

#[test]
fn the_report_label_colour_offender_and_log_line() {
    let l = report_lines();
    let f = |tag: &str, key: &str| field(&l, tag, key);
    assert_eq!(f("off", "LABEL"), "JPEG XS: off");
    assert_eq!(f("off", "COLOUR"), "#95a5a6");
    assert_eq!(
        f("off", "LOG"),
        "jxs-state: state=OFF reason=no_jxs inputs=0 down=0 degraded=0 outputs=0 down=0 degraded=0 entry=- (issue 1387)"
    );
    assert!(f("off", "TIP").contains("no JPEG XS source and no running JPEG XS output"));

    assert_eq!(f("ok", "LABEL"), "JPEG XS: 1 in / 1 out, 8 bpp");
    assert_eq!(f("ok", "COLOUR"), "#2ecc71");
    let tip = f("ok", "TIP");
    assert!(tip.starts_with("JPEG XS: OK|  in 'JXS cam5 src' (UDP 7000, camera 5): last frame 0.0 s ago, 30.00 fps, 7.98 bpp, lost slices 0, undecodable 0, dropped 0, errors 0 -- OK"), "{tip}");
    assert!(tip.contains("|  out 'camera-box JPEG XS program' (to 10.77.9.204:7100): last frame 0.0 s ago, 30.00 fps, 8.00 bpp, dropped 0, errors 0, receiver report 0.4 s ago -- OK"), "{tip}");
    assert_eq!(f("ok-range", "LABEL"), "JPEG XS: 2 in / 0 out, 6-8 bpp");
    // no window yet: no bpp to show, never "0 bpp"
    assert_eq!(f("ok-first-window", "LABEL"), "JPEG XS: 1 in / 0 out");
    assert!(f("ok-first-window", "TIP").contains("no 5 s window yet -- OK"));

    assert_eq!(
        f("output-down", "LABEL"),
        "JPEG XS: DOWN program output sends nothing: camera-box JPEG XS program (1 in / 1 out)"
    );
    assert_eq!(f("output-down", "COLOUR"), "#e74c3c");
    assert_eq!(
        f("output-down", "LOG"),
        "jxs-state: state=DOWN reason=output_down inputs=1 down=0 degraded=0 outputs=1 down=1 degraded=0 entry='camera-box JPEG XS program' (issue 1387)"
    );
    assert!(f("output-down", "TIP").contains("no frame yet"));
    assert_eq!(f("all-inputs-down", "OFFENDER"), "a");
    assert_eq!(f("all-inputs-down", "COLOUR"), "#e74c3c");
    assert_eq!(f("input-down", "OFFENDER"), "b");
    assert_eq!(f("input-down", "COLOUR"), "#f1c40f");
    assert_eq!(
        f("input-down", "LABEL"),
        "JPEG XS: DEGRADED input receives nothing: b (2 in / 0 out)"
    );
    assert_eq!(
        f("output-no-report", "OFFENDER"),
        "camera-box JPEG XS program"
    );
    assert!(f("output-no-report", "TIP").contains("no receiver report -- DEGRADED"));
    assert_eq!(f("input-lost", "OFFENDER"), "a");
    assert_eq!(
        f("input-lost", "LABEL"),
        "JPEG XS: DEGRADED input losing: a (2 in / 0 out)"
    );
    assert_eq!(f("input-short", "OFFENDER"), "a");
}

#[test]
fn the_report_and_the_state_header_stay_free_of_qt_and_obs() {
    for file in [REPORT, REPORT_HPP] {
        for line in read(file)
            .lines()
            .filter(|l| l.trim_start().starts_with("#include"))
        {
            assert!(
                !line.contains("<obs") && !line.contains("<Q") && !line.contains("\"OBSBasic"),
                "{file}: `{line}` -- the report must stay plain std C++ (this gate runs it)"
            );
        }
    }
    // the C decision header goes into the .cpp only, so the widget header's includers see std types
    assert!(!read(REPORT_HPP).contains("#include \"GenlockJxsState.hpp\""));
    assert!(read(REPORT).contains("#include \"GenlockJxsState.hpp\""));
}

// ---------------------------------------------------------------------------------------------
// 3. anchors

fn assert_in(file: &str, needle: &str) {
    let s = squish(&read(file));
    assert!(
        s.contains(needle),
        "{file}: issue-1387 anchor `{needle}` is GONE"
    );
}

#[test]
fn the_plugin_fills_the_table_from_the_source_and_the_output() {
    let src = squish(&read(SOURCE));
    for needle in [
        "#include \"jxs-state-table.h\"",
        "cb_jxs_state().set_entry(s, CB_JXS_STATE_INPUT, obs_source_get_name(s->source), detail);",
        "last_frame_ns = now; cb_jxs_state().note_frame(s, now);",
        "cb_jxs_state().publish_window(s, (uint32_t)(fps * 100.0 + 0.5), bpp_x100, now.lost_slices - prev.lost_slices, undecodable, now.dropped_unread - prev.dropped_unread, now.report_errors - prev.report_errors, -1);",
        "jxs_set_connected(s, false); cb_jxs_state().remove(s); }",
        "proc_handler_add(obs_get_proc_handler(), \"void camera_box_jxs_state(out string json)\", jxs_state_proc, nullptr);",
        "const std::string json = cb_jxs_state().json(os_gettime_ns()); calldata_set_string(cd, \"json\", json.c_str());",
    ] {
        assert!(src.contains(needle), "jxs-source.cpp lacks `{needle}`");
    }
    // the row exists from the thread start (a source no sender reaches reads DOWN, not absent)
    let start = src.find("os_set_thread_name(\"jxs-source\");").unwrap();
    let first_entry = src[start..]
        .find("jxs_state_entry(s, jxs_read_config(s, &gen0));")
        .unwrap();
    assert!(first_entry < src[start..].find("while (s->running.load()) {").unwrap());

    let out = squish(&read(OUTPUT));
    for needle in [
        "#include \"jxs-state-table.h\"",
        "obs_output_set_genlock_wall_stamping(o->output, true); /* the JPEG XS indicator's row (issue 1387): published only while the output runs */ cb_jxs_state().set_entry(o, CB_JXS_STATE_OUTPUT, name, \"to \" + target);",
        "o->taken++; cb_jxs_state().note_frame(o, os_gettime_ns());",
        "cb_jxs_state().publish_window(o, (uint32_t)(fps * 100.0 + 0.5), st.bpp * 100u, 0, 0, st.dropped_busy, st.send_errors + st.encode_errors, st.report_age_s < 0.0 ? -1 : (int64_t)(st.report_age_s * 1000.0));",
        "cbjxs_send_close(tx); cb_jxs_state().remove(o);",
    ] {
        assert!(out.contains(needle), "jxs-output.cpp lacks `{needle}`");
    }
    // the window is published before raw_video's counters reset
    assert!(
        out.find("cb_jxs_state().publish_window(o,").unwrap()
            < out
                .find("o->offered = o->taken = o->not_taken = o->refused = o->stamp_dup = 0; }")
                .unwrap()
    );

    let main = squish(&read(PLUGIN_MAIN));
    assert!(main.contains("void camera_box_jxs_state_register(); #endif"));
    assert!(main.contains(
        "Plugin features registered: camera-box JPEG XS program output (issue 1387)\"); camera_box_jxs_state_register(); #endif"
    ));
    assert!(squish(&read(DISTROAV_CMAKE))
        .contains("src/genlock-emit-timecode.h src/jxs-state-table.h)"));
}

#[test]
fn the_status_bar_shows_the_widget_beside_the_lock_label() {
    assert_in(STATUSBAR, "#include \"GenlockJxsWidget.hpp\"");
    // right after the LOCK label's timer, at the end of the constructor (the next permanent widget)
    let sb = squish(&read(STATUSBAR));
    let lock = sb.find("genlockTimer->start(1000);").unwrap();
    let jxs = sb
        .find("*/ jxsWidget = new GenlockJxsWidget(this); addPermanentWidget(jxsWidget, 0); }")
        .expect("the JPEG XS widget, added as the last statement of the status bar constructor");
    assert!(
        lock < jxs && sb[lock..jxs].matches(';').count() == 1,
        "nothing between the LOCK timer and the JPEG XS widget"
    );
    assert_eq!(sb.matches("new GenlockJxsWidget(").count(), 1);
    assert_in(STATUSBAR_HPP, "GenlockJxsWidget *jxsWidget = nullptr;");
    assert_in(
        "vendor/obs-studio/frontend/cmake/ui-widgets.cmake",
        "widgets/GenlockJxsReport.cpp widgets/GenlockJxsReport.hpp widgets/GenlockJxsWidget.cpp widgets/GenlockJxsWidget.hpp widgets/GenlockRecentEvents.cpp",
    );
    // its own always-on 1 Hz timer, never the stream-only refreshTimer
    assert_in(WIDGET, "timer->start(1000);");
    assert!(!read(WIDGET).contains("refreshTimer"));
    // the widget needs no moc (no Q_OBJECT, a lambda connect)
    assert!(!read(WIDGET_HPP).contains("Q_OBJECT"));
    assert_in(
        WIDGET,
        "connect(timer, &QTimer::timeout, this, [this]() { Tick(); });",
    );
    // no procedure = OFF, never an error: the read's result is not a failure path
    assert_in(
        WIDGET,
        "const bool have = proc_handler_call(obs_get_proc_handler(), \"camera_box_jxs_state\", &cd);",
    );
    assert_in(WIDGET, "calldata_free(&cd);");
    // one jxs-state: line per change, never per tick
    let w = squish(&read(WIDGET));
    assert!(w.contains(
        "if (genlock_jxs_log_due(logKey, rep)) blog(LOG_INFO, \"%s\", rep.log_line.c_str());"
    ));
    assert_eq!(w.matches("blog(").count(), 1);
    // a table that answered but did not parse is never OFF (a failed parse is an EMPTY object)
    assert!(w.contains("if (!obs_data_has_user_value(table, \"v\")) {"));
    assert!(w.contains("return have ? GenlockJxsRead::Unreadable : GenlockJxsRead::None;"));
}

/// The JSON keys of `src` a given pattern names: `prefix"KEY"`.
fn keys(src: &str, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(i) = rest.find(prefix) {
        rest = &rest[i + prefix.len()..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
        }
    }
    out
}

#[test]
fn the_widget_reads_exactly_the_keys_the_table_writes() {
    let table = read(TABLE);
    let widget = read(WIDGET);
    let mut read_keys = BTreeSet::new();
    for getter in [
        "obs_data_get_int(e, \"",
        "obs_data_get_bool(e, \"",
        "obs_data_get_string(e, \"",
    ] {
        read_keys.extend(keys(&widget, getter));
    }
    read_keys.extend(keys(&widget, "obs_data_get_array(table, \""));
    let want: BTreeSet<String> = [
        "age_ms",
        "bpp_x100",
        "detail",
        "dropped",
        "entries",
        "errors",
        "fps_x100",
        "kind",
        "lost",
        "name",
        "report_age_ms",
        "undecodable",
        "window",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(read_keys, want, "the keys the widget reads");
    for k in &want {
        assert!(
            table.contains(&format!("\\\"{k}\\\":")),
            "jxs-state-table.h never writes `{k}`"
        );
    }
    // ... and the table writes no key the widget leaves unread (v is the schema version)
    let mut written = BTreeSet::new();
    let mut rest = table.as_str();
    while let Some(i) = rest.find("\\\":") {
        let head = &rest[..i];
        if let Some(q) = head.rfind("\\\"") {
            written.insert(head[q + 2..].to_string());
        }
        rest = &rest[i + 3..];
    }
    written.remove("v");
    assert_eq!(written, want, "the keys jxs-state-table.h writes");
}

#[test]
fn the_sources_dock_tags_every_ndi_and_jpeg_xs_source_by_kind() {
    let t = squish(&read(TREE));
    for needle in [
        "#include <widgets/GenlockJxsState.hpp>",
        "if (const char *transport = genlock_jxs_transport_tag(sourceId)) { transportLabel = new QLabel(transport);",
        "boxLayout->addWidget(label); if (transportLabel) boxLayout->addWidget(transportLabel); boxLayout->addWidget(vis);",
        "label->setEnabled(visible); if (transportLabel) transportLabel->setEnabled(visible);",
    ] {
        assert!(t.contains(needle), "SourceTreeItem.cpp lacks `{needle}`");
    }
    // by KIND (the unversioned id read for the item), never the name: sourceId is that id
    assert!(t.contains("const char *sourceId = obs_source_get_unversioned_id(source);"));
    assert!(
        t.find("const char *sourceId = obs_source_get_unversioned_id(source);")
            .unwrap()
            < t.find("genlock_jxs_transport_tag(sourceId)").unwrap()
    );
    // the name label stays the name: the rename editor starts from its text
    assert!(t.contains("editor = new QLineEdit(label->text());"));
    assert!(!t.contains("label->setText("));
    assert_in(TREE_HPP, "QLabel *transportLabel = nullptr;");
}

#[test]
fn the_jxs_state_log_family_collides_with_no_other_marker() {
    let new = ["jxs-state:", "jxs-state-proc:"];
    let existing = [
        "jxs-source '",
        "jxs-output '",
        "jxs-output: ",
        "jxs send: ",
        "jxs recv: ",
        "genlock-lock:",
        "genlock-lock-json:",
        "genlock-fifo audit '",
        "genlock-ndi-output audit '",
        "genlock-wall-step:",
        "multiview-audit:",
        "program-render-audit:",
    ];
    for n in new {
        for m in existing.iter().chain(new.iter().filter(|x| **x != n)) {
            assert!(
                !n.contains(m) && !m.contains(n),
                "`{n}` collides with `{m}`"
            );
        }
    }
    assert!(read(REPORT).contains("\"jxs-state: state=%s reason=%s"));
    assert!(read(SOURCE).contains("\"jxs-state-proc: camera_box_jxs_state registered"));
}

// ---------------------------------------------------------------------------------------------
// 4. the pwsh mirrors

/// Every anchor the windows-genlock pwsh step checks (file, squished needle): the step must carry
/// each one, and each one must hold in the tree.
const PWSH_ANCHORS: &[(&str, &str)] = &[
    ("widgets/OBSBasicStatusBar.cpp", "#include \"GenlockJxsWidget.hpp\""),
    ("widgets/OBSBasicStatusBar.cpp", "jxsWidget = new GenlockJxsWidget(this); addPermanentWidget(jxsWidget, 0);"),
    ("widgets/GenlockJxsWidget.cpp", "proc_handler_call(obs_get_proc_handler(), \"camera_box_jxs_state\", &cd);"),
    ("widgets/GenlockJxsWidget.cpp", "const GenlockJxsReport rep = genlock_jxs_report(rows, read == GenlockJxsRead::Unreadable);"),
    ("widgets/GenlockJxsWidget.cpp", "blog(LOG_INFO, \"%s\", rep.log_line.c_str());"),
    ("widgets/GenlockJxsReport.cpp", "const genlock_jxs_state_t state = genlock_jxs_decide(&c, &reason);"),
    ("widgets/GenlockJxsReport.cpp", "\"jxs-state: state=%s reason=%s inputs=%d down=%d degraded=%d outputs=%d down=%d degraded=%d entry=\""),
    ("widgets/GenlockJxsState.hpp", "static inline genlock_jxs_state_t genlock_jxs_decide(const genlock_jxs_counts_t *c, genlock_jxs_reason_t *reason)"),
    ("widgets/GenlockJxsState.hpp", "static inline const char *genlock_jxs_transport_tag(const char *unversioned_id)"),
    ("cmake/ui-widgets.cmake", "widgets/GenlockJxsReport.cpp widgets/GenlockJxsReport.hpp widgets/GenlockJxsWidget.cpp widgets/GenlockJxsWidget.hpp"),
    ("components/SourceTreeItem.cpp", "#include <widgets/GenlockJxsState.hpp>"),
    ("components/SourceTreeItem.cpp", "if (const char *transport = genlock_jxs_transport_tag(sourceId)) {"),
    ("components/SourceTreeItem.cpp", "boxLayout->addWidget(label); if (transportLabel) boxLayout->addWidget(transportLabel);"),
];
const PWSH_PLUGIN_ANCHOR: (&str, &str) = (
    "vendor/distroav/src/jxs-source.cpp",
    "proc_handler_add(obs_get_proc_handler(), \"void camera_box_jxs_state(out string json)\", jxs_state_proc, nullptr);",
);

fn all_pwsh_anchors() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = PWSH_ANCHORS
        .iter()
        .map(|(f, n)| (format!("{FE}{f}"), n.to_string()))
        .collect();
    v.push((
        PWSH_PLUGIN_ANCHOR.0.to_string(),
        PWSH_PLUGIN_ANCHOR.1.to_string(),
    ));
    v
}

#[test]
fn every_pwsh_anchor_holds_in_the_tree() {
    for (file, needle) in all_pwsh_anchors() {
        assert_in(&file, &needle);
    }
}

#[test]
fn both_windows_genlock_ymls_carry_the_issue_1387_step_with_every_anchor() {
    for yml in WIN_YMLS {
        let text = read(yml);
        let name =
            "- name: Assert the JPEG XS indicator + per-source transport tag present (issue 1387)";
        assert_eq!(text.matches(name).count(), 1, "{yml}: the issue-1387 step");
        let step_start = text.find(name).unwrap();
        let step =
            &text[step_start..step_start + text[step_start..].find("\n      - name:").unwrap()];
        for (file, needle) in all_pwsh_anchors() {
            let row = format!("@('{file}', '{needle}')");
            assert!(
                step.contains(&row),
                "{yml}: the issue-1387 step lacks `{row}`"
            );
        }
        // the check squishes like the Rust guard and fails the job on a miss
        assert!(step.contains("$text = (Get-Content $c[0] -Raw) -replace '\\s+', ' '"));
        assert!(step.contains("if ($text -notmatch [regex]::Escape($c[1])) {"));
        assert!(step.contains("exit 1"));
    }
}
