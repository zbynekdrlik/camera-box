//! issue 1317 — `scripts/genlock-runtime-packages.sh` records the apt packages the built genlock
//! bundle links against so `setup-strih.sh` installs them on a fresh Ubuntu 26.04 box (else the
//! bundle dies at exec with the 13-soname `libavcodec.so.62` load failure).
//!
//! Tier-0 (no cargo compile of the appliance): these tests SOURCE the script's guarded pure half
//! `runtime_packages_from_ldd` and RUN the whole `--stage/--out` program with fake `ldd`/`dpkg` on
//! PATH — the same fake-PATH convention as `tests/strih_provision_pure_functions.rs`.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/genlock-runtime-packages.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Run a bash `body` with the script path exported as `$SCRIPT`. Returns (exit_code, stdout, stderr).
fn run(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", script())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A fake `dpkg` that maps two known library paths to packages (writes it into `$fake/dpkg`).
const FAKE_DPKG: &str =
    "printf '%s\\n' '#!/usr/bin/env bash' 'case \"$2\" in */libavcodec.so.62) echo \"libavcodec62:amd64: $2\";; */libc.so.6) echo \"libc6:amd64: $2\";; *) exit 1;; esac' > \"$fake/dpkg\"; chmod +x \"$fake/dpkg\"\n";

/// The pure half maps a resolved SYSTEM library path to its apt package (via `dpkg -S`), DROPS a
/// soname resolved INSIDE the stage tree (bundle-internal), and skips vdso/loader lines.
#[test]
fn runtime_packages_from_ldd_maps_system_and_excludes_bundle_internal() {
    let body = format!(
        "fake=$(mktemp -d)\n{FAKE_DPKG}\
         out=$(PATH=\"$fake:$PATH\" bash -c '. \"$SCRIPT\"; printf \"%s\\n\" \
             \"libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x1)\" \
             \"libavcodec.so.62 => /usr/lib/x86_64-linux-gnu/libavcodec.so.62 (0x2)\" \
             \"libobs.so.30 => /opt/stage/lib/x86_64-linux-gnu/libobs.so.30 (0x3)\" \
             \"linux-vdso.so.1 (0x4)\" | runtime_packages_from_ldd /opt/stage'); rc=$?\n\
         printf '%s' \"$out\"\n\
         rm -rf \"$fake\"; exit $rc"
    );
    let (code, out, err) = run(&body);
    assert_eq!(code, 0, "mapping must succeed; stderr={err}");
    let mut pkgs: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    pkgs.sort_unstable();
    assert_eq!(
        pkgs,
        vec!["libavcodec62", "libc6"],
        "must map the two SYSTEM libs to packages and EXCLUDE the bundle-internal libobs: {out}"
    );
}

/// The pure half FAILS LOUD (non-zero) on any `=> not found` line — a genuinely unresolved dependency
/// is a build problem, not something to silently drop.
#[test]
fn runtime_packages_from_ldd_fails_loud_on_not_found() {
    let (code, _o, err) = run(". \"$SCRIPT\"\n\
         printf '%s\\n' 'libmissing.so.9 => not found' | runtime_packages_from_ldd /opt/stage");
    assert_ne!(code, 0, "a `=> not found` line must fail loud");
    assert!(
        err.contains("UNRESOLVED"),
        "the failure must name the unresolved soname: {err}"
    );
}

/// The whole `--stage/--out` program: with fake `ldd`/`dpkg` on PATH it writes RUNTIME_PACKAGES.txt
/// listing the mapped SYSTEM package and EXCLUDING the bundle-internal soname. The fake `ldd` reads
/// the real stage path from `$STAGE_ABS` (exported into its env) so the exclusion path is genuine.
#[test]
fn program_writes_runtime_packages_file() {
    // Fake ldd: emits one SYSTEM lib, one bundle-internal lib (under $STAGE_ABS), and a vdso line.
    // $STAGE_ABS stays LITERAL in the written file (single-quoted arg) and expands at ldd runtime.
    let fake_ldd = "printf '%s\\n' '#!/usr/bin/env bash' 'printf \"%s\\n\" \"libavcodec.so.62 => /usr/lib/x86_64-linux-gnu/libavcodec.so.62 (0x1)\" \"libobs.so.30 => $STAGE_ABS/lib/x86_64-linux-gnu/libobs.so.30 (0x2)\" \"linux-vdso.so.1 (0x3)\"' > \"$fake/ldd\"; chmod +x \"$fake/ldd\"\n";
    let body = format!(
        "stage=$(mktemp -d); mkdir -p \"$stage/bin\" \"$stage/lib/x86_64-linux-gnu\"\n\
         : > \"$stage/bin/obs\"\n\
         : > \"$stage/lib/x86_64-linux-gnu/libobs.so.30\"\n\
         : > \"$stage/lib/x86_64-linux-gnu/libavcodec.so.62\"\n\
         export STAGE_ABS=$(cd \"$stage\" && pwd)\n\
         fake=$(mktemp -d)\n{fake_ldd}{FAKE_DPKG}\
         outf=\"$stage/RUNTIME_PACKAGES.txt\"\n\
         PATH=\"$fake:$PATH\" bash \"$SCRIPT\" --stage \"$stage\" --out \"$outf\" >/dev/null 2>&1; rc=$?\n\
         echo \"RC=$rc\"\n\
         grep -q '^libavcodec62$' \"$outf\" && echo HAS_AVCODEC || echo NO_AVCODEC\n\
         if grep -q libobs \"$outf\"; then echo HAS_LIBOBS; else echo NO_LIBOBS; fi\n\
         rm -rf \"$stage\" \"$fake\""
    );
    let (code, out, err) = run(&body);
    assert_eq!(code, 0, "harness must run; stderr={err}");
    assert!(out.contains("RC=0"), "the program must exit 0: {out}");
    assert!(
        out.contains("HAS_AVCODEC"),
        "RUNTIME_PACKAGES.txt must list libavcodec62: {out}"
    );
    assert!(
        out.contains("NO_LIBOBS"),
        "the bundle-internal libobs must be excluded: {out}"
    );
}

/// The program requires both `--stage` and `--out`; a missing arg is a usage error (exit 2).
#[test]
fn program_requires_stage_and_out_args() {
    let (code, _o, err) = run(
        "stage=$(mktemp -d); bash \"$SCRIPT\" --stage \"$stage\"; rc=$?; rm -rf \"$stage\"; exit $rc",
    );
    assert_eq!(
        code, 2,
        "a missing --out must be a usage error (exit 2); stderr={err}"
    );
}
