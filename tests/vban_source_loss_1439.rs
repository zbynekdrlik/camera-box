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
/// `VBAN_BITFMT_*` (vendor/obs-vban/vban/vban.h): 8-bit, 16-bit, 24-bit, 32-bit int, 32-bit float.
const VBAN_BITFMT_8_INT: u8 = 0;
const VBAN_BITFMT_16_INT: u8 = 1;
const VBAN_BITFMT_24_INT: u8 = 2;
const VBAN_BITFMT_32_INT: u8 = 3;
const VBAN_BITFMT_32_FLOAT: u8 = 4;
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
/* the source's settings, changed by `port=` / `name=` / `host=` tokens (each one runs update()) */
static long long g_port = 6980;
static char g_name[32] = "stream";
static char g_host[64] = "";
static long long g_buffer = 40;

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
		return g_port;
	if (strcmp(name, "receive_buffer_ms") == 0)
		return g_buffer;
	return 0;
}

const char *obs_data_get_string(obs_data_t *data, const char *name)
{
	(void)data;
	if (strcmp(name, "stream_name") == 0)
		return g_name;
	if (strcmp(name, "ip_from") == 0)
		return g_host;
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
	} else if (audio->format == AUDIO_FORMAT_U8BIT) {
		const uint8_t *d = (const uint8_t *)audio->data[0];
		first = (long)d[0] - 128;
		for (size_t i = 0; i < n; i++)
			if (d[i] != 128)
				zero = 0;
	} else if (audio->format == AUDIO_FORMAT_32BIT) {
		const int32_t *d = (const int32_t *)audio->data[0];
		first = (long)(d[0] / 65536);
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
	printf("out ts=%" PRIu64 " frames=%" PRIu32 " ch=%d rate=%" PRIu32 " zero=%d first=%ld fmt=%d\n",
	       audio->timestamp, audio->frames, (int)audio->speakers, audio->samples_per_sec, zero, first,
	       (int)audio->format);
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

/* Whether the source had already asked for a new timeline, and which Receive Buffer it holds, when it
 * let a new port / stream / host in: from that call on, the receive thread may deliver packets. */
static void udp_report(const char *op, void *data)
{
	const struct vban_src_s *s = data;
	printf("udp %s reset=%d r=%ld\n", op, os_atomic_load_bool(&s->timeline_reset) ? 1 : 0,
	       os_atomic_load_long(&s->receive_buffer_ms));
}

void vban_udp_add_callback(vban_udp_t *dev, vban_udp_cb_t cb, void *data)
{
	(void)dev;
	(void)cb;
	udp_report("add", data);
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
	(void)name;
	udp_report("name", data);
}

void vban_udp_set_host(vban_udp_t *dev, vban_udp_cb_t cb, void *data, const char *host)
{
	(void)dev;
	(void)cb;
	(void)host;
	udp_report("host", data);
}

/* One stereo 16-bit 48 kHz packet of FRAMES frames, every sample = VALUE. */
/* One stereo 48 kHz packet of FRAMES frames in BITFMT (a VBAN_BITFMT_*), every sample = VALUE on the
 * 16-bit scale. */
static void send_packet(void *src, uint32_t nu_frame, uint32_t frames, int bitfmt, int16_t value, uint64_t now)
{
	/* room for 256 stereo 32-bit frames: the test keeps one packet size for every format (a real
	 * 32-bit sender sends smaller packets, VBAN_PROTOCOL_MAX_SIZE; the source never checks that) */
	char buf[VBAN_HEADER_SIZE + 256 * 2 * 4];
	memset(buf, 0, sizeof(buf));
	struct VBanHeader *h = (struct VBanHeader *)buf;
	memcpy(&h->vban, "VBAN", 4);
	h->format_SR = 3;
	h->format_nbs = (uint8_t)(frames - 1);
	h->format_nbc = 1;
	h->format_bit = (uint8_t)bitfmt;
	strncpy(h->streamname, "stream", VBAN_STREAM_NAME_SIZE);
	h->nuFrame = nu_frame;
	uint8_t *pcm = (uint8_t *)(buf + VBAN_HEADER_SIZE);
	size_t bytes = 0;
	for (uint32_t i = 0; i < frames * 2; i++) {
		if (bitfmt == VBAN_BITFMT_8_INT) {
			pcm[bytes++] = (uint8_t)(value + 128);
		} else if (bitfmt == VBAN_BITFMT_16_INT) {
			int16_t v = value;
			memcpy(pcm + bytes, &v, 2);
			bytes += 2;
		} else if (bitfmt == VBAN_BITFMT_24_INT) {
			int32_t v = (int32_t)value * 256;
			pcm[bytes++] = (uint8_t)(v & 0xff);
			pcm[bytes++] = (uint8_t)((v >> 8) & 0xff);
			pcm[bytes++] = (uint8_t)((v >> 16) & 0xff);
		} else if (bitfmt == VBAN_BITFMT_32_INT) {
			int32_t v = (int32_t)value * 65536;
			memcpy(pcm + bytes, &v, 4);
			bytes += 4;
		} else {
			float v = (float)value / 32768.0f;
			memcpy(pcm + bytes, &v, 4);
			bytes += 4;
		}
	}
	g_now = now;
	printf("in nu=%" PRIu32 " now=%" PRIu64 " value=%d\n", nu_frame, now, (int)value);
	vban_src_callback(buf, VBAN_HEADER_SIZE + bytes, src);
}

int main(int argc, char **argv)
{
	if (argc < 2)
		return 2;
	char *rest = NULL;
	uint32_t frames = (uint32_t)strtoul(argv[1], &rest, 10);
	int bitfmt = (rest && *rest == ',') ? atoi(rest + 1) : VBAN_BITFMT_16_INT;
	void *src = vban_source_info.create(NULL, (obs_source_t *)&g_udp_dummy);
	for (int i = 2; i < argc; i++) {
		if (strncmp(argv[i], "port=", 5) == 0 || strncmp(argv[i], "name=", 5) == 0 ||
		    strncmp(argv[i], "host=", 5) == 0 || strncmp(argv[i], "buffer=", 7) == 0) {
			if (argv[i][0] == 'b')
				g_buffer = atoll(argv[i] + 7);
			else if (argv[i][0] == 'p')
				g_port = atoll(argv[i] + 5);
			else if (argv[i][0] == 'n')
				snprintf(g_name, sizeof(g_name), "%s", argv[i] + 5);
			else
				snprintf(g_host, sizeof(g_host), "%s", argv[i] + 5);
			printf("update %s\n", argv[i]);
			vban_source_info.update(src, NULL);
			continue;
		}
		unsigned long nu = 0, value = 0;
		unsigned long long now = 0;
		if (sscanf(argv[i], "%lu:%llu:%lu", &nu, &now, &value) != 3)
			return 3;
		send_packet(src, (uint32_t)nu, frames, bitfmt, (int16_t)value, (uint64_t)now);
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
    /// The libobs `enum audio_format` value OBS received.
    format: i32,
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
    run_with_log(packets).0
}

/// [`run`], plus the plugin's log lines (`blog <level>: <text>`).
fn run_with_log(packets: &[(u32, u64, i16)]) -> (Vec<Out>, String) {
    run_as(packets, VBAN_BITFMT_16_INT)
}

/// The packets sent in the VBAN sample format `bitfmt` (the header's `VBAN_BITFMT_*` value).
fn run_as(packets: &[(u32, u64, i16)], bitfmt: u8) -> (Vec<Out>, String) {
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
            }
        })
        .collect();
    (outs, log)
}

/// Runs the driver over free tokens: packets (`nu:now_ns:value`) and setting changes (`port=N`,
/// `name=S`, `host=H`, `buffer=MS`, each one followed by the source's `update()`). Returns (stdout,
/// the log).
fn run_tokens(tokens: &[String], bitfmt: u8) -> (String, String) {
    run_tokens_frames(tokens, FRAMES, bitfmt)
}

/// [`run_tokens`] with packets of `frames` frames.
fn run_tokens_frames(tokens: &[String], frames: u32, bitfmt: u8) -> (String, String) {
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

/// The 10 s status line the stream-role verify reads (`verify-strih.sh`): the received counter, the
/// fixed R, the port, the packet's own stream name and the OBS source name.
#[test]
fn the_status_line_reports_the_counters_r_port_and_stream_1439() {
    // 11 s of an ideal sender: the first packet arms the status clock, the line comes 10 s later.
    let nus: Vec<u32> = (1000..1000 + 2063).collect();
    let (outs, log) = run_with_log(&on_grid(&nus));
    assert_eq!(outs.len(), nus.len(), "no packet may be lost or padded");
    let lines: Vec<&str> = log
        .lines()
        // the plugin's blog macro prefixes every line with `[obs-vban] `
        .filter_map(|l| l.strip_prefix("blog 300: [obs-vban] obs-vban receive: "))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "issue 1439: one status line in 11 s:\n{log}"
    );
    let line = lines[0];
    let field = |k: &str| -> &str {
        line.split_whitespace()
            .find_map(|f| f.strip_prefix(k))
            .unwrap_or_else(|| panic!("issue 1439: no `{k}` in the status line `{line}`"))
    };
    let received: u64 = field("received=").parse().unwrap();
    // 10 s of 256-frame packets at 48 kHz is 1875 packets; the line is written by the packet that
    // crosses the 10 s mark, which it has already counted.
    assert!(
        (1875..=1877).contains(&received),
        "issue 1439: received={received} after 10 s: `{line}`"
    );
    for (k, want) in [
        ("lost=", "0"),
        ("late=", "0"),
        ("re_anchors=", "0"),
        ("receive_buffer_ms=", "40"),
        ("rate=", "48000"),
        ("ch=", "2"),
        ("port=", "6980"),
        ("stream=", "'stream'"),
    ] {
        assert_eq!(field(k), want, "issue 1439: `{k}` in `{line}`");
    }
    assert!(
        line.ends_with("source='Zvuk z mixu'"),
        "issue 1439: the line names its OBS source last: `{line}`"
    );
}

/// A lost packet's silence must reach OBS in the SAME sample format as the stream's packets. libobs
/// `process_audio` resets the source's resampler, and with it the genlock ASRC, whenever the format
/// changes, so silence in another format reset the ASRC twice per loss (review round 1).
#[test]
fn the_silence_keeps_the_streams_sample_format_1439() {
    for (bitfmt, name) in [
        (VBAN_BITFMT_8_INT, "8-bit"),
        (VBAN_BITFMT_16_INT, "16-bit"),
        (VBAN_BITFMT_24_INT, "24-bit"),
        (VBAN_BITFMT_32_INT, "32-bit int"),
        (VBAN_BITFMT_32_FLOAT, "32-bit float"),
    ] {
        let (outs, _log) = run_as(&on_grid(&[300, 301, 303, 304]), bitfmt);
        assert_eq!(
            outs.len(),
            5,
            "{name}: four packets + one silent slot: {outs:?}"
        );
        let formats: Vec<i32> = outs.iter().map(|o| o.format).collect();
        assert!(
            formats.iter().all(|f| *f == formats[0]),
            "issue 1439: {name} -- every output of one stream must keep one sample format, else libobs \
             resets the resampler + the genlock ASRC at each change: {formats:?}"
        );
        assert_eq!(
            audio_values(&outs),
            vec![1, 2, 4, 5],
            "{name}: the audio itself is unchanged: {outs:?}"
        );
        assert_eq!(
            outs.iter()
                .filter(|o| o.zero)
                .map(|o| o.frames)
                .sum::<u64>(),
            u64::from(FRAMES),
            "{name}: one lost packet = one packet of silence: {outs:?}"
        );
        assert_contiguous(&outs);
    }
}

/// Review round 1: `update()` registered the new port / stream / host with the UDP thread first and
/// asked for a new timeline only after it, so the new source's first packet could land on the OLD
/// timeline (a gap read as lost slots, or a counted re-anchor). From the moment the source lets the
/// new sender in, the reset must already be pending.
#[test]
fn a_reconfiguration_asks_for_a_new_timeline_before_the_new_source_can_deliver_1439() {
    let ms = 1_000_000u64;
    let mut tokens: Vec<String> = (0..4)
        .map(|k| format!("{}:{}:100", 10 + k, 100 * ms + k * 6 * ms))
        .collect();
    // a packet after each change, so every change has to ask for its own new timeline
    for (k, change) in ["port=6981", "name=program", "host=10.77.8.110"]
        .iter()
        .enumerate()
    {
        tokens.push(change.to_string());
        tokens.push(format!(
            "{}:{}:100",
            500 + 100 * k as u64,
            (200 + 100 * k as u64) * ms
        ));
    }
    let (stdout, log) = run_tokens(&tokens, VBAN_BITFMT_16_INT);
    let after = |change: &str| -> Vec<&str> {
        let a = stdout
            .find(&format!("update {change}\n"))
            .unwrap_or_else(|| panic!("no `update {change}` in:\n{stdout}"));
        stdout[a..]
            .lines()
            .skip(1)
            .take_while(|l| l.starts_with("udp "))
            .collect()
    };
    for (change, op) in [
        ("port=6981", "udp add"),
        ("port=6981", "udp name"),
        ("port=6981", "udp host"),
        ("name=program", "udp name"),
        ("host=10.77.8.110", "udp host"),
    ] {
        let lines = after(change);
        let line = lines
            .iter()
            .find(|l| l.starts_with(op))
            .unwrap_or_else(|| panic!("{change}: no `{op}` call ({lines:?})"));
        assert!(
            line.starts_with(&format!("{op} reset=1 ")),
            "issue 1439: after `{change}` the source let the new sender in before it asked for a \
             new timeline:\n{stdout}"
        );
    }
    // The packet after each change starts a new timeline: a fresh anchor, never a counted re-anchor.
    assert_eq!(
        log.matches("obs-vban receive anchored:").count(),
        4,
        "issue 1439: the packet after a reconfiguration must anchor a new timeline:\n{log}"
    );
    assert!(
        !log.contains("obs-vban receive re-anchor:"),
        "issue 1439: a reconfiguration is a new timeline, never a counted re-anchor:\n{log}"
    );
}

/// Review round 2: on create, `update()` registered the callback (`update_port`) before it read the
/// Receive Buffer, so a packet in that window anchored with R = 0 and the next one logged a counted
/// `reason=config` re-anchor. The source must hold its R before any sender can reach it.
#[test]
fn the_receive_buffer_is_in_place_before_the_first_sender_can_deliver_1439() {
    let (stdout, _log) = run_tokens(&[], VBAN_BITFMT_16_INT);
    let first = stdout
        .lines()
        .find(|l| l.starts_with("udp "))
        .unwrap_or_else(|| panic!("no UDP registration on create:\n{stdout}"));
    assert!(
        first.ends_with(" r=40"),
        "issue 1439: the source let a sender in before it held its Receive Buffer: `{first}`"
    );
    for line in stdout.lines().filter(|l| l.starts_with("udp ")) {
        assert!(
            line.ends_with(" r=40"),
            "issue 1439: a UDP registration without the Receive Buffer in place: `{line}`"
        );
    }
}

/// ROZHODNUTÉ 6343922049 (2): a packet past its stamp once the fit is valid shifts the line ONCE.
/// The source logs one re-anchor with `fit=kept`, outputs the slots lost before the late packet as
/// silence and then the packet, contiguous on the shifted line, and keeps receiving.
#[test]
fn a_late_shift_outputs_the_lost_slots_and_the_packet_on_the_new_line_1439() {
    // 15 s on the grid: the fit is valid
    let n = 15 * RATE / u64::from(FRAMES);
    let nus: Vec<u32> = (0..n as u32).collect();
    let mut pkts = on_grid(&nus);
    // packets n and n + 1 lost, n + 2 arrives 100 ms after its grid instant (more than R late)
    let k = n + 2;
    pkts.push((
        k as u32,
        1_000_000_000 + ns(k * u64::from(FRAMES)) + 100_000_000,
        99,
    ));
    let (outs, log) = run_with_log(&pkts);
    let reanchors: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("obs-vban receive re-anchor:"))
        .collect();
    assert_eq!(
        reanchors.len(),
        1,
        "issue 1439: one event, one re-anchor:\n{log}"
    );
    assert!(
        reanchors[0].contains("reason=late") && reanchors[0].contains(" fit=kept shift_ms="),
        "issue 1439: a late packet with a valid fit shifts the line, the fit kept: `{}`",
        reanchors[0]
    );
    let tail = &outs[outs.len() - 2..];
    assert!(
        tail[0].zero && tail[0].frames == 2 * u64::from(FRAMES),
        "issue 1439: the two lost slots are silence: {tail:?}"
    );
    assert!(
        !tail[1].zero && tail[1].first == 99,
        "issue 1439: the late packet itself reaches OBS: {tail:?}"
    );
    assert_contiguous(tail);
    assert_eq!(
        outs.iter().filter(|o| !o.zero).count() as u64,
        n + 1,
        "issue 1439: every packet's audio reaches OBS once"
    );
}

/// ROZHODNUTÉ 6343922049 (3): a Receive Buffer no longer than one packet + 10 ms is lifted to that
/// floor, and the source names both in ONE warning: not on every re-anchor, not again after a
/// reconfiguration with the same values.
#[test]
fn a_receive_buffer_under_one_packet_plus_10_ms_warns_once_naming_both_1439() {
    let ms = 1_000_000u64;
    let mut tokens = vec!["buffer=10".to_string()];
    // a first timeline, a sender restart (a counted re-anchor), then a reconfiguration
    tokens.extend((0..5u64).map(|k| format!("{}:{}:100", 10 + k, 100 * ms + k * 6 * ms)));
    tokens.extend((0..5u64).map(|k| format!("{}:{}:100", 5000 + k, 300 * ms + k * 6 * ms)));
    tokens.push("port=6981".to_string());
    tokens.extend((0..5u64).map(|k| format!("{}:{}:100", 20 + k, 500 * ms + k * 6 * ms)));
    let (stdout, log) = run_tokens(&tokens, VBAN_BITFMT_16_INT);
    let warnings: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("obs-vban receive buffer:"))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "issue 1439: one warning for one (R, packet) pair:\n{log}"
    );
    let w = warnings[0];
    assert!(
        w.starts_with("blog 200: ")
            && w.contains("receive_buffer_ms=10 ")
            && w.contains("5.33 ms")
            && w.contains("15.33 ms"),
        "issue 1439: the warning names R and one packet + 10 ms: `{w}`"
    );
    // the first packet is stamped with the effective R: arrival - one packet + 15.33 ms
    let first = stdout
        .lines()
        .find_map(|l| l.strip_prefix("out ts="))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|v| v.parse::<u64>().ok())
        .expect("an output");
    assert_eq!(first, 100 * ms + 10 * ms, "issue 1439: `{stdout}`");
}

/// Issue 1462 + 1439: mbc.lan sends the SNV program to stream-snv as VBAN `stream` in 24-bit PCM
/// (format byte 0x02: `VBAN_BITFMT_24_INT`, PCM codec), 48 kHz stereo, 239 frames per packet. The
/// shipped source decodes it (sign-extended) into float planar, stamps it on the timeline, and a
/// lost packet is 239 frames of silence in the same format.
#[test]
fn mbcs_24_bit_239_frame_packets_decode_and_a_loss_is_one_slot_of_silence_1439() {
    let frames = 239u64;
    let t0 = 1_000_000_000u64;
    let values: [i16; 5] = [1234, -1234, 32767, -32768, 0];
    let tokens: Vec<String> = [0u64, 1, 3, 4, 5]
        .iter()
        .zip(values)
        .map(|(&k, v)| format!("{}:{}:{}", 700 + k, t0 + ns(k * frames), v as u16))
        .collect();
    let (stdout, log) = run_tokens_frames(&tokens, frames as u32, VBAN_BITFMT_24_INT);
    let outs: Vec<(u64, u64, bool, i64, i32)> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("out "))
        .map(|l| {
            let f = |k: &str| -> &str {
                l.split_whitespace()
                    .find_map(|x| x.strip_prefix(k))
                    .unwrap_or_else(|| panic!("no `{k}` in `{l}`"))
            };
            (
                f("ts=").parse().unwrap(),
                f("frames=").parse().unwrap(),
                f("zero=") == "1",
                f("first=").parse().unwrap(),
                f("fmt=").parse().unwrap(),
            )
        })
        .collect();
    assert!(!log.contains("Unsupported format"), "issue 1462: {log}");
    assert_eq!(outs.len(), 6, "five packets + one silent slot: {outs:?}");
    // AUDIO_FORMAT_FLOAT_PLANAR (libobs enum audio_format) for every output, the silence included
    assert!(outs.iter().all(|o| o.4 == 8), "issue 1462: {outs:?}");
    assert!(outs.iter().all(|o| o.1 == frames), "issue 1462: {outs:?}");
    let firsts: Vec<i64> = outs.iter().filter(|o| !o.2).map(|o| o.3).collect();
    assert_eq!(
        firsts,
        vec![1234, -1234, 32767, -32768],
        "issue 1462: the 24-bit samples (sign-extended) reach OBS unchanged: {outs:?}"
    );
    assert!(
        outs[2].2,
        "issue 1462: the lost packet is one slot of silence: {outs:?}"
    );
    for w in outs.windows(2) {
        let expect = w[0].0 + ns(w[0].1);
        assert!(
            w[1].0.abs_diff(expect) <= 2,
            "issue 1462: the stamps are contiguous: {outs:?}"
        );
    }
}
