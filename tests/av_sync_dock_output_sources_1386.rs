//! Issue 1386 — the A/V-sync dock output is split over several files, and every text anchor reads
//! them as ONE source.
//!
//! `vendor/av-sync-dock/src/sync-test-output.cpp` had grown past 2000 lines; it is now
//! `sync-test-output.cpp` (the obs_output_info callbacks, the lifecycle, the registration),
//! `sync-test-output-video.cpp`, `sync-test-output-audio.cpp` and the shared
//! `sync-test-output-internal.hpp`. The Rust anchor tests read the union through
//! `tests/support/av_sync_dock_output.rs`; the pwsh anchor steps of both windows-genlock workflows
//! dot-source its twin `vendor/av-sync-dock/test/dock-output-source.ps1`. These checks pin that
//! wiring:
//! - the union is exactly the output's compiled TUs plus its internal header, never the public
//!   `sync-test-output.hpp`;
//! - every TU wraps its code in the one namespace and includes the internal header;
//! - the pwsh twin reads the same files in the same order, and every dock step goes through it;
//! - no Rust test reads one output file on its own.

use std::collections::BTreeSet;
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "support/av_sync_dock_output.rs"]
mod av_sync_dock_output;

const CMAKE: &str = "vendor/av-sync-dock/CMakeLists.txt";
const PWSH_TWIN: &str = "vendor/av-sync-dock/test/dock-output-source.ps1";
const WORKFLOWS: [&str; 2] = [
    ".github/workflows/windows-genlock.yml",
    ".github/workflows/windows-genlock-fast.yml",
];
const DOT_SOURCE: &str = ". ./vendor/av-sync-dock/test/dock-output-source.ps1";

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn repo_file(rel: &str) -> String {
    let p: PathBuf = [env!("CARGO_MANIFEST_DIR"), rel].iter().collect();
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn union_names() -> Vec<String> {
    av_sync_dock_output::files()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_union_is_the_compiled_output_tus_and_the_internal_header() {
    let names = union_names();
    let compiled: BTreeSet<String> = repo_file(CMAKE)
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("src/"))
        .filter(|n| av_sync_dock_output::is_output_file(n) && n.ends_with(".cpp"))
        .map(str::to_owned)
        .collect();
    let read: BTreeSet<String> = names
        .iter()
        .filter(|n| n.ends_with(".cpp"))
        .cloned()
        .collect();
    assert_eq!(
        read, compiled,
        "{CMAKE} PLUGIN_SOURCES and the anchor union must name the same output TUs (issue 1386): a TU \
         the anchors do not read is a TU no gate guards"
    );
    assert!(
        compiled.len() >= 3,
        "the output is split into several TUs: {compiled:?}"
    );
    assert!(
        names.iter().any(|n| n == "sync-test-output-internal.hpp"),
        "the union must read the internal header (struct sync_test_output lives there): {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "sync-test-output.hpp"),
        "the public sync-test-output.hpp (the dock UI's interface) is not part of the output union"
    );
}

#[test]
fn every_output_tu_includes_the_internal_header_inside_the_one_namespace() {
    for path in av_sync_dock_output::files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".cpp") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let include = src
            .find("#include \"sync-test-output-internal.hpp\"")
            .unwrap_or_else(|| panic!("{name} must include sync-test-output-internal.hpp"));
        let macros = src
            .find("#include \"plugin-macros.generated.h\"")
            .unwrap_or_else(|| panic!("{name} must include plugin-macros.generated.h"));
        let ns = src
            .find("namespace av_sync_output {")
            .unwrap_or_else(|| panic!("{name} must wrap its code in namespace av_sync_output"));
        assert!(
            include < macros && macros < ns,
            "{name}: the internal header, then the plugin macros (blog), then the namespace -- the \
             include order the single-file output had"
        );
    }
}

#[test]
fn the_pwsh_twin_reads_the_same_files_in_the_same_order() {
    let ps = repo_file(PWSH_TWIN);
    // The pwsh file rule, pinned WHOLE: an extra or a dropped clause (a pwsh union that also read the
    // public sync-test-output.hpp, say) fails here until `is_output_file` changes with it.
    const FILTER: &str = "Where-Object { $_ -ceq 'sync-test-output.cpp' -or \
         ($_.StartsWith('sync-test-output-', [StringComparison]::Ordinal) -and \
         ($_.EndsWith('.cpp', [StringComparison]::Ordinal) -or \
         $_.EndsWith('.hpp', [StringComparison]::Ordinal))) }";
    let squished = squish(&ps);
    assert_eq!(
        squished.matches("Where-Object {").count(),
        1,
        "{PWSH_TWIN}: one file filter"
    );
    let start = squished.find("Where-Object {").unwrap();
    let mut depth = 0usize;
    let mut end = None;
    for (off, ch) in squished[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + off + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let filter = &squished[start..end.expect("the file filter's braces balance")];
    assert_eq!(
        filter, FILTER,
        "{PWSH_TWIN}: the pwsh file rule drifted from tests/support/av_sync_dock_output.rs \
         `is_output_file` -- change both together"
    );
    for need in [
        "[Array]::Sort($names, [StringComparer]::Ordinal)",
        "Join-Path $PSScriptRoot '../src'",
        "-replace '\\s+', ' '",
        "function Get-DockBody([string]$src, [string]$sig)",
        "$src.IndexOf($sig, $i + 1, [StringComparison]::Ordinal) -ge 0",
    ] {
        assert!(
            ps.contains(need),
            "{PWSH_TWIN}: `{need}` is gone -- the pwsh twin must read the same files as \
             tests/support/av_sync_dock_output.rs and refuse a signature found twice"
        );
    }
    // the Rust rule the pwsh one mirrors
    for (name, is) in [
        ("sync-test-output.cpp", true),
        ("sync-test-output-audio.cpp", true),
        ("sync-test-output-internal.hpp", true),
        ("sync-test-output.hpp", false),
        ("sync-test-dock.cpp", false),
        ("sync-test-output-audio.cpp.orig", false),
    ] {
        assert_eq!(av_sync_dock_output::is_output_file(name), is, "{name}");
    }
}

#[test]
fn every_dock_step_in_both_workflows_reads_the_union() {
    for wf in WORKFLOWS {
        let text = repo_file(wf);
        // Any spelling of one output file outside a comment (a Get-Content, a
        // [IO.File]::ReadAllText, a quoted path in either quote style) means a step reads it on its
        // own instead of through the helper. The file names are judged by the union's own rule, so
        // the public sync-test-output.hpp and a comment pointer stay allowed.
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            for (at, _) in line.match_indices("sync-test-output") {
                let name: String = line[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                    .collect();
                assert!(
                    !av_sync_dock_output::is_output_file(&name),
                    "{wf}:{}: reads the dock output file {name} on its own ({line:?}) -- a step \
                     must read the union: dot-source {PWSH_TWIN} and use Get-DockOutputSource \
                     (issue 1386)",
                    n + 1
                );
            }
        }
        let mut dock_steps = 0;
        for step in text.split("\n      - name: ") {
            if !(step.contains("Get-DockOutputSource") || step.contains("Get-DockBody")) {
                continue;
            }
            dock_steps += 1;
            let name = step.lines().next().unwrap_or("");
            assert!(
                step.contains(DOT_SOURCE),
                "{wf}: step `{name}` uses the union helper without dot-sourcing {PWSH_TWIN}"
            );
            assert!(
                !step.contains("function Get-Body("),
                "{wf}: step `{name}` carries its own body extractor again -- use Get-DockBody"
            );
        }
        assert!(
            dock_steps >= 6,
            "{wf}: the six dock anchor steps (#942, #1319, both issue-1367 steps, issue 1381, \
             #398) must all read the union; found {dock_steps}"
        );
    }
}

#[test]
fn no_rust_test_reads_one_output_file_on_its_own() {
    let dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests"].iter().collect();
    // Split so this file's own text never matches: a quoted path literal to one output file.
    let prefix = ["\"vendor/av-sync-dock/src/", "sync-test-output"].concat();
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.extension().is_some_and(|e| e == "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (at, _) in text.match_indices(&prefix) {
            let literal = &text[at + 1..];
            let literal = &literal[..literal.find('"').unwrap_or(literal.len())];
            let base = literal.rsplit('/').next().unwrap_or(literal);
            if av_sync_dock_output::is_output_file(base) {
                offenders.push(format!(
                    "{}: {literal}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "issue 1386: these tests read one dock output file on its own instead of \
         av_sync_dock_output::source(): {offenders:?}"
    );
}
