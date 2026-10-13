//! issue 1424 part 2 -- two fixed HDMI outputs at once (owner ROZHODNUTE 13.10.2026, answer "1"; main
//! design comment 6150029770). strih-pp drives HDMI-1 = the Program + its audio for the vestibule
//! (Blackmagic HDMI-to-SDI) AND HDMI-0 = the cutter's Multiview, simultaneously.
//!
//! Shape pinned here:
//! - the config grammar: the single-object form (one output, issue 1152 / 1346, imag + strih-snv
//!   byte-unchanged) or an optional `"outputs": [{connector, view, backend, argb}, ...]` array (max 2),
//!   all or nothing: a malformed array starts NO output and names why. It is read with jansson
//!   directly in a NEW TU `obs-drm-output-outputs.c` (obs_data's loader drops a non-object array
//!   member silently and would start half a pair). Facet B lifts the marked pure block VERBATIM and
//!   compiles it against the REAL jansson over `tests/fixtures/drm_output_outputs_parity.tsv`, the
//!   table the Python mirror `strih_scenes.drm_output_instances` is pinned to as well;
//! - the module's global state is PER INSTANCE: the lease backend's `g_lease[]`, the vk-direct OBS
//!   side's `g_vkd[]`, the vk core split into ONE shared `g_drm_vk_dev` (one VkDevice) + a display slot
//!   per output `g_drm_vk_disp[]`, the view + present clock + audio leg inside `struct drm_output_inst`;
//! - an output that fails to start never stops the other one;
//! - the Tools menu: one Program / Multiview pair per connector, persisting into that output's entry.
//!
//! Std-only, runs offline: `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021
//! tests/drm_output_outputs_1424.rs -o /tmp/t && /tmp/t`. Facet B needs a C compiler and the jansson
//! headers + library (libjansson-dev; the CI test jobs install it) and FAILS, never skips, without them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const DRM_H: &str = "vendor/obs-studio/libobs/obs-drm-output.h";
const INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-internal.h";
const OUTPUTS_C: &str = "vendor/obs-studio/libobs/obs-drm-output-outputs.c";
const LEASE_C: &str = "vendor/obs-studio/libobs/obs-drm-output.c";
const VIEW_C: &str = "vendor/obs-studio/libobs/obs-drm-output-view.c";
const BACKEND_C: &str = "vendor/obs-studio/libobs/obs-drm-output-backend.c";
const VK_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.h";
const VK_INTERNAL_H: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h";
const VK_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk.c";
const VK_SETUP_C: &str = "vendor/obs-studio/libobs/obs-drm-output-vk-setup.c";
const AUDIO_H: &str = "vendor/obs-studio/libobs/obs-drm-output-audio.h";
const AUDIO_C: &str = "vendor/obs-studio/libobs/obs-drm-output-audio.c";
const LINUX_CMAKE: &str = "vendor/obs-studio/libobs/cmake/os-linux.cmake";
const FE_VIEW_CPP: &str = "vendor/obs-studio/frontend/components/DrmOutputView.cpp";
const RIG_HARNESS: &str = "tests/c/drm_output_vk_rig_harness.c";
const PARITY_TSV: &str = "tests/fixtures/drm_output_outputs_parity.tsv";

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1424: cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to one space so anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of the C function whose definition starts with `sig` (up to its closing `\n}\n`).
fn body_of(src: &str, sig: &str, what: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("issue 1424: {what}: `{sig}` not found"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1424: {what}: `{sig}` has no closing `\\n}}\\n`"));
    src[start..end].to_string()
}

// ----------------------------------------------------------------------------------------------
// Facet A -- the per-output API, the per-instance state, the shared VkDevice, the frontend
// ----------------------------------------------------------------------------------------------

#[test]
fn header_declares_the_per_output_api_with_two_outputs() {
    let h = squish(&file(DRM_H));
    for token in [
        "#define OBS_DRM_OUTPUT_MAX_OUTPUTS 2",
        "EXPORT int obs_drm_output_count(void);",
        "EXPORT const char *obs_drm_output_connector(int idx);",
        "EXPORT bool obs_drm_output_active_of(int idx);",
        "EXPORT void obs_drm_output_set_view_renderer_of(int idx, obs_drm_output_view_render_t render, void *param);",
        "EXPORT enum obs_drm_output_view obs_drm_output_get_view_of(int idx);",
        "EXPORT bool obs_drm_output_set_view_of(int idx, enum obs_drm_output_view view);",
        // the issue-1346 single-output forms stay (output 0)
        "EXPORT bool obs_drm_output_set_view(enum obs_drm_output_view view);",
        "EXPORT enum obs_drm_output_view obs_drm_output_get_view(void);",
    ] {
        assert!(h.contains(token), "issue 1424: {DRM_H} must declare `{token}`");
    }
}

#[test]
fn outputs_tu_is_linux_only_built_by_libobs_and_reads_the_config_with_jansson() {
    let o = file(OUTPUTS_C);
    assert!(
        o.contains("#if defined(__linux__)")
            && o.trim_end().ends_with("#endif /* defined(__linux__) */"),
        "issue 1424: {OUTPUTS_C} must be wholly under #if defined(__linux__)"
    );
    let cmake = squish(&file(LINUX_CMAKE));
    assert!(
        cmake.contains("obs-drm-output-outputs.c"),
        "issue 1424: {LINUX_CMAKE} must add obs-drm-output-outputs.c to libobs"
    );
    assert!(
        o.contains("#include <jansson.h>") && o.contains("json_load_file(path, 0, &err)"),
        "issue 1424: the autostart reads the config with jansson directly"
    );
    assert!(
        !o.contains("obs_data_create_from_json_file"),
        "issue 1424: obs_data's JSON loader drops a non-object \"outputs\" member silently -- a \
         malformed array must never start half a pair"
    );
    let lease = file(LEASE_C);
    assert!(
        !lease.contains("void obs_drm_output_maybe_autostart(void)"),
        "issue 1424: the autostart moved to {OUTPUTS_C} (one config grammar)"
    );
}

#[test]
fn every_output_owns_its_state_no_module_singleton_is_left() {
    let i = squish(&file(INTERNAL_H));
    for token in [
        "#define DRM_OUTPUT_MAX_OUTPUTS OBS_DRM_OUTPUT_MAX_OUTPUTS",
        "struct drm_output_inst {",
        "char connector[DRM_OUTPUT_CONNECTOR_MAX];",
        "struct drm_output_view_state view;",
        "struct drm_output_present_clock clock;",
        "struct drm_output_audio *audio;",
        "const struct drm_output_backend_ops *ops;",
        "int drm_output_view_frame(struct drm_output_inst *inst);",
        "int drm_output_claim_render_buf(struct drm_output_inst *inst);",
        "void drm_output_publish_render_buf(struct drm_output_inst *inst, int idx);",
        "bool drm_output_blit_raw(struct drm_output_inst *inst, gs_texture_t *src, int idx);",
    ] {
        assert!(
            i.contains(token),
            "issue 1424: {INTERNAL_H} must declare `{token}`"
        );
    }
    let lease = file(LEASE_C);
    assert!(
        !lease.contains("g_drm.")
            && lease.contains("static struct drm_output_lease g_lease[DRM_OUTPUT_MAX_OUTPUTS]"),
        "issue 1424: the lease backend keeps one state per output (g_lease[]), no g_drm singleton"
    );
    let b = file(BACKEND_C);
    assert!(
        !b.contains("g_vkd.")
            && b.contains("static struct drm_output_vkd g_vkd[DRM_OUTPUT_MAX_OUTPUTS]"),
        "issue 1424: the vk-direct OBS side keeps one state per output (g_vkd[])"
    );
    let v = file(VIEW_C);
    assert!(
        !v.contains("g_view"),
        "issue 1424: the view state lives in each instance (struct drm_output_view_state)"
    );
    for f in [VK_C, VK_SETUP_C, VK_INTERNAL_H] {
        assert!(
            !file(f).contains("g_drm_vk."),
            "issue 1424: {f} must not use the retired single g_drm_vk state"
        );
    }
    let a = file(AUDIO_C);
    assert!(
        !a.contains("drm_output_present_clock_main")
            && !file(AUDIO_H).contains("drm_output_present_clock_main"),
        "issue 1424: each output has its own present clock (struct drm_output_inst::clock)"
    );
    assert!(
        a.contains("os_atomic_load_long(a->view) == OBS_DRM_OUTPUT_VIEW_PROGRAM")
            && !a.contains("obs_drm_output_get_view()"),
        "issue 1424: an audio leg plays while ITS OWN output shows the Program (the Multiview output mutes \
         its leg, the Program output does not)"
    );
}

#[test]
fn vk_core_is_one_shared_device_with_a_display_per_output() {
    let h = squish(&file(VK_H));
    for token in [
        "#define DRM_OUTPUT_VK_MAX_DISPLAYS 2",
        "struct drm_output_vk_disp *drm_output_vk_open(const char *output_name, uint32_t solid_argb, drm_output_vk_shown_fn shown, void *shown_param);",
        "void drm_output_vk_close(struct drm_output_vk_disp *d);",
        "int drm_output_vk_claim(struct drm_output_vk_disp *d);",
    ] {
        assert!(h.contains(token), "issue 1424: {VK_H} must declare `{token}`");
    }
    let i = squish(&file(VK_INTERNAL_H));
    for token in [
        "struct drm_output_vk_device {",
        "VkQueue queues[DRM_OUTPUT_VK_MAX_DISPLAYS];",
        "pthread_mutex_t queue_lock[DRM_OUTPUT_VK_MAX_DISPLAYS];",
        "struct drm_output_vk_disp {",
        "extern struct drm_output_vk_device g_drm_vk_dev;",
        "extern struct drm_output_vk_disp g_drm_vk_disp[DRM_OUTPUT_VK_MAX_DISPLAYS];",
        "X(vkQueueWaitIdle)",
    ] {
        assert!(
            i.contains(token),
            "issue 1424: {VK_INTERNAL_H} must declare `{token}`"
        );
    }
    // A queue two displays may share is only touched under its host-access lock.
    let c = file(VK_C);
    let thread = body_of(
        &c,
        "static void *drm_output_vk_present_thread(void *arg)",
        "present thread",
    );
    let lock = thread
        .find("pthread_mutex_lock(d->queue_lock);")
        .expect("submit locks the queue");
    let submit = thread.find("vkQueueSubmit(d->queue").expect("submit");
    let present = thread.find("vkQueuePresentKHR(d->queue").expect("present");
    let unlock = thread[present..]
        .find("pthread_mutex_unlock(d->queue_lock);")
        .map(|x| present + x)
        .expect("unlock after the present");
    assert!(
        lock < submit && submit < present && present < unlock,
        "issue 1424: lock -> vkQueueSubmit -> vkQueuePresentKHR -> unlock on the display's queue"
    );
    assert!(
        thread.contains("d->shown(d->shown_param, src, os_gettime_ns());")
            && !c.contains("drm_output_present_note_shown("),
        "issue 1424: the vk core reports the present to the OWNING output (a callback), no global clock"
    );
    let s = file(VK_SETUP_C);
    assert!(
        s.contains(".queueCount = d->dev->n_queues,"),
        "issue 1424: the shared device asks one queue per display slot (the family permitting)"
    );
    assert!(
        s.contains("is driven by another GPU than the open output(s)"),
        "issue 1424: a second output on another GPU is refused by name (one shared VkDevice)"
    );
    assert!(
        s.contains("if (drm_output_vk_plane_taken(d, (int)p))"),
        "issue 1424: each display presents on a display plane of its own"
    );
    let destroy = body_of(
        &s,
        "void drm_output_vk_destroy_display(struct drm_output_vk_disp *d)",
        "destroy",
    );
    assert!(
        destroy.contains("vkQueueWaitIdle(d->queue)") && !destroy.contains("vkDeviceWaitIdle"),
        "issue 1424: closing ONE display waits its own queue, never the device another display uses"
    );
    assert!(
        destroy.contains("if (dev->refs == 0)\n\t\tdrm_output_vk_destroy_device(dev);"),
        "issue 1424: the shared device goes with the LAST display"
    );
}

#[test]
fn the_autostart_is_all_or_none_and_one_output_never_stops_the_other() {
    let o = file(OUTPUTS_C);
    let auto = body_of(&o, "void obs_drm_output_maybe_autostart(void)", "autostart");
    assert!(
        auto.contains("\"outputs\\\" is malformed (%s at entry %zu) — NO output \"")
            && auto.contains("\"started (all or none)\""),
        "issue 1424: a malformed array logs ONE named line and starts nothing"
    );
    let loop_at = auto
        .find("/* Every configured output starts on its own: one that fails never stops the other. */")
        .expect("the start loop");
    let tail = &auto[loop_at..];
    let end = tail
        .find("pthread_mutex_unlock(&g_outs_lock);")
        .expect("loop end");
    let start_loop = &tail[..end];
    assert!(
        start_loop.contains("drm_output_inst_start_locked(insts[i])")
            && !start_loop.contains("return")
            && !start_loop.contains("break"),
        "issue 1424: every configured output is started; a failed one neither returns nor breaks the loop"
    );
    let stop = body_of(
        &o,
        "static void drm_output_inst_stop_locked(struct drm_output_inst *inst)",
        "stop",
    );
    let audio = stop
        .find("drm_output_audio_close(inst->audio);")
        .expect("audio first");
    let backend = stop.find("inst->ops->stop(inst);").expect("backend stop");
    assert!(
        audio < backend,
        "issue 1424: an output's audio leg closes before its scanout"
    );
    let start = body_of(
        &o,
        "static bool drm_output_inst_start_locked(struct drm_output_inst *inst)",
        "start",
    );
    assert!(
        start.contains("drm_output_audio_open(inst->connector, edid, edid_len, &inst->clock, &inst->view.view)"),
        "issue 1424: each output opens ITS OWN audio leg on ITS OWN clock and view"
    );
}

#[test]
fn every_output_s_lines_name_its_connector() {
    let lease = file(LEASE_C);
    assert!(
        lease.contains(
            "\"drm-output: program scanout LIVE (Program buffer on the leased CRTC of '%s')\""
        ),
        "issue 1424: verify grades each output by its own `program scanout LIVE` line"
    );
    let v = file(VIEW_C);
    assert!(
        v.contains("\"scanout buffer of '%s')\""),
        "issue 1424: the `multiview bind LIVE` line names its output"
    );
    let c = file(VK_C);
    assert!(
        c.contains("\"drm-output: vk-direct present loop exited on '%s' after %llu presents"),
        "issue 1424: a dead present loop names its output (verify's present-dead per output)"
    );
    assert!(
        c.contains(
            "\"drm-output: program scanout LIVE (vk-direct: a published frame reached '%s')\""
        ),
        "issue 1346: the vk-direct LIVE line already names the output"
    );
}

#[test]
fn the_tools_menu_has_a_view_pair_per_connector() {
    let f = file(FE_VIEW_CPP);
    for token in [
        "DrmOutputSlot drmSlots[OBS_DRM_OUTPUT_MAX_OUTPUTS];",
        "const int count = obs_drm_output_count();",
        "obs_drm_output_set_view_of(idx, view);",
        "obs_drm_output_set_view_renderer_of(slot.index, RenderDrmMultiview, &slot);",
        "obs_drm_output_set_view_renderer_of(slot.index, nullptr, nullptr);",
        // a one-output box keeps today's two labels; several outputs name the connector
        "return QString(\"HDMI výstup: %1\").arg(view);",
        "return QString(\"HDMI výstup %1: %2\")",
    ] {
        assert!(
            f.contains(token),
            "issue 1424: {FE_VIEW_CPP} must contain `{token}`"
        );
    }
    // The renderer of one output is cleared BEFORE its Multiview is freed.
    let clear = body_of(&f, "void ClearDrmMultiview(DrmOutputSlot &slot)", "clear");
    let unreg = clear
        .find("obs_drm_output_set_view_renderer_of(")
        .expect("unregister");
    let del = clear.find("delete slot.multiview;").expect("delete");
    assert!(
        unreg < del,
        "issue 1424: unregister the renderer before deleting the Multiview"
    );
}

#[test]
fn the_rig_harness_drives_two_outputs_on_one_device() {
    let h = file(RIG_HARNESS);
    assert!(
        h.contains("DISPLAY=:0 ./vk-rig-harness HDMI-0 HDMI-1")
            && h.contains("disp[n_disp] = drm_output_vk_open(argv[a], 0x202020u, NULL, NULL);"),
        "issue 1424: the on-box harness proves both displays on ONE shared VkDevice"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B -- the pure config grammar, lifted VERBATIM and compiled against the real jansson
// ----------------------------------------------------------------------------------------------

/// The marked pure block of obs-drm-output-outputs.c.
fn grammar_block() -> String {
    let o = file(OUTPUTS_C);
    let begin = o
        .find("/* ---- BEGIN pure config grammar")
        .expect("issue 1424: the BEGIN marker of the pure config grammar");
    let end_marker = "/* ---- END pure config grammar ---- */";
    let end = o.find(end_marker).expect("issue 1424: the END marker") + end_marker.len();
    o[begin..end].to_string()
}

struct Row {
    name: String,
    config: String,
    verdict: String,
    bad: Option<usize>,
    outputs: String,
}

fn parity_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for line in file(PARITY_TSV).lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            cols.len(),
            5,
            "issue 1424: a parity row has 5 TAB-separated columns: {line}"
        );
        rows.push(Row {
            name: cols[0].to_string(),
            config: if cols[1] == "<empty>" {
                String::new()
            } else {
                cols[1].to_string()
            },
            verdict: cols[2].to_string(),
            bad: if cols[3] == "-" {
                None
            } else {
                Some(cols[3].parse().expect("bad index"))
            },
            outputs: cols[4].to_string(),
        });
    }
    assert!(rows.len() >= 40, "issue 1424: the parity table lost rows");
    rows
}

/// A C string literal of `s` (backslash + double quote escaped; the table holds no control bytes).
fn c_lit(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn compile_and_run(src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "drm_output_outputs_1424-{}-{}",
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
            "-Wno-unused-function",
        ])
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .arg("-ljansson")
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1424: no C compiler ({cc}): {e} -- the parity gate cannot run")
        });
    if !out.status.success() {
        let _ = fs::remove_dir_all(&dir);
    }
    assert!(
        out.status.success(),
        "issue 1424: the lifted config grammar must compile cleanly against jansson (libjansson-dev):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run harness");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "issue 1424: harness failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout).expect("utf8")
}

fn harness_prelude() -> String {
    let view = body_of(
        &file(VIEW_C),
        "static int drm_output_parse_view(const char *s)",
        "view grammar",
    );
    let backend = body_of(
        &file(BACKEND_C),
        "static int drm_output_parse_backend(const char *s)",
        "backend grammar",
    );
    format!(
        "#include <jansson.h>\n#include <stdbool.h>\n#include <stddef.h>\n#include <stdint.h>\n\
         #include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n\n\
         {view}\n{backend}\n\
         int drm_output_view_from_config(const char *value);\n\
         int drm_output_view_from_config(const char *value) {{ return drm_output_parse_view(value); }}\n\
         int drm_output_backend_from_config(const char *value);\n\
         int drm_output_backend_from_config(const char *value) {{ return drm_output_parse_backend(value); }}\n\n\
         {}\n",
        grammar_block()
    )
}

#[test]
fn c_config_grammar_matches_the_shared_parity_table() {
    let rows = parity_rows();
    let mut table = String::new();
    for r in &rows {
        table.push_str(&format!("\t{},\n", c_lit(&r.config)));
    }
    let src = format!(
        "{}\n\
         static const char *const configs[] = {{\n{table}}};\n\
         static const char *const view_names[] = {{\"program\", \"multiview\"}};\n\
         static const char *const backend_names[] = {{\"lease\", \"vk-direct\"}};\n\
         int main(void)\n{{\n\
         \tfor (size_t r = 0; r < sizeof(configs) / sizeof(configs[0]); r++) {{\n\
         \t\tjson_error_t err;\n\
         \t\tjson_t *root = json_loads(configs[r], 0, &err);\n\
         \t\tstruct drm_output_spec specs[2];\n\
         \t\tsize_t n = 99, bad = 99;\n\
         \t\tbool program = false;\n\
         \t\tconst int v = drm_output_config_classify(root, specs, 2, &n, &bad, &program);\n\
         \t\tprintf(\"%s\\t%zu\\t\", drm_output_cfg_verdict_name(v), bad);\n\
         \t\tif (v != DRM_OUTPUT_CFG_OK || n == 0)\n\t\t\tprintf(\"-\");\n\
         \t\tfor (size_t i = 0; v == DRM_OUTPUT_CFG_OK && i < n; i++)\n\
         \t\t\tprintf(\"%s%s|%s|%s|%u\", i ? \";\" : \"\", specs[i].connector, view_names[specs[i].view],\n\
         \t\t\t       backend_names[specs[i].backend], (unsigned)specs[i].argb);\n\
         \t\tprintf(\"\\n\");\n\
         \t\tjson_decref(root);\n\
         \t}}\n\
         \treturn 0;\n}}\n",
        harness_prelude()
    );
    let out = compile_and_run(&src);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        rows.len(),
        "issue 1424: one harness line per parity row"
    );
    for (r, line) in rows.iter().zip(lines) {
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(cols[0], r.verdict, "issue 1424: row `{}`: verdict", r.name);
        assert_eq!(cols[2], r.outputs, "issue 1424: row `{}`: outputs", r.name);
        if let Some(bad) = r.bad {
            assert_eq!(
                cols[1],
                bad.to_string(),
                "issue 1424: row `{}`: the named entry",
                r.name
            );
        }
    }
}

#[test]
fn c_set_view_rewrites_only_that_output_s_view_on_one_compact_line() {
    // (config, connector, view) -> the expected compact line, or NOTFOUND
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "{\"enabled\":true,\"connector\":\"HDMI-0\",\"argb\":2105376,\"view\":\"multiview\",\"backend\":\"vk-direct\"}",
            "HDMI-0",
            "program",
            "{\"enabled\":true,\"connector\":\"HDMI-0\",\"argb\":2105376,\"view\":\"program\",\"backend\":\"vk-direct\"}",
        ),
        (
            "{\"enabled\":true,\"connector\":\"HDMI-1\",\"argb\":2105376}",
            "HDMI-1",
            "multiview",
            "{\"enabled\":true,\"connector\":\"HDMI-1\",\"argb\":2105376,\"view\":\"multiview\"}",
        ),
        (
            "{\"enabled\":true,\"connector\":\"HDMI-1\"}",
            "HDMI-0",
            "multiview",
            "NOTFOUND",
        ),
        (
            "{\"enabled\":true,\"outputs\":[{\"connector\":\"HDMI-0\",\"argb\":2105376,\"view\":\"multiview\",\"backend\":\"vk-direct\"},{\"connector\":\"HDMI-1\",\"argb\":2105376,\"view\":\"program\",\"backend\":\"vk-direct\"}]}",
            "HDMI-1",
            "multiview",
            "{\"enabled\":true,\"outputs\":[{\"connector\":\"HDMI-0\",\"argb\":2105376,\"view\":\"multiview\",\"backend\":\"vk-direct\"},{\"connector\":\"HDMI-1\",\"argb\":2105376,\"view\":\"multiview\",\"backend\":\"vk-direct\"}]}",
        ),
        (
            "{\"enabled\":true,\"outputs\":[{\"connector\":\"HDMI-0\"},{\"connector\":\"HDMI-1\"}]}",
            "HDMI-0",
            "multiview",
            "{\"enabled\":true,\"outputs\":[{\"connector\":\"HDMI-0\",\"view\":\"multiview\"},{\"connector\":\"HDMI-1\"}]}",
        ),
        (
            "{\"enabled\":true,\"outputs\":[{\"connector\":\"HDMI-0\"}]}",
            "DP-2",
            "program",
            "NOTFOUND",
        ),
    ];
    let mut table = String::new();
    for (cfg, conn, view, _) in cases {
        table.push_str(&format!(
            "\t{{{}, {}, {}}},\n",
            c_lit(cfg),
            c_lit(conn),
            c_lit(view)
        ));
    }
    let src = format!(
        "{}\n\
         static const char *const cases[][3] = {{\n{table}}};\n\
         int main(void)\n{{\n\
         \tfor (size_t c = 0; c < sizeof(cases) / sizeof(cases[0]); c++) {{\n\
         \t\tjson_error_t err;\n\
         \t\tjson_t *root = json_loads(cases[c][0], 0, &err);\n\
         \t\tif (!drm_output_config_set_view(root, cases[c][1], cases[c][2])) {{\n\
         \t\t\tprintf(\"NOTFOUND\\n\");\n\
         \t\t}} else {{\n\
         \t\t\tchar *s = json_dumps(root, JSON_COMPACT | JSON_PRESERVE_ORDER);\n\
         \t\t\tprintf(\"%s\\n\", s);\n\
         \t\t\tfree(s);\n\
         \t\t}}\n\
         \t\tjson_decref(root);\n\
         \t}}\n\
         \treturn 0;\n}}\n",
        harness_prelude()
    );
    let out = compile_and_run(&src);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), cases.len());
    for ((cfg, conn, view, want), got) in cases.iter().zip(lines) {
        assert_eq!(
            got, *want,
            "issue 1424: set view {view} on {conn} in {cfg} must touch only that output's \"view\""
        );
    }
    // The persist path dumps exactly this way (one compact line, the key order kept).
    let o = file(OUTPUTS_C);
    assert!(
        o.contains("json_dumps(root, JSON_COMPACT | JSON_PRESERVE_ORDER)")
            && o.contains(
                "os_quick_write_utf8_file_safe(g_config_path, line, len + 1, false, \"tmp\","
            ),
        "issue 1424: the view is persisted as one compact line through an atomic temp-file rename"
    );
}
