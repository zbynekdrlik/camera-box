//! Issue 1439 -- the lift-and-run harness of `tests/vban_source_loss_1439.rs`, split out of it when it
//! passed its line budget (round 5): the C driver build + run of the SHIPPED `vban-source.c`, the
//! packet schedules, and the output helpers. Included by `#[path]`; std only.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) const SOURCE: &str = "vendor/obs-vban/src/vban-source.c";
pub(crate) const FRAMES: u32 = 256;
/// `VBAN_BITFMT_*` (vendor/obs-vban/vban/vban.h): 8-bit, 16-bit, 24-bit, 32-bit int, 32-bit float.
pub(crate) const VBAN_BITFMT_8_INT: u8 = 0;
pub(crate) const VBAN_BITFMT_16_INT: u8 = 1;
pub(crate) const VBAN_BITFMT_24_INT: u8 = 2;
pub(crate) const VBAN_BITFMT_32_INT: u8 = 3;
pub(crate) const VBAN_BITFMT_32_FLOAT: u8 = 4;
pub(crate) const RATE: u64 = 48_000;

pub(crate) fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The driver: includes the shipped source by its absolute path, stubs every libobs / UDP call it
/// makes, and replays the packets named on its command line (`nu:now_ns:value`).
pub(crate) const DRIVER: &str = include_str!("vban_source_driver_1439.c");

/// One audio block OBS received.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Out {
    pub(crate) ts: u64,
    pub(crate) frames: u64,
    pub(crate) zero: bool,
    pub(crate) first: i64,
    /// The libobs `enum audio_format` value OBS received.
    pub(crate) format: i32,
    /// When it went out (the driver's clock: round 5, the playout worker's deadline).
    pub(crate) at: u64,
}

static COMPILES: AtomicUsize = AtomicUsize::new(0);

/// A scratch dir named by pid + a per-process counter (std only, no tempfile), removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Compiles the driver against the shipped source (once per call, in a dir of its own) and runs it
/// over the packets `(nuFrame, arrival ns, sample value)`.
pub(crate) fn run(packets: &[(u32, u64, i16)]) -> Vec<Out> {
    run_with_log(packets).0
}

/// [`run`], plus the plugin's log lines (`blog <level>: <text>`).
pub(crate) fn run_with_log(packets: &[(u32, u64, i16)]) -> (Vec<Out>, String) {
    run_as(packets, VBAN_BITFMT_16_INT)
}

/// The packets sent in the VBAN sample format `bitfmt` (the header's `VBAN_BITFMT_*` value).
pub(crate) fn run_as(packets: &[(u32, u64, i16)], bitfmt: u8) -> (Vec<Out>, String) {
    let tokens: Vec<String> = packets
        .iter()
        .map(|(nu, now, value)| format!("{nu}:{now}:{value}"))
        .collect();
    let (stdout, log) = run_tokens(&tokens, bitfmt);
    let outs = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("out "))
        .map(|l| {
            let field = |k: &str| -> &str {
                l.split_whitespace()
                    .find_map(|f| f.strip_prefix(k))
                    .unwrap_or_else(|| panic!("no `{k}` in `{l}`"))
            };
            Out {
                ts: field("ts=").parse().unwrap(),
                frames: field("frames=").parse().unwrap(),
                zero: field("zero=") == "1",
                first: field("first=").parse().unwrap(),
                format: field("fmt=").parse().unwrap(),
                at: field("at=").parse().unwrap(),
            }
        })
        .collect();
    (outs, log)
}

/// Runs the driver over free tokens: packets (`nu:now_ns:value`) and setting changes (`port=N`,
/// `name=S`, `host=H`, `buffer=MS`, each one followed by the source's `update()`). Returns (stdout,
/// the log).
pub(crate) fn run_tokens(tokens: &[String], bitfmt: u8) -> (String, String) {
    run_tokens_frames(tokens, FRAMES, bitfmt)
}

/// [`run_tokens`] with packets of `frames` frames.
pub(crate) fn run_tokens_frames(tokens: &[String], frames: u32, bitfmt: u8) -> (String, String) {
    let source = repo(SOURCE);
    assert!(source.is_file(), "issue 1439: {SOURCE} is missing");
    let n = COMPILES.fetch_add(1, Ordering::SeqCst);
    let scratch = Scratch(
        std::env::temp_dir().join(format!("vban_source_loss_1439-{}-{n}", std::process::id())),
    );
    let dir = &scratch.0;
    let inc = dir.join("inc");
    fs::create_dir_all(&inc).expect("create the harness dir");
    fs::write(
        inc.join("obsconfig.h"),
        "#pragma once\n#define OBS_RELEASE_CANDIDATE 0\n#define OBS_BETA 0\n",
    )
    .expect("write obsconfig.h");
    let macros = fs::read_to_string(repo("vendor/obs-vban/src/plugin-macros.h.in"))
        .expect("read plugin-macros.h.in")
        .replace("@PROJECT_NAME@", "obs-vban")
        .replace("@PROJECT_VERSION@", "0.3.1")
        .replace("@ID_PREFIX@", "net.nagater.obs-vban.");
    fs::write(inc.join("plugin-macros.generated.h"), macros).expect("write plugin macros");
    let driver = dir.join("driver.c");
    let bin = dir.join("driver.bin");
    fs::write(
        &driver,
        DRIVER.replace("@SOURCE@", &source.display().to_string()),
    )
    .expect("write the driver");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-O1",
            "-pthread",
        ])
        .arg("-I")
        .arg(&inc)
        .arg("-I")
        .arg(repo("vendor/obs-studio/libobs"))
        .arg("-I")
        .arg(repo("vendor/obs-vban/vban"))
        .arg("-I")
        .arg(repo("vendor/obs-vban/src"))
        .arg(&driver)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1439: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 shipped {SOURCE}; it must FAIL rather than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1439: {SOURCE} (+ the lift driver) does not compile under -Wall -Wextra -Werror:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut cmd = Command::new(&bin);
    cmd.arg(format!("{frames},{bitfmt}"));
    cmd.args(tokens);
    let run = cmd
        .output()
        .expect("issue 1439: the compiled driver failed to execute");
    assert!(
        run.status.success(),
        "issue 1439: the driver exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let log = String::from_utf8(run.stderr).expect("driver stderr is utf-8");
    (
        String::from_utf8(run.stdout).expect("driver stdout is utf-8"),
        log,
    )
}

/// floor(samples * 1e9 / RATE).
pub(crate) fn ns(samples: u64) -> u64 {
    samples * 1_000_000_000 / RATE
}

/// The packets `nu_frames`, each arriving exactly when its audio is complete on an ideal sender.
pub(crate) fn on_grid(nu_frames: &[u32]) -> Vec<(u32, u64, i16)> {
    let t0 = 1_000_000_000u64;
    let first = nu_frames[0];
    nu_frames
        .iter()
        .map(|&nu| {
            let k = u64::from(nu - first);
            (nu, t0 + ns(k * u64::from(FRAMES)), (nu - first + 1) as i16)
        })
        .collect()
}

/// The packets of an anchor's first second at 256 frames / 48 kHz (packet k ends at (k + 1) x 256):
/// HELD, never output (ROZHODNUTÉ 6345255813 item 1).
pub(crate) const HELD: u32 = 187;

/// [`on_grid`] behind a held first second: the `HELD` packets just before `nu_frames[0]`, on the
/// same grid, carrying the value 999 (they never reach OBS), then `nu_frames` as [`on_grid`] gives
/// them (their values unchanged).
pub(crate) fn after_hold(nu_frames: &[u32]) -> Vec<(u32, u64, i16)> {
    let first = nu_frames[0] - HELD;
    let mut all: Vec<u32> = (first..nu_frames[0]).collect();
    all.extend_from_slice(nu_frames);
    let shown = nu_frames[0];
    on_grid(&all)
        .into_iter()
        .map(|(nu, at, _)| {
            (
                nu,
                at,
                if nu < shown {
                    999
                } else {
                    (nu - shown + 1) as i16
                },
            )
        })
        .collect()
}

pub(crate) fn assert_contiguous(outs: &[Out]) {
    for w in outs.windows(2) {
        let expect = w[0].ts + ns(w[0].frames);
        let d = w[1].ts.abs_diff(expect);
        assert!(
            d <= 2,
            "issue 1439: the stamps are not contiguous: an output at {} after one at {} of {} frames \
             (expected {expect}): {outs:?}",
            w[1].ts,
            w[0].ts,
            w[0].frames
        );
    }
}

/// Review round 5: on the grid a stretch's first output's stamp is R − one packet (34.67 ms)
/// after its arrival, and its LEAD-IN starts the 2 ms margin after the worker's now: 1568 whole
/// frames of silence.
pub(crate) const LEAD_IN: u64 = 1568;

/// Splits off the LEAD-INs (review round 5): the run of silent outputs that opens a stretch (the
/// first output, or one that does not continue the previous output's stamps), which must end
/// exactly where the stretch's next output starts. Returns (each lead-in's frames, the other
/// outputs in order).
pub(crate) fn split_lead_ins(outs: &[Out]) -> (Vec<u64>, Vec<Out>) {
    let (mut leads, mut rest) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < outs.len() {
        let opens = i == 0 || outs[i].ts.abs_diff(outs[i - 1].ts + ns(outs[i - 1].frames)) > 2;
        if !(opens && outs[i].zero) {
            rest.push(outs[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        let mut frames = 0;
        while j < outs.len() && outs[j].zero {
            frames += outs[j].frames;
            j += 1;
        }
        if j < outs.len() {
            let end = outs[j - 1].ts + ns(outs[j - 1].frames);
            assert!(
                end.abs_diff(outs[j].ts) <= 2,
                "issue 1439: a lead-in ends where its stretch's output starts: {outs:?}"
            );
        }
        leads.push(frames);
        i = j;
    }
    (leads, rest)
}

/// The outputs of ONE stretch on the grid after its lead-in, which must be exactly [`LEAD_IN`]
/// frames.
pub(crate) fn after_lead_in(outs: &[Out]) -> Vec<Out> {
    let (leads, rest) = split_lead_ins(outs);
    assert_eq!(
        leads,
        vec![LEAD_IN],
        "issue 1439: one stretch, one lead-in of R − one packet: {outs:?}"
    );
    rest
}

/// The non-silent outputs' sample values, in order.
pub(crate) fn audio_values(outs: &[Out]) -> Vec<i64> {
    outs.iter().filter(|o| !o.zero).map(|o| o.first).collect()
}
