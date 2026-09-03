//! camera-box #1260 lever (1) — committed guard for the multiview-audit cell-name sanitizer.
//!
//! `obs_audit_copy_cell_name()` (vendor/obs-studio/libobs/obs-display.c) is the SINGLE point that
//! makes an operator-controlled scene name safe for the whitespace-tokenized, pure-ASCII
//! `multiview-audit:` line — every byte outside printable ASCII (`<= ' '`, `>= 0x7f`, `=`, `:`) is
//! replaced by `_`, bounded to 63 chars + NUL. It is the FIRST operator-controlled string ever put
//! on that line, and the downstream read path assumes pure ASCII (`scripts/lib/mv-fps-health.sh`
//! does a LOSSLESS high-byte strip; the #1258/#1262 byte-safety harness documents the invariant) —
//! an un-clamped high byte or a torn multibyte tail at the 63-char cap would make the whole OBS log
//! invalid UTF-8 for `mv-fps-gate`'s `read_to_string`, and a Unicode-whitespace byte (NBSP `C2 A0`)
//! would slip past a `<= ' '` test yet still split under Rust's `split_whitespace()`.
//!
//! The function is a self-contained C `static` (only `size_t`), so this test EXTRACTS it verbatim
//! from the committed source, compiles it with a tiny harness under
//! `-Wall -Wextra -Wconversion -Wformat=2`, and asserts the sanitization + NULL + 63-byte-cap +
//! multibyte behavior — the REAL committed function, not a copy. The full frontend+libobs compile
//! is CI-only; this pure-logic gate runs in the fast Linux CI (and locally via `cc`).

use std::path::PathBuf;
use std::process::Command;

fn manifest(rel: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), rel].iter().collect()
}

#[test]
fn obs_audit_copy_cell_name_sanitizes_to_ascii_and_bounds() {
    let src = std::fs::read_to_string(manifest("vendor/obs-studio/libobs/obs-display.c"))
        .expect("read obs-display.c");
    let sig = "static void obs_audit_copy_cell_name(char *dst, size_t dst_cap, const char *src)";
    let start = src
        .find(sig)
        .expect("#1260 obs_audit_copy_cell_name definition present in obs-display.c");
    // The function's inner `if (!src) {` block closes with a TAB-indented `}`, while the function's
    // own closing brace is at column 0 (`\n}`), so the first `\n}` after the signature ends exactly
    // the function — slice it verbatim.
    let after = &src[start..];
    let end = after
        .find("\n}")
        .expect("#1260 closing brace of obs_audit_copy_cell_name")
        + "\n}".len();
    let func = &after[..end];

    // The ASCII-only clamp is load-bearing: a `c == 0x7f`-only clamp would let high bytes through.
    assert!(
        func.contains("c >= 0x7f"),
        "#1260: the sanitizer must clamp every byte >= 0x7f to '_' (pure-ASCII invariant); found:\n{func}"
    );

    let harness = format!(
        concat!(
            "#include <stddef.h>\n#include <string.h>\n#include <stdio.h>\n",
            "{func}\n",
            "static int failed;\n",
            "static void chk(const char *in, const char *want) {{\n",
            "  char b[64];\n",
            "  obs_audit_copy_cell_name(b, sizeof(b), in);\n",
            "  if (strcmp(b, want) != 0) {{ printf(\"MISMATCH got=[%s] want=[%s]\\n\", b, want); failed = 1; }}\n",
            "}}\n",
            "int main(void) {{\n",
            "  chk(\"NDI cam1\", \"NDI_cam1\");\n",
            "  chk(\"CG bridge\", \"CG_bridge\");\n",
            "  chk(\"a=b:c\\td\", \"a_b_c_d\");\n",
            "  chk(\"preview\", \"preview\");\n",
            "  chk(\"\\xC2\\xA0x\", \"__x\");            /* NBSP (2 bytes) -> two underscores */\n",
            "  chk(\"\\x7f\", \"_\");                     /* DEL */\n",
            "  chk(\"\\xe2\\x9c\\x93ok\", \"___ok\");     /* U+2713 (3 bytes) -> 3 underscores, no torn tail */\n",
            "  {{ char b[64]; obs_audit_copy_cell_name(b, sizeof(b), NULL); if (b[0] != '\\0') {{ printf(\"NULL not empty\\n\"); failed = 1; }} }}\n",
            "  {{ char b[64]; char big[100]; memset(big, 'X', 99); big[99] = '\\0'; obs_audit_copy_cell_name(b, sizeof(b), big); if (strlen(b) != 63) {{ printf(\"cap not 63: %zu\\n\", strlen(b)); failed = 1; }} }}\n",
            "  {{ char t[1]; obs_audit_copy_cell_name(t, sizeof(t), \"abc\"); if (t[0] != '\\0') {{ printf(\"cap1 not empty\\n\"); failed = 1; }} }}\n",
            "  if (!failed) printf(\"ALL PASS\\n\");\n",
            "  return failed;\n",
            "}}\n"
        ),
        func = func
    );

    let dir = std::env::temp_dir();
    let c_path = dir.join(format!("mv_cellname_1260_{}.c", std::process::id()));
    let bin_path = dir.join(format!("mv_cellname_1260_{}", std::process::id()));
    std::fs::write(&c_path, &harness).expect("write sanitizer harness .c");
    let compile = Command::new("cc")
        .args(["-Wall", "-Wextra", "-Wconversion", "-Wformat=2", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&bin_path)
        .output()
        .expect("spawn cc (install build-essential) — the #1260 sanitizer gate needs a C compiler");
    let _ = std::fs::remove_file(&c_path);
    assert!(
        compile.status.success(),
        "#1260 sanitizer must compile clean (-Wall -Wextra -Wconversion -Wformat=2):\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&bin_path)
        .output()
        .expect("run the #1260 sanitizer harness");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let _ = std::fs::remove_file(&bin_path);
    assert!(
        run.status.success() && stdout.contains("ALL PASS"),
        "#1260 sanitizer behavior wrong (ASCII clamp / NULL / 63-byte cap / multibyte):\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
