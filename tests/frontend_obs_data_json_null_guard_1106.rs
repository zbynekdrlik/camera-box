//! #1106 — Harden the `obs_data_get_json()` NULL-deref class across the REST of the OBS frontend
//! (a #773 follow-up). `libobs/obs-data.c::obs_data_get_json()` returns NULL when `json_dumps()`
//! fails (an intermittent jansson serialisation/alloc failure on a perfectly valid `obs_data_t`)
//! or for a NULL `obs_data_t`. Constructing a `std::string` from that NULL — directly
//! (`std::string x(obs_data_get_json(w))`), by copy-init (`std::string x = obs_data_get_json(w)`),
//! as a temporary (`std::string(obs_data_get_json(w))`), OR implicitly when passed to a
//! `const std::string &` parameter (`undo_stack::add_action(...)`, `utility/undo_stack.hpp`) — is
//! undefined behaviour (the `c0000005` in `ucrtbase!strcmp`/string-copy that #773 fixed for
//! `OBSBasicProperties`). #773 guarded ONE file; this ticket sweeps the 46 remaining construction
//! sites across 11 files and routes each through a single NULL-safe helper.
//!
//! Fix = a header-only `static inline std::string OBSDataGetJsonSafe(obs_data_t *, const char *)`
//! (`vendor/obs-studio/frontend/utility/obs-data-json-safe.hpp`) that coalesces a NULL result to
//! `""` and `blog(LOG_WARNING, ...)` once, so the undo/redo (or clipboard/drop/transform) action
//! degrades to an empty payload instead of crashing. Every crash-class site becomes a one-line
//! call to it. NULL-tolerant C-API consumers (`obs_data_set_string`, `config_set_string`,
//! `obs_data_create_from_json`, Qt's NULL-safe `QString(const char *)`, a bare discarded call) are
//! NOT the crash class and stay byte-identical.
//!
//! Why this test is std-only + runs offline: camera-box's `# airuleset:build-ok` bypass is disabled
//! and the vendored OBS frontend compiles only on CI, so per
//! `.claude/rules/vendored-obs-frontend-crash-safety.md` (the #773 / #1026 pattern) this file
//! (Facet A) SOURCE-ANCHORS the routed sites with a std-only `fs::read_to_string` guard runnable via
//! `rustc --test` (revert protection against a future `git subtree pull`), and (Facet B) LIFTS the
//! helper VERBATIM, compiles it with the C++ toolchain under `-Werror`, and runs a truth table —
//! proving the SHIPPED bytes COMPILE and COMPUTE the NULL-safe result. Per test-strictness Facet B
//! FAILS LOUD when no compiler is present.
//!
//! Local RED→GREEN (no cargo — the #1026 recipe):
//!   CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 \
//!       tests/frontend_obs_data_json_null_guard_1106.rs -o /tmp/x && /tmp/x

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const HELPER: &str = "vendor/obs-studio/frontend/utility/obs-data-json-safe.hpp";

/// `(relative path, crash-class sites routed through the helper, legitimate NULL-tolerant
/// `obs_data_get_json()` calls that correctly stay raw)`. The two counts are grep-derived ground
/// truth on the pre-fix tree; the fix must make each file's `OBSDataGetJsonSafe(` count equal the
/// first and its remaining raw `obs_data_get_json(` count equal the second.
const CONSUMERS: &[(&str, usize, usize)] = &[
    ("vendor/obs-studio/frontend/widgets/OBSBasic_Dropfiles.cpp", 1, 0),
    ("vendor/obs-studio/frontend/widgets/OBSBasic_Transitions.cpp", 4, 0),
    ("vendor/obs-studio/frontend/widgets/OBSBasic_Preview.cpp", 2, 0),
    ("vendor/obs-studio/frontend/widgets/OBSBasic_Clipboard.cpp", 3, 0),
    ("vendor/obs-studio/frontend/widgets/OBSBasic_SceneItems.cpp", 20, 0),
    ("vendor/obs-studio/frontend/widgets/OBSBasic_Scenes.cpp", 3, 1),
    ("vendor/obs-studio/frontend/widgets/OBSBasicPreview.cpp", 2, 0),
    ("vendor/obs-studio/frontend/dialogs/OBSBasicFilters.cpp", 6, 2),
    ("vendor/obs-studio/frontend/components/SourceToolbar.cpp", 2, 0),
    ("vendor/obs-studio/frontend/dialogs/OBSBasicSourceSelect.cpp", 1, 0),
    ("vendor/obs-studio/frontend/dialogs/OBSBasicTransform.cpp", 2, 0),
];

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection).
// ----------------------------------------------------------------------------------------------

#[test]
fn helper_header_exists_and_is_null_safe_1106() {
    let raw = read(HELPER);
    let sq = squish(&raw);
    assert!(
        sq.contains("std::string OBSDataGetJsonSafe(obs_data_t *data, const char *context)"),
        "{HELPER}: #1106 NULL-safe helper OBSDataGetJsonSafe(obs_data_t *, const char *) is missing \
         — the whole frontend sweep hangs off it. A `git subtree pull` may have dropped the file."
    );
    // It must read obs_data_get_json's result, NULL-check it, and coalesce to "" — the guard must
    // come BEFORE the std::string is built from the (possibly NULL) pointer.
    assert!(
        sq.contains("obs_data_get_json(data)"),
        "#1106: helper must call obs_data_get_json(data)"
    );
    let guard = sq.find("if (!json)").unwrap_or_else(|| {
        panic!("#1106: helper must NULL-check the obs_data_get_json() result (`if (!json)`)")
    });
    assert!(
        sq.contains("blog(LOG_WARNING"),
        "#1106: helper must blog(LOG_WARNING ...) on the NULL path (comprehensive-logging: every \
         error branch logs), so a json_dumps failure leaves a trace in the OBS log"
    );
    let ret = sq.find("return std::string(json ? json :").unwrap_or_else(|| {
        panic!("#1106: helper must `return std::string(json ? json : \"\")` — NULL coalesced to empty")
    });
    assert!(
        guard < ret,
        "#1106: the NULL guard/log must precede the std::string construction in the helper"
    );
}

#[test]
fn every_consumer_routes_crash_class_through_the_helper_1106() {
    for &(path, crash, safe) in CONSUMERS {
        let sq = squish(&read(path));

        // (a) every std::string-from-obs_data_get_json crash-class site now routes through the helper.
        let got_helper = count(&sq, "OBSDataGetJsonSafe(");
        assert_eq!(
            got_helper, crash,
            "{path}: expected {crash} OBSDataGetJsonSafe(...) call(s) — one per std::string-from-\
             obs_data_get_json crash-class site (#1106) — but found {got_helper}."
        );

        // (b) the ONLY raw obs_data_get_json( calls left are the NULL-tolerant C-API / discarded
        //     ones; a leftover crash-class construction (or a churned safe site) fails this.
        let got_raw = count(&sq, "obs_data_get_json(");
        assert_eq!(
            got_raw, safe,
            "{path}: expected {safe} remaining raw obs_data_get_json( call(s) (NULL-tolerant C-API \
             sites only) but found {got_raw} — an un-guarded std::string construction survived, or a \
             safe site was needlessly churned."
        );

        // (c) any file that routes something through the helper must include it.
        if crash > 0 {
            assert!(
                sq.contains("#include <utility/obs-data-json-safe.hpp>"),
                "{path}: missing `#include <utility/obs-data-json-safe.hpp>` for the #1106 helper."
            );
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the helper VERBATIM, compile it standalone under -Werror, run a truth table.
// ----------------------------------------------------------------------------------------------

#[test]
fn helper_lift_compiles_and_computes_the_null_safe_truth_table_1106() {
    let raw = read(HELPER);
    let sig = "static inline std::string OBSDataGetJsonSafe(obs_data_t *data, const char *context)";
    let start = raw
        .find(sig)
        .unwrap_or_else(|| panic!("#1106: {HELPER} no longer defines OBSDataGetJsonSafe — nothing to compile."));
    let end = raw[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1106: OBSDataGetJsonSafe has no closing brace `\\n}\\n`");
    let helper = &raw[start..end];

    // Stub the two externals the helper touches: obs_data_get_json (round-trips the pointer back to
    // the json string, so a NULL pointer models a json_dumps failure) and blog (a no-op).
    let harness = format!(
        "#include <string>\n#include <cstdio>\n#include <cstdarg>\n\
         typedef struct obs_data obs_data_t;\n\
         enum {{ LOG_WARNING = 200 }};\n\
         static const char *obs_data_get_json(obs_data_t *d) {{ return (const char *)d; }}\n\
         static void blog(int level, const char *fmt, ...) {{ (void)level; (void)fmt; }}\n\
         {helper}\n\
         int main(void) {{\n\
         \tchar buf[] = \"abc\";\n\
         \tstd::string n = OBSDataGetJsonSafe((obs_data_t *)0, \"t\");\n\
         \tstd::string s = OBSDataGetJsonSafe((obs_data_t *)buf, \"t\");\n\
         \tprintf(\"%d %s %d\\n\", (int)n.empty(), s.c_str(), (int)(s == \"abc\"));\n\
         \treturn 0;\n\
         }}\n"
    );

    // Key the scratch dir on the pid so concurrent runs (worktree-fleet workers, nextest + a local
    // run) never race on the binary.
    let dir = std::env::temp_dir().join(format!("frontend_json_1106_{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("helper.cpp");
    let bin = dir.join("helper.bin");
    fs::write(&cfile, &harness).expect("write the harness");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let out = Command::new(&cxx)
        .args(["-std=c++17", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1106: could not run the C++ compiler `{cxx}` ({e}). This gate compiles the lifted \
                 OBSDataGetJsonSafe to prove the shipped bytes COMPILE and compute the NULL-safe \
                 result; per test-strictness it must FAIL rather than skip when the toolchain is absent."
            )
        });
    assert!(
        out.status.success(),
        "#1106: the lifted OBSDataGetJsonSafe did not compile under -Werror:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin).output().expect("run the compiled harness");
    assert!(
        run.status.success(),
        "#1106: the harness crashed at runtime (a NULL was dereferenced — the helper is not NULL-safe)."
    );
    let got = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        got.trim(),
        "1 abc 1",
        "#1106: OBSDataGetJsonSafe truth table wrong — a NULL obs_data_get_json() must map to an \
         EMPTY std::string and a non-NULL result to an exact copy. Got: {got:?}"
    );
}
