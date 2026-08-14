//! #846 -- `scripts/imag-gpu-contention-sampler.sh` (#674) hard-required `nvidia-smi`
//! (`command -v nvidia-smi || FATAL`) and sampled NVENC encoder-session-count + dGPU VRAM-used.
//! The imag box is now Intel iGPU-only (i915, #816) -- so the script FATALs on the hardware the
//! rig actually runs. Same "incumbent NVIDIA box" class as issues 845/847/849.
//!
//! Resolution: RETIRE, not port (full reasoning in the #846 design comment). The script was a
//! one-shot diagnostic for ONE hypothesis -- "GPU/encode contention builds up during imag's own
//! recording" (#674) -- and that hypothesis was already REJECTED live (util/VRAM/encoder-sessions
//! flat, zero growth, no correlation with judder density). The hardware it measured no longer
//! exists (no NVENC/dGPU VRAM on an iGPU), and the REAL cause of imag render degradation was later
//! found and is continuously monitored (#1040 MMIO RAPL PL1 power clamp: thermald purged, envelope
//! pinned + supervised, gt_act_freq_mhz sampled continuously with dev1-side alerting). Porting a
//! spent, unwired diagnostic for a disproven hypothesis onto hardware where the real cause is
//! already fixed is exactly the "just in case" dead code the MVP rule bans.
//!
//! These guards keep the obsolete nvidia-smi-hardcoded tool from being silently resurrected, and
//! keep the LIVE doc pointers from dangling at a deleted script.

use std::fs;
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const SAMPLER_REL: &str = "scripts/imag-gpu-contention-sampler.sh";
const SAMPLER_BASENAME: &str = "imag-gpu-contention-sampler.sh";

#[test]
fn obsolete_sampler_script_is_removed() {
    let p = repo().join(SAMPLER_REL);
    assert!(
        !p.exists(),
        "#846: the obsolete nvidia-smi-hardcoded sampler must be DELETED (retired, not ported): \
         {} still exists",
        p.display()
    );
}

/// No RUNNABLE surface (a script, a CI workflow, a sourced lib) may reference the deleted sampler.
/// (Scoped to executable surfaces -- historical docs/autopilot-log.md entries are HISTORY and are
/// deliberately left intact; this test's own path is excluded so it does not self-match.)
#[test]
fn no_runnable_surface_references_the_deleted_sampler() {
    let dirs = ["scripts", "scripts/lib", ".github/workflows"];
    let mut hits = Vec::new();
    for dir in dirs {
        let d = repo().join(dir);
        if !d.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_file() {
                continue;
            }
            let ext_ok = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("sh") | Some("yml") | Some("yaml")
            );
            if !ext_ok {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                if text.contains(SAMPLER_BASENAME) {
                    hits.push(path.display().to_string());
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "#846: no runnable surface may reference the deleted sampler, found: {hits:?}"
    );
}

/// The two LIVE doc pointers (the e2e skill + the imag-nb-provisioning rule) may keep the
/// historical context, but if they still name the sampler they MUST mark it retired -- so a reader
/// can never follow a dangling "run this committed script" instruction.
#[test]
fn live_docs_mark_the_sampler_retired_where_they_still_name_it() {
    for rel in [
        ".claude/skills/e2e/SKILL.md",
        ".claude/rules/imag-nb-provisioning.md",
    ] {
        let p = repo().join(rel);
        let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        for line in text.lines() {
            if line.contains(SAMPLER_BASENAME) {
                let lc = line.to_lowercase();
                assert!(
                    lc.contains("retired") || lc.contains("#846"),
                    "#846: {} still names the sampler on a line that does not mark it retired / \
                     cite #846 (a dangling instruction to run a deleted script): {line:?}",
                    p.display()
                );
            }
        }
    }
}

/// Sanity: the retirement did not accidentally leave the old script under a renamed path either.
#[test]
fn no_i915_ported_clone_was_smuggled_in() {
    let scripts = repo().join("scripts");
    for entry in fs::read_dir(&scripts).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(
            !name.contains("gpu-contention") && !name.contains("gpu-sampler"),
            "#846: retirement means no ported/renamed contention-sampler clone: found {}",
            path.display()
        );
    }
}
