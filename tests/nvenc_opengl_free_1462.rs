//! Issue 1462 -- the NVENC texture encoder leaked ~154 MiB of GPU memory per StartRecord/StopRecord
//! on the Linux stream-snv (RTX 4060), ~50 cycles to CUDA_ERROR_OUT_OF_MEMORY.
//!
//! The OpenGL path registers libobs's encode textures with `cuGraphicsGLRegisterImage` while
//! `cuda_opengl_encode` holds the graphics context (`obs_enter_graphics`). `nvenc_destroy` runs from
//! `obs_encoder_shutdown` on the output's stop thread with NO GL context current, and
//! `cuda_opengl_free` unregistered them there with every return value ignored. These tests lift the
//! REAL `cuda_opengl_free` out of `nvenc-opengl.c`, compile it against stubbed CUDA + graphics
//! symbols and assert that every unregister runs inside the graphics context and that a failing
//! unregister is reported through the plugin's own `cuda_error_check`.
//!
//! Runs under `cargo test` (scratch dir via the `tempfile` dev-dependency, issue 1409).
//! Fails loudly when no C compiler is present.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NVENC_OPENGL: &str = "vendor/obs-studio/plugins/obs-nvenc/nvenc-opengl.c";
const FREE_SIG: &str = "void cuda_opengl_free(struct nvenc_data *enc)";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A function, by its signature, through its first top-level closing brace.
fn lift_fn(src: &str, signature: &str) -> String {
    assert_eq!(
        src.matches(signature).count(),
        1,
        "issue 1462: `{signature}` must be defined exactly once"
    );
    let a = src.find(signature).unwrap();
    let b = src[a..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("issue 1462: `{signature}` does not end"));
    src[a..a + b + 3].to_string()
}

const HARNESS_HEAD: &str = r#"
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>

typedef int CUresult;
#define CUDA_SUCCESS 0
typedef void *CUcontext;
typedef void *CUgraphicsResource;

struct handle_tex {
	unsigned tex_id;
	CUgraphicsResource res_y;
	CUgraphicsResource res_uv;
};

struct nvenc_data {
	CUcontext cu_ctx;
	struct {
		struct handle_tex *array;
		size_t num;
	} input_textures;
};

static int gfx_depth;
static int ctx_depth;
static int fail_unregister_of = -1; /* index of the resource whose unregister fails */

static void obs_enter_graphics(void) { gfx_depth++; }
static void obs_leave_graphics(void) { gfx_depth--; }

static CUresult stub_push(CUcontext c) { (void)c; ctx_depth++; return CUDA_SUCCESS; }
static CUresult stub_pop(CUcontext *c) { (void)c; ctx_depth--; return CUDA_SUCCESS; }
static CUresult stub_unregister(CUgraphicsResource r)
{
	int idx = (int)(size_t)r;
	printf("UNREG res=%d in_graphics=%d in_ctx=%d\n", idx, gfx_depth > 0, ctx_depth > 0);
	return idx == fail_unregister_of ? 999 : CUDA_SUCCESS;
}

struct cu_fns {
	CUresult (*cuCtxPushCurrent)(CUcontext);
	CUresult (*cuCtxPopCurrent)(CUcontext *);
	CUresult (*cuGraphicsUnregisterResource)(CUgraphicsResource);
};
static struct cu_fns fns = {stub_push, stub_pop, stub_unregister};
static struct cu_fns *cu = &fns;

static bool cuda_error_check(struct nvenc_data *enc, CUresult res, const char *func, const char *call)
{
	(void)enc;
	if (res == CUDA_SUCCESS)
		return true;
	printf("ERROR func=%s call=%s res=%d\n", func, call, res);
	return false;
}
"#;

const HARNESS_MAIN: &str = r#"
int main(void)
{
	struct handle_tex texs[2] = {{1, (void *)1, (void *)2}, {2, (void *)3, (void *)4}};
	struct nvenc_data enc = {(void *)0x10, {texs, 2}};

	printf("CASE ok\n");
	cuda_opengl_free(&enc);
	printf("DEPTH gfx=%d ctx=%d\n", gfx_depth, ctx_depth);

	printf("CASE fail\n");
	fail_unregister_of = 3;
	cuda_opengl_free(&enc);
	printf("DEPTH gfx=%d ctx=%d\n", gfx_depth, ctx_depth);

	printf("CASE noctx\n");
	struct nvenc_data none = {NULL, {texs, 2}};
	int before = gfx_depth;
	cuda_opengl_free(&none);
	printf("NOCTX gfx_touched=%d\n", gfx_depth != before);
	return 0;
}
"#;

fn run_harness() -> Vec<String> {
    let src =
        fs::read_to_string(repo().join(NVENC_OPENGL)).expect("issue 1462: read nvenc-opengl.c");
    let body = lift_fn(&src, FREE_SIG);
    let program = format!("{HARNESS_HEAD}\n{body}\n{HARNESS_MAIN}");
    let scratch = tempfile::tempdir().expect("issue 1462: scratch dir");
    let dir = scratch.path();
    let c = dir.join("harness.c");
    let bin = dir.join("harness");
    fs::write(&c, program).expect("issue 1462: write harness");
    let out = Command::new("cc")
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wno-unused-function",
        ])
        .arg(&c)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("issue 1462: a C compiler (cc) is required -- this gate never skips");
    assert!(
        out.status.success(),
        "issue 1462: the harness does not compile:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1462: run the harness");
    assert!(run.status.success(), "issue 1462: the harness failed");
    String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn case(lines: &[String], name: &str) -> Vec<String> {
    let start = lines
        .iter()
        .position(|l| l == &format!("CASE {name}"))
        .unwrap_or_else(|| panic!("issue 1462: no CASE {name}"));
    lines[start + 1..]
        .iter()
        .take_while(|l| !l.starts_with("CASE "))
        .cloned()
        .collect()
}

#[test]
fn every_unregister_runs_inside_the_graphics_context_1462() {
    let lines = run_harness();
    let ok = case(&lines, "ok");
    let unregs: Vec<&String> = ok.iter().filter(|l| l.starts_with("UNREG ")).collect();
    assert_eq!(
        unregs.len(),
        4,
        "issue 1462: both planes of both textures: {ok:?}"
    );
    for l in &unregs {
        assert!(
            l.contains("in_graphics=1"),
            "issue 1462: a GL-interop unregister ran without the GL context current: {l}"
        );
        assert!(
            l.contains("in_ctx=1"),
            "issue 1462: unregister outside the CUDA context: {l}"
        );
    }
    assert!(
        ok.contains(&"DEPTH gfx=0 ctx=0".to_string()),
        "issue 1462: graphics/CUDA context left unbalanced: {ok:?}"
    );
}

#[test]
fn a_failing_unregister_is_reported_and_the_rest_still_run_1462() {
    let lines = run_harness();
    let fail = case(&lines, "fail");
    assert_eq!(
        fail.iter().filter(|l| l.starts_with("UNREG ")).count(),
        4,
        "issue 1462: one failure must not stop the other unregisters: {fail:?}"
    );
    let errors: Vec<&String> = fail.iter().filter(|l| l.starts_with("ERROR ")).collect();
    assert_eq!(
        errors.len(),
        1,
        "issue 1462: the failing unregister must be logged once: {fail:?}"
    );
    assert!(
        errors[0].contains("cuGraphicsUnregisterResource") && errors[0].contains("res=999"),
        "issue 1462: the error line must name the call and its result: {}",
        errors[0]
    );
    assert!(fail.contains(&"DEPTH gfx=0 ctx=0".to_string()));
}

#[test]
fn no_cuda_context_touches_nothing_1462() {
    let lines = run_harness();
    let noctx = case(&lines, "noctx");
    assert!(noctx.iter().all(|l| !l.starts_with("UNREG ")));
    assert!(noctx.contains(&"NOCTX gfx_touched=0".to_string()));
}
