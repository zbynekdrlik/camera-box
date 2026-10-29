//! Issue 1387 -- the JPEG XS indicator's decision: the C port
//! `vendor/obs-studio/frontend/widgets/GenlockJxsState.hpp` against the Rust authority
//! `camera_box::genlock_jxs_state`.
//!
//! The SHIPPED header is compiled whole (as C11 and as C++17, the frontend includes it from C++)
//! into a driver over generated vector tables; every row health, every box decision, every name
//! and every transport tag must equal the Rust side. A compiler missing = FAIL, never a skip.

use camera_box::genlock_jxs_state::{
    decide, entry_health, reason_key, state_name, transport_tag, JxsCounts, JxsEntryFacets,
    JxsKind, JxsReason, JxsState, JXS_DOWN_AGE_MS, JXS_MIN_FPS_X100, JXS_REPORT_MAX_AGE_MS,
};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const HEADER: &str = "vendor/obs-studio/frontend/widgets/GenlockJxsState.hpp";

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

static RUNS: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> tempfile::TempDir {
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    tempfile::Builder::new()
        .prefix(&format!("jxs_state_parity_1387-{tag}-{n}-"))
        .tempdir()
        .unwrap()
}

/// Compile `driver` (which `#include`s the shipped header) with `compiler` and run it; its stdout.
fn compile_and_run(compiler: &str, lang_args: &[&str], ext: &str, driver: &str) -> String {
    let dir = scratch(ext);
    let src = dir.path().join(format!("driver.{ext}"));
    let bin = dir.path().join("driver.bin");
    fs::write(&src, driver).unwrap();
    let out = Command::new(compiler)
        .args(lang_args)
        .args(["-Wall", "-Wextra", "-Werror", "-O1"])
        .arg("-I")
        .arg(repo("vendor/obs-studio/frontend/widgets"))
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("{compiler}: cannot run ({e}); this gate must FAIL, never skip")
        });
    assert!(
        out.status.success(),
        "{compiler} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().unwrap();
    assert!(run.status.success(), "driver failed");
    String::from_utf8(run.stdout).unwrap()
}

fn entry_vectors() -> Vec<JxsEntryFacets> {
    let mut v = Vec::new();
    for kind in [JxsKind::Input, JxsKind::Output] {
        for age_ms in [
            -5,
            -1,
            0,
            1,
            JXS_DOWN_AGE_MS - 1,
            JXS_DOWN_AGE_MS,
            JXS_DOWN_AGE_MS + 1,
            60_000,
        ] {
            for window in [false, true] {
                for fps_x100 in [0, JXS_MIN_FPS_X100 - 1, JXS_MIN_FPS_X100, 3000] {
                    for bits in 0u32..16 {
                        for report_age_ms in
                            [-1, 0, JXS_REPORT_MAX_AGE_MS, JXS_REPORT_MAX_AGE_MS + 1]
                        {
                            v.push(JxsEntryFacets {
                                kind,
                                age_ms,
                                window,
                                fps_x100,
                                lost: u64::from(bits & 1),
                                undecodable: u64::from((bits >> 1) & 1) * 7,
                                dropped: u64::from((bits >> 2) & 1) * u64::MAX,
                                errors: u64::from((bits >> 3) & 1),
                                report_age_ms,
                            });
                        }
                    }
                }
            }
        }
    }
    v
}

fn count_vectors() -> Vec<JxsCounts> {
    let mut v = Vec::new();
    for n_inputs in -1..=3 {
        for n_inputs_down in 0..=4 {
            for n_inputs_degraded in 0..=2 {
                for n_outputs in 0..=2 {
                    for n_outputs_down in 0..=2 {
                        for n_outputs_degraded in 0..=2 {
                            v.push(JxsCounts {
                                n_inputs,
                                n_inputs_down,
                                n_inputs_degraded,
                                n_outputs,
                                n_outputs_down,
                                n_outputs_degraded,
                            });
                        }
                    }
                }
            }
        }
    }
    v
}

fn c_string_literal(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

const IDS: &[&str] = &[
    "ndi_source",
    "camera_box_jxs_source",
    "ffmpeg_source",
    "ndi_filter",
    "ndi_source_v2",
    "camera_box_jxs_output",
    "NDI cam5",
    "",
];

/// The driver: the entry table, the counts table, the names, the tags; one token per line.
fn driver() -> String {
    let mut d = String::new();
    d.push_str("#include <stdio.h>\n#include <stddef.h>\n#include \"GenlockJxsState.hpp\"\n");
    d.push_str("static const genlock_jxs_entry_t entries[] = {\n");
    for e in entry_vectors() {
        writeln!(
            d,
            "\t{{{}, INT64_C({}), {}, {}u, UINT64_C({}), UINT64_C({}), UINT64_C({}), UINT64_C({}), INT64_C({})}},",
            e.kind as u8,
            e.age_ms,
            u8::from(e.window),
            e.fps_x100,
            e.lost,
            e.undecodable,
            e.dropped,
            e.errors,
            e.report_age_ms
        )
        .unwrap();
    }
    d.push_str("};\nstatic const genlock_jxs_counts_t counts[] = {\n");
    for c in count_vectors() {
        writeln!(
            d,
            "\t{{{}, {}, {}, {}, {}, {}}},",
            c.n_inputs,
            c.n_inputs_down,
            c.n_inputs_degraded,
            c.n_outputs,
            c.n_outputs_down,
            c.n_outputs_degraded
        )
        .unwrap();
    }
    d.push_str("};\nstatic const char *ids[] = {\n");
    for id in IDS {
        writeln!(d, "\t{},", c_string_literal(id)).unwrap();
    }
    d.push_str(
        "};\nint main(void)\n{\n\tsize_t i;\n\
         \tfor (i = 0; i < sizeof entries / sizeof entries[0]; i++)\n\
         \t\tprintf(\"E %d\\n\", (int)genlock_jxs_entry_health(&entries[i]));\n\
         \tfor (i = 0; i < sizeof counts / sizeof counts[0]; i++) {\n\
         \t\tgenlock_jxs_reason_t r = GENLOCK_JXS_REASON_NONE;\n\
         \t\tgenlock_jxs_state_t s = genlock_jxs_decide(&counts[i], &r);\n\
         \t\tprintf(\"D %d %d\\n\", (int)s, (int)r);\n\t}\n\
         \tfor (i = 0; i < sizeof ids / sizeof ids[0]; i++) {\n\
         \t\tconst char *t = genlock_jxs_transport_tag(ids[i]);\n\
         \t\tprintf(\"T %s\\n\", t ? t : \"-\");\n\t}\n\
         \tprintf(\"T %s\\n\", genlock_jxs_transport_tag(NULL) ? \"?\" : \"-\");\n\
         \tfor (i = 0; i < 4; i++)\n\
         \t\tprintf(\"S %s\\n\", genlock_jxs_state_name((genlock_jxs_state_t)i));\n\
         \tfor (i = 0; i < 7; i++)\n\
         \t\tprintf(\"R %s\\n\", genlock_jxs_reason_key((genlock_jxs_reason_t)i));\n\
         \tprintf(\"K %lld %u %lld\\n\", (long long)GENLOCK_JXS_DOWN_AGE_MS, (unsigned)GENLOCK_JXS_MIN_FPS_X100, (long long)GENLOCK_JXS_REPORT_MAX_AGE_MS);\n\
         \treturn 0;\n}\n",
    );
    d
}

/// The lines the Rust authority predicts for the same driver.
fn expected() -> Vec<String> {
    let mut out = Vec::new();
    for e in entry_vectors() {
        out.push(format!("E {}", entry_health(&e) as u8));
    }
    for c in count_vectors() {
        let (s, r) = decide(&c);
        out.push(format!("D {} {}", s as u8, r as u8));
    }
    for id in IDS {
        out.push(format!("T {}", transport_tag(id).unwrap_or("-")));
    }
    out.push("T -".to_string());
    for s in [
        JxsState::Off,
        JxsState::Ok,
        JxsState::Degraded,
        JxsState::Down,
    ] {
        out.push(format!("S {}", state_name(s)));
    }
    for r in [
        JxsReason::None,
        JxsReason::NoJxs,
        JxsReason::OutputDown,
        JxsReason::AllInputsDown,
        JxsReason::InputDown,
        JxsReason::OutputDegraded,
        JxsReason::InputDegraded,
    ] {
        out.push(format!("R {}", reason_key(r)));
    }
    out.push(format!(
        "K {JXS_DOWN_AGE_MS} {JXS_MIN_FPS_X100} {JXS_REPORT_MAX_AGE_MS}"
    ));
    out
}

fn check(got: &str, compiler: &str) {
    let got: Vec<&str> = got.lines().collect();
    let want = expected();
    assert_eq!(got.len(), want.len(), "{compiler}: line count");
    let entries = entry_vectors();
    let counts = count_vectors();
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        if g != w {
            let what = if i < entries.len() {
                format!("{:?}", entries[i])
            } else if i < entries.len() + counts.len() {
                format!("{:?}", counts[i - entries.len()])
            } else {
                "a name / tag / constant".to_string()
            };
            panic!("{compiler}: C `{g}` != Rust `{w}` at vector {i}: {what}");
        }
    }
}

#[test]
fn the_c_header_compiled_as_c_matches_the_rust_authority_on_every_vector() {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let got = compile_and_run(&cc, &["-std=c11"], "c", &driver());
    check(&got, "cc -std=c11");
}

#[test]
fn the_c_header_compiled_as_cpp_matches_the_rust_authority_on_every_vector() {
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
    let got = compile_and_run(&cxx, &["-std=c++17", "-x", "c++"], "cpp", &driver());
    check(&got, "c++ -std=c++17");
}

#[test]
fn the_header_stays_pure_c_with_no_qt_or_obs_include() {
    let src = fs::read_to_string(repo(HEADER)).unwrap();
    for line in src
        .lines()
        .filter(|l| l.trim_start().starts_with("#include"))
    {
        assert!(
            line.contains("<stdint.h>") || line.contains("<string.h>"),
            "GenlockJxsState.hpp must stay pure C (the parity gate compiles it as C11): `{line}`"
        );
    }
}

#[test]
fn the_vector_tables_reach_every_state_reason_and_health() {
    // a table that never reaches a branch would prove nothing about it
    let mut healths = std::collections::BTreeSet::new();
    for e in entry_vectors() {
        healths.insert(entry_health(&e) as u8);
    }
    assert_eq!(healths.len(), 3);
    let mut decisions = std::collections::BTreeSet::new();
    for c in count_vectors() {
        let (s, r) = decide(&c);
        decisions.insert((s as u8, r as u8));
    }
    assert_eq!(decisions.len(), 7, "{decisions:?}");
}
