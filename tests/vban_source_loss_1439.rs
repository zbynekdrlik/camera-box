//! Issue 1439 -- a lost VBAN packet must become silence for exactly its own slot.
//!
//! The vendored obs-vban receive source (`vendor/obs-vban/src/vban-source.c`) used to pad a
//! `nuFrame` gap with COPIES of the next packet's audio, and for two or more lost packets it
//! back-dated those copies by the whole gap per copy, so they landed on top of audio that had
//! already been delivered (the stock 0.3.1 code, `lost_ns * (n_packets - i)`).
//!
//! This gate lifts the SHIPPED `vban-source.c` into a C driver, links it against small stubs of
//! the libobs calls it makes (`obs_source_output_audio` prints what OBS would receive), feeds it
//! scripted packets, and requires:
//!
//! - every packet's audio reaches OBS exactly once;
//! - the slots of the lost packets reach OBS as silence, exactly as many frames as were lost;
//! - the stamps are contiguous: each output starts where the previous one ended.
//!
//! It runs under `cargo test` in CI and standalone with no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_source_loss_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required. Per the project's test-strictness rule this FAILS LOUDLY rather than skipping
//! when the toolchain is missing.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "vendor/obs-vban/src/vban-source.c";
const FRAMES: u32 = 256;
const RATE: u64 = 48_000;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The driver: includes the shipped source by its absolute path, stubs every libobs / UDP call it
/// makes, and replays the packets named on its command line (`nu:now_ns:value`).
const DRIVER: &str = r##"
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
#include "@SOURCE@"
#undef blog

static uint64_t g_now;

uint64_t os_gettime_ns(void)
{
	return g_now;
}

void blog(int level, const char *format, ...)
{
	va_list args;
	va_start(args, format);
	fprintf(stderr, "blog %d: ", level);
	vfprintf(stderr, format, args);
	fputc('\n', stderr);
	va_end(args);
}

void *bmalloc(size_t size)
{
	void *p = malloc(size ? size : 1);
	if (!p)
		abort();
	return p;
}

void *brealloc(void *ptr, size_t size)
{
	void *p = realloc(ptr, size ? size : 1);
	if (!p)
		abort();
	return p;
}

void bfree(void *ptr)
{
	free(ptr);
}

void *bmemdup(const void *ptr, size_t size)
{
	void *p = bmalloc(size);
	if (size)
		memcpy(p, ptr, size);
	return p;
}

const char *obs_module_text(const char *lookup_string)
{
	return lookup_string;
}

long long obs_data_get_int(obs_data_t *data, const char *name)
{
	(void)data;
	if (strcmp(name, "port") == 0)
		return 6980;
	if (strcmp(name, "receive_buffer_ms") == 0)
		return 40;
	return 0;
}

const char *obs_data_get_string(obs_data_t *data, const char *name)
{
	(void)data;
	if (strcmp(name, "stream_name") == 0)
		return "stream";
	return "";
}

void obs_data_set_default_int(obs_data_t *data, const char *name, long long val)
{
	(void)data;
	(void)name;
	(void)val;
}

obs_properties_t *obs_properties_create(void)
{
	return NULL;
}

obs_property_t *obs_properties_add_int(obs_properties_t *props, const char *name, const char *description, int min,
				       int max, int step)
{
	(void)props;
	(void)name;
	(void)description;
	(void)min;
	(void)max;
	(void)step;
	return NULL;
}

obs_property_t *obs_properties_add_text(obs_properties_t *props, const char *name, const char *description,
					enum obs_text_type type)
{
	(void)props;
	(void)name;
	(void)description;
	(void)type;
	return NULL;
}

const char *obs_source_get_name(const obs_source_t *source)
{
	(void)source;
	return "Zvuk z mixu";
}

void obs_source_output_audio(obs_source_t *source, const struct obs_source_audio *audio)
{
	(void)source;
	int zero = 1;
	long first = 0;
	size_t n = (size_t)audio->frames * (size_t)audio->speakers;
	if (audio->format == AUDIO_FORMAT_16BIT) {
		const int16_t *d = (const int16_t *)audio->data[0];
		first = d[0];
		for (size_t i = 0; i < n; i++)
			if (d[i] != 0)
				zero = 0;
	} else if (audio->format == AUDIO_FORMAT_FLOAT_PLANAR) {
		const float *d0 = (const float *)audio->data[0];
		first = (long)(d0[0] * 32768.0f);
		for (int ch = 0; ch < (int)audio->speakers; ch++) {
			const float *d = (const float *)audio->data[ch];
			for (uint32_t i = 0; i < audio->frames; i++)
				if (d[i] != 0.0f)
					zero = 0;
		}
	} else if (audio->format == AUDIO_FORMAT_FLOAT) {
		const float *d = (const float *)audio->data[0];
		first = (long)(d[0] * 32768.0f);
		for (size_t i = 0; i < n; i++)
			if (d[i] != 0.0f)
				zero = 0;
	} else {
		zero = -1;
	}
	printf("out ts=%" PRIu64 " frames=%" PRIu32 " ch=%d rate=%" PRIu32 " zero=%d first=%ld\n", audio->timestamp,
	       audio->frames, (int)audio->speakers, audio->samples_per_sec, zero, first);
}

static int g_udp_dummy;

vban_udp_t *vban_udp_find_or_create(int port)
{
	(void)port;
	return (vban_udp_t *)&g_udp_dummy;
}

void vban_udp_release(vban_udp_t *dev)
{
	(void)dev;
}

void vban_udp_add_callback(vban_udp_t *dev, vban_udp_cb_t cb, void *data)
{
	(void)dev;
	(void)cb;
	(void)data;
}

void vban_udp_remove_callback(vban_udp_t *dev, vban_udp_cb_t cb, void *data)
{
	(void)dev;
	(void)cb;
	(void)data;
}

void vban_udp_set_name(vban_udp_t *dev, vban_udp_cb_t cb, void *data, const char *name)
{
	(void)dev;
	(void)cb;
	(void)data;
	(void)name;
}

void vban_udp_set_host(vban_udp_t *dev, vban_udp_cb_t cb, void *data, const char *host)
{
	(void)dev;
	(void)cb;
	(void)data;
	(void)host;
}

/* One stereo 16-bit 48 kHz packet of FRAMES frames, every sample = VALUE. */
static void send_packet(void *src, uint32_t nu_frame, uint32_t frames, int16_t value, uint64_t now)
{
	char buf[VBAN_PROTOCOL_MAX_SIZE];
	memset(buf, 0, sizeof(buf));
	struct VBanHeader *h = (struct VBanHeader *)buf;
	memcpy(&h->vban, "VBAN", 4);
	h->format_SR = 3;
	h->format_nbs = (uint8_t)(frames - 1);
	h->format_nbc = 1;
	h->format_bit = VBAN_BITFMT_16_INT;
	strncpy(h->streamname, "stream", VBAN_STREAM_NAME_SIZE);
	h->nuFrame = nu_frame;
	int16_t *pcm = (int16_t *)(buf + VBAN_HEADER_SIZE);
	for (uint32_t i = 0; i < frames * 2; i++)
		pcm[i] = value;
	g_now = now;
	printf("in nu=%" PRIu32 " now=%" PRIu64 " value=%d\n", nu_frame, now, (int)value);
	vban_src_callback(buf, VBAN_HEADER_SIZE + (size_t)frames * 4, src);
}

int main(int argc, char **argv)
{
	if (argc < 2)
		return 2;
	uint32_t frames = (uint32_t)strtoul(argv[1], NULL, 10);
	void *src = vban_source_info.create(NULL, (obs_source_t *)&g_udp_dummy);
	for (int i = 2; i < argc; i++) {
		unsigned long nu = 0, value = 0;
		unsigned long long now = 0;
		if (sscanf(argv[i], "%lu:%llu:%lu", &nu, &now, &value) != 3)
			return 3;
		send_packet(src, (uint32_t)nu, frames, (int16_t)value, (uint64_t)now);
	}
	vban_source_info.destroy(src);
	return 0;
}
"##;

/// One audio block OBS received.
#[derive(Debug, Clone, Copy)]
struct Out {
    ts: u64,
    frames: u64,
    zero: bool,
    first: i64,
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
fn run(packets: &[(u32, u64, i16)]) -> Vec<Out> {
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
        .args(["-std=gnu11", "-Wall", "-Wextra", "-Werror", "-O1"])
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
    cmd.arg(FRAMES.to_string());
    for (nu, now, value) in packets {
        cmd.arg(format!("{nu}:{now}:{value}"));
    }
    let run = cmd
        .output()
        .expect("issue 1439: the compiled driver failed to execute");
    assert!(
        run.status.success(),
        "issue 1439: the driver exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("driver stdout is utf-8")
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
            }
        })
        .collect()
}

/// floor(samples * 1e9 / RATE).
fn ns(samples: u64) -> u64 {
    samples * 1_000_000_000 / RATE
}

/// The packets `nu_frames`, each arriving exactly when its audio is complete on an ideal sender.
fn on_grid(nu_frames: &[u32]) -> Vec<(u32, u64, i16)> {
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

fn assert_contiguous(outs: &[Out]) {
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

/// The non-silent outputs' sample values, in order.
fn audio_values(outs: &[Out]) -> Vec<i64> {
    outs.iter().filter(|o| !o.zero).map(|o| o.first).collect()
}

#[test]
fn two_lost_packets_are_silence_for_exactly_their_slots_1439() {
    let outs = run(&on_grid(&[100, 101, 102, 105, 106]));
    assert_eq!(
        audio_values(&outs),
        vec![1, 2, 3, 6, 7],
        "issue 1439: every packet's audio must reach OBS exactly once, in order (a lost packet was \
         padded with copies of the next one): {outs:?}"
    );
    let at3 = outs.iter().position(|o| !o.zero && o.first == 3).unwrap();
    let at6 = outs.iter().position(|o| !o.zero && o.first == 6).unwrap();
    let gap = &outs[at3 + 1..at6];
    assert!(
        gap.iter().all(|o| o.zero),
        "issue 1439: the lost slots must be silence: {outs:?}"
    );
    assert_eq!(
        gap.iter().map(|o| o.frames).sum::<u64>(),
        2 * u64::from(FRAMES),
        "issue 1439: exactly the two lost packets' frames must be silence: {outs:?}"
    );
    assert_contiguous(&outs);
}

#[test]
fn one_lost_packet_is_one_silent_slot_1439() {
    let outs = run(&on_grid(&[200, 201, 203, 204]));
    assert_eq!(audio_values(&outs), vec![1, 2, 4, 5], "{outs:?}");
    let silent: u64 = outs.iter().filter(|o| o.zero).map(|o| o.frames).sum();
    assert_eq!(
        silent,
        u64::from(FRAMES),
        "issue 1439: one lost packet = one packet of silence: {outs:?}"
    );
    assert_contiguous(&outs);
}

#[test]
fn a_stream_without_loss_has_no_silence_1439() {
    let outs = run(&on_grid(&[7, 8, 9, 10, 11, 12]));
    assert_eq!(audio_values(&outs), vec![1, 2, 3, 4, 5, 6], "{outs:?}");
    assert!(outs.iter().all(|o| !o.zero), "{outs:?}");
    assert_contiguous(&outs);
}
