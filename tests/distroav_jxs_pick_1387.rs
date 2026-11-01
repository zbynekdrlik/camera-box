//! Issue 1387 (owner 1.11.2026): the "Kamera" list on a JPEG XS input
//! (`vendor/distroav/src/jxs-source.cpp`, design comment 6558038959, Approach 1).
//!
//! With NDI the operator picked in a list which camera an input shows; the JPEG XS input had only
//! two raw numbers. The input now offers the cameras the box declares (`/opt/camera-box/jxs-seed.json`,
//! rendered by setup-strih step 6): label `CAM<N> (UDP <port>)`, value `<port>:<camera>`, plus one
//! custom entry that leaves the numbers to the operator. Picking writes `jxs_port` + `jxs_camera`.
//!
//! CI's strih Linux build is the plugin's first real compile, so this gate:
//! 1. compiles the pure value/label/parse/shown helpers of `jxs-source-decide.h` as shipped into a
//!    truth table (C11, `-Wconversion -Werror`) and runs it, bad values included;
//! 2. LIFTS the dropdown block out of `jxs-source.cpp` (the manifest reader, the list fill + the shown
//!    pick, the modified callback) and runs it against a fake libobs `obs_data` (missing / broken /
//!    real manifest, a stale pick, the custom entry, every reference released);
//! 3. anchors the wiring: the list property, its callback, the default, the manifest path shared
//!    with the seeder and setup-strih, and the state row that names why an input cannot listen.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

const SOURCE: &str = "vendor/distroav/src/jxs-source.cpp";
const DECIDE: &str = "vendor/distroav/src/jxs-source-decide.h";

static RUNS: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> tempfile::TempDir {
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    tempfile::Builder::new()
        .prefix(&format!("jxs_pick_1387-{tag}-{n}-"))
        .tempdir()
        .unwrap()
}

fn run(cmd: &mut Command, what: &str) -> String {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: cannot run ({e}); this gate must FAIL, never skip"));
    assert!(
        out.status.success(),
        "{what} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const TRUTH_TABLE: &str = r#"
#include <stdio.h>
#include <string.h>
#include "@DECIDE@"
static int fails = 0;
#define EXPECT(c) do { if (!(c)) { printf("FAIL line %d: %s\n", __LINE__, #c); fails++; } } while (0)

static int parses(const char *v, unsigned port, unsigned camera)
{
	uint16_t p = 4242;
	uint8_t c = 42;
	if (!cb_jxs_pick_parse(v, &p, &c))
		return 0;
	return p == port && c == camera;
}

/* a refused value never touches the outputs */
static int refused(const char *v)
{
	uint16_t p = 4242;
	uint8_t c = 42;
	return !cb_jxs_pick_parse(v, &p, &c) && p == 4242 && c == 42;
}

int main(void)
{
	char b[CB_JXS_PICK_LABEL_MAX];
	/* the value: "<port>:<camera>" */
	EXPECT(cb_jxs_pick_value(b, sizeof b, 7002, 2) && strcmp(b, "7002:2") == 0);
	EXPECT(cb_jxs_pick_value(b, sizeof b, 1, 0) && strcmp(b, "1:0") == 0);
	EXPECT(cb_jxs_pick_value(b, sizeof b, 65535, 255) && strcmp(b, "65535:255") == 0);
	EXPECT(sizeof "65535:255" <= CB_JXS_PICK_VALUE_MAX);
	EXPECT(!cb_jxs_pick_value(b, sizeof b, 0, 2) && b[0] == '\0');
	EXPECT(!cb_jxs_pick_value(b, sizeof b, 65536, 2) && b[0] == '\0');
	EXPECT(!cb_jxs_pick_value(b, sizeof b, -1, 2) && b[0] == '\0');
	EXPECT(!cb_jxs_pick_value(b, sizeof b, 7002, 256) && b[0] == '\0');
	EXPECT(!cb_jxs_pick_value(b, sizeof b, 7002, -1) && b[0] == '\0');
	EXPECT(!cb_jxs_pick_value(b, sizeof b, 4294974298LL, 2) && b[0] == '\0'); /* 7002 + 2^32 */
	EXPECT(!cb_jxs_pick_value(b, 6, 7002, 2) && b[0] == '\0'); /* "7002:2" needs 7 bytes */
	EXPECT(cb_jxs_pick_value(b, 7, 7002, 2) && strcmp(b, "7002:2") == 0);
	EXPECT(!cb_jxs_pick_value(b, 0, 7002, 2));
	EXPECT(!cb_jxs_pick_value(NULL, 16, 7002, 2));
	/* the label: "CAM<N> (UDP <port>)" */
	EXPECT(cb_jxs_pick_label(b, sizeof b, 7002, 2) && strcmp(b, "CAM2 (UDP 7002)") == 0);
	EXPECT(cb_jxs_pick_label(b, sizeof b, 65535, 255) && strcmp(b, "CAM255 (UDP 65535)") == 0);
	EXPECT(sizeof "CAM255 (UDP 65535)" <= CB_JXS_PICK_LABEL_MAX);
	EXPECT(!cb_jxs_pick_label(b, sizeof b, 0, 2) && b[0] == '\0');
	EXPECT(!cb_jxs_pick_label(b, sizeof b, 7002, 300) && b[0] == '\0');
	EXPECT(!cb_jxs_pick_label(b, 8, 7002, 2) && b[0] == '\0');
	/* the parse: exactly "<port>:<camera>", plain ASCII digits, in range */
	EXPECT(parses("7002:2", 7002, 2));
	EXPECT(parses("1:0", 1, 0));
	EXPECT(parses("7000:0", 7000, 0));
	EXPECT(parses("65535:255", 65535, 255));
	EXPECT(parses("7100:100", 7100, 100));
	EXPECT(refused(""));                 /* empty */
	EXPECT(refused(NULL));
	EXPECT(refused(CB_JXS_PICK_CUSTOM)); /* the custom entry is never a pair */
	EXPECT(refused("custom"));
	EXPECT(refused("7002"));             /* no colon */
	EXPECT(refused("7002 2"));
	EXPECT(refused(":2"));
	EXPECT(refused("7002:"));
	EXPECT(refused("7002::2"));
	EXPECT(refused("7002:2:3"));
	EXPECT(refused("0:2"));              /* a port outside 1..65535 */
	EXPECT(refused("65536:2"));
	EXPECT(refused("99999:2"));
	EXPECT(refused("123456:2"));
	EXPECT(refused("4294974298:2"));     /* 7002 + 2^32: never wraps */
	EXPECT(refused("18446744073709558618:2")); /* 7002 + 2^64: a 64-bit accumulator wraps to 7002 */
	EXPECT(refused("7002:256"));         /* a camera outside 0..255 */
	EXPECT(refused("7002:1000"));
	EXPECT(refused("7002:4294967298"));  /* 2 + 2^32 */
	EXPECT(refused("7002:18446744073709551618")); /* 2 + 2^64 */
	EXPECT(refused("7002:2x"));          /* trailing junk */
	EXPECT(refused("7002:2 "));
	EXPECT(refused("7002:2\n"));
	EXPECT(refused(" 7002:2"));
	EXPECT(refused("7002 :2"));
	EXPECT(refused("-1:2"));             /* a negative number */
	EXPECT(refused("7002:-1"));
	EXPECT(refused("-7002:2"));
	EXPECT(refused("+7002:2"));          /* a sign */
	EXPECT(refused("7002:+2"));
	EXPECT(refused("07002:2"));          /* a leading zero: one spelling per pair */
	EXPECT(refused("7002:02"));
	EXPECT(refused("0x1b5a:2"));
	EXPECT(refused("7002.0:2"));
	/* the value round-trips through the parse */
	{
		const long long pairs[][2] = {{1, 0}, {7000, 5}, {7002, 2}, {7100, 100}, {65535, 255}};
		for (size_t i = 0; i < sizeof pairs / sizeof pairs[0]; i++) {
			char v[CB_JXS_PICK_VALUE_MAX];
			EXPECT(cb_jxs_pick_value(v, sizeof v, pairs[i][0], pairs[i][1]));
			EXPECT(parses(v, (unsigned)pairs[i][0], (unsigned)pairs[i][1]));
		}
	}
	/* the shown pick: the pair's value when it is listed, else the custom entry */
	{
		static const char *const listed[] = {"7002:2", "7003:3", NULL, "7000:5"};
		char v[CB_JXS_PICK_VALUE_MAX];
		cb_jxs_pick_shown(v, sizeof v, 7003, 3, listed, 4);
		EXPECT(strcmp(v, "7003:3") == 0);
		cb_jxs_pick_shown(v, sizeof v, 7000, 5, listed, 4);
		EXPECT(strcmp(v, "7000:5") == 0);
		cb_jxs_pick_shown(v, sizeof v, 7000, 0, listed, 4); /* the kind's default pair */
		EXPECT(strcmp(v, CB_JXS_PICK_CUSTOM) == 0);
		cb_jxs_pick_shown(v, sizeof v, 7003, 2, listed, 4); /* a mixed pair is no camera */
		EXPECT(strcmp(v, CB_JXS_PICK_CUSTOM) == 0);
		cb_jxs_pick_shown(v, sizeof v, 0, 0, listed, 4);
		EXPECT(strcmp(v, CB_JXS_PICK_CUSTOM) == 0);
		cb_jxs_pick_shown(v, sizeof v, 7002, 2, listed, 0); /* nothing declared */
		EXPECT(strcmp(v, CB_JXS_PICK_CUSTOM) == 0);
		cb_jxs_pick_shown(v, sizeof v, 7002, 2, NULL, 3);
		EXPECT(strcmp(v, CB_JXS_PICK_CUSTOM) == 0);
	}
	printf("checked, %d failure(s)\n", fails);
	return fails ? 1 : 0;
}
"#;

#[test]
fn the_pick_value_label_parse_and_shown_helpers_hold_their_truth_table_compiled_from_the_shipped_header(
) {
    let dir = scratch("truth");
    let src = dir.path().join("truth.c");
    let bin = dir.path().join("truth.bin");
    fs::write(
        &src,
        TRUTH_TABLE.replace("@DECIDE@", &repo(DECIDE).display().to_string()),
    )
    .unwrap();
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    run(
        Command::new(&cc)
            .args(["-std=c11", "-Wall", "-Wextra", "-Wconversion", "-Werror"])
            .arg(&src)
            .arg("-o")
            .arg(&bin),
        "compile the pick truth table",
    );
    let out = run(&mut Command::new(&bin), "the pick truth table");
    assert!(out.contains("checked, 0 failure(s)"), "{out}");
}

/// The dropdown block of jxs-source.cpp: from the item struct to the end of the modified callback.
fn pick_block(src: &str) -> &str {
    let first = "struct jxs_pick_item {";
    let last = "static bool jxs_pick_modified(";
    assert_eq!(src.matches(first).count(), 1, "`{first}` once");
    assert_eq!(src.matches(last).count(), 1, "`{last}` once");
    let start = src.find(first).unwrap();
    let cb = src.find(last).unwrap();
    assert!(start < cb, "the item struct comes before the callback");
    let end = src[cb..].find("\n}\n").expect("the callback's end") + cb + 3;
    &src[start..end]
}

/// A fake of the libobs calls the dropdown block makes: obs_data trees the test builds, a file
/// registry for obs_data_create_from_json_file, the list items, and a reference count that must
/// read 0 at the end (every get released once).
const LIFT_HARNESS: &str = r#"
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <string>
#include <utility>
#include <vector>
#include "@DECIDE@"

struct obs_data;
struct obs_data_array {
	std::vector<obs_data *> items;
};
struct obs_data {
	std::map<std::string, long long> ints;
	std::map<std::string, std::string> strs;
	std::map<std::string, std::string> default_strs;
	std::map<std::string, obs_data *> objs;
	std::map<std::string, obs_data_array *> arrs;
	int int_writes = 0;
};
typedef struct obs_data obs_data_t;
typedef struct obs_data_array obs_data_array_t;
struct obs_property {
	std::vector<std::pair<std::string, std::string>> items;
};
typedef struct obs_property obs_property_t;
struct obs_properties {};
typedef struct obs_properties obs_properties_t;
struct obs_source {
	obs_data *settings;
};
typedef struct obs_source obs_source_t;
struct jxs_source {
	obs_source_t *source;
};

static int refs = 0;
static std::map<std::string, obs_data *> files;

static obs_data_t *obs_data_create_from_json_file(const char *path)
{
	auto it = files.find(path);
	if (it == files.end())
		return nullptr;
	refs++;
	return it->second;
}
static void obs_data_release(obs_data_t *d)
{
	if (d)
		refs--;
}
static void obs_data_array_release(obs_data_array_t *a)
{
	if (a)
		refs--;
}
static obs_data_array_t *obs_data_get_array(obs_data_t *d, const char *k)
{
	if (!d || !d->arrs.count(k))
		return nullptr;
	refs++;
	return d->arrs[k];
}
static size_t obs_data_array_count(obs_data_array_t *a)
{
	return a ? a->items.size() : 0;
}
static obs_data_t *obs_data_array_item(obs_data_array_t *a, size_t i)
{
	if (!a || i >= a->items.size())
		return nullptr;
	refs++;
	return a->items[i];
}
static obs_data_t *obs_data_get_obj(obs_data_t *d, const char *k)
{
	if (!d || !d->objs.count(k))
		return nullptr;
	refs++;
	return d->objs[k];
}
static long long obs_data_get_int(obs_data_t *d, const char *k)
{
	return d && d->ints.count(k) ? d->ints[k] : 0;
}
static void obs_data_set_int(obs_data_t *d, const char *k, long long v)
{
	d->ints[k] = v;
	d->int_writes++;
}
static const char *obs_data_get_string(obs_data_t *d, const char *k)
{
	if (!d)
		return "";
	if (d->strs.count(k))
		return d->strs[k].c_str();
	if (d->default_strs.count(k))
		return d->default_strs[k].c_str();
	return "";
}
static void obs_data_set_string(obs_data_t *d, const char *k, const char *v)
{
	d->strs[k] = v;
}
static obs_data_t *obs_source_get_settings(const obs_source_t *s)
{
	refs++;
	return s->settings;
}
static size_t obs_property_list_add_string(obs_property_t *p, const char *name, const char *val)
{
	p->items.emplace_back(name, val);
	return p->items.size() - 1;
}

#define PROP_JXS_PORT "jxs_port"
#define PROP_JXS_CAMERA "jxs_camera"
#define PROP_JXS_PICK "jxs_pick"
#define JXS_SEED_MANIFEST "/fake/jxs-seed.json"

@BLOCK@

static int fails = 0;
#define EXPECT(c) do { if (!(c)) { printf("FAIL line %d: %s\n", __LINE__, #c); fails++; } } while (0)

static obs_data *entry(long long port, long long camera)
{
	obs_data *e = new obs_data();
	obs_data *st = new obs_data();
	st->ints["jxs_port"] = port;
	st->ints["jxs_camera"] = camera;
	st->ints["jxs_max_bpp"] = 0;
	e->strs["name"] = "x";
	e->objs["settings"] = st;
	return e;
}

static obs_data *source_settings(long long port, long long camera, const char *pick)
{
	obs_data *s = new obs_data();
	s->ints["jxs_port"] = port;
	s->ints["jxs_camera"] = camera;
	s->default_strs["jxs_pick"] = CB_JXS_PICK_CUSTOM;
	if (pick)
		s->strs["jxs_pick"] = pick;
	return s;
}

static std::vector<std::string> labels(const obs_property &p)
{
	std::vector<std::string> out;
	for (const auto &it : p.items)
		out.push_back(it.first + "=" + it.second);
	return out;
}

int main(void)
{
	/* 1. no manifest file: only the custom entry, the pick reads custom */
	{
		obs_property list;
		obs_source src = {source_settings(7002, 2, nullptr)};
		jxs_source s = {&src};
		jxs_pick_list(&list, &s);
		EXPECT(list.items.size() == 1);
		EXPECT(list.items[0].second == CB_JXS_PICK_CUSTOM);
		EXPECT(std::string(obs_data_get_string(src.settings, PROP_JXS_PICK)) == CB_JXS_PICK_CUSTOM);
	}
	/* 2. an unparseable file (libobs hands back an EMPTY object): only the custom entry */
	{
		files[JXS_SEED_MANIFEST] = new obs_data();
		obs_property list;
		jxs_pick_list(&list, nullptr); /* the type's own properties: no input, no settings */
		EXPECT(list.items.size() == 1 && list.items[0].second == CB_JXS_PICK_CUSTOM);
	}
	/* 3. the strih-snv manifest, a duplicate pair, an entry with no settings and two out-of-range
	 *    pairs: the declared cameras in manifest order, each pair once, then the custom entry */
	{
		obs_data *m = new obs_data();
		obs_data_array *inputs = new obs_data_array();
		inputs->items = {entry(7002, 2), entry(7003, 3), entry(7000, 5), entry(7003, 3), new obs_data(),
				 entry(0, 4), entry(7006, 300)};
		m->arrs["inputs"] = inputs;
		files[JXS_SEED_MANIFEST] = m;
		obs_property list;
		obs_source src = {source_settings(7003, 3, "7002:2")}; /* a stale pick from an older pair */
		jxs_source s = {&src};
		jxs_pick_list(&list, &s);
		const std::vector<std::string> want = {"CAM2 (UDP 7002)=7002:2", "CAM3 (UDP 7003)=7003:3",
						       "CAM5 (UDP 7000)=7000:5"};
		std::vector<std::string> got = labels(list);
		EXPECT(got.size() == 4);
		EXPECT(std::vector<std::string>(got.begin(), got.begin() + 3) == want);
		EXPECT(list.items.back().second == CB_JXS_PICK_CUSTOM);
		/* the shown pick follows the input's pair, never the stale one */
		EXPECT(std::string(obs_data_get_string(src.settings, PROP_JXS_PICK)) == "7003:3");
		/* libobs then applies every callback once (obs_properties_apply_settings): no write */
		EXPECT(!jxs_pick_modified(nullptr, &list, src.settings));
		EXPECT(src.settings->int_writes == 0);
		EXPECT(obs_data_get_int(src.settings, PROP_JXS_PORT) == 7003);
		EXPECT(obs_data_get_int(src.settings, PROP_JXS_CAMERA) == 3);
		/* an unlisted pair (the operator's own numbers) shows the custom entry */
		obs_property list2;
		obs_source src2 = {source_settings(7099, 3, "7003:3")};
		jxs_source s2 = {&src2};
		jxs_pick_list(&list2, &s2);
		EXPECT(std::string(obs_data_get_string(src2.settings, PROP_JXS_PICK)) == CB_JXS_PICK_CUSTOM);
		EXPECT(!jxs_pick_modified(nullptr, &list2, src2.settings));
		EXPECT(obs_data_get_int(src2.settings, PROP_JXS_PORT) == 7099);
		/* a listed pair under the default pick shows that camera */
		obs_property list3;
		obs_source src3 = {source_settings(7000, 5, nullptr)};
		jxs_source s3 = {&src3};
		jxs_pick_list(&list3, &s3);
		EXPECT(std::string(obs_data_get_string(src3.settings, PROP_JXS_PICK)) == "7000:5");
	}
	/* 4. the operator picks: a camera writes the pair and refreshes, the custom entry or junk never
	 *    touches the numbers */
	{
		obs_data *st = source_settings(7003, 3, "7002:2");
		EXPECT(jxs_pick_modified(nullptr, nullptr, st));
		EXPECT(obs_data_get_int(st, PROP_JXS_PORT) == 7002 && obs_data_get_int(st, PROP_JXS_CAMERA) == 2);
		st->strs["jxs_pick"] = CB_JXS_PICK_CUSTOM;
		const int writes = st->int_writes;
		EXPECT(!jxs_pick_modified(nullptr, nullptr, st));
		st->strs["jxs_pick"] = "7002:2x";
		EXPECT(!jxs_pick_modified(nullptr, nullptr, st));
		st->strs["jxs_pick"] = "";
		EXPECT(!jxs_pick_modified(nullptr, nullptr, st));
		EXPECT(st->int_writes == writes);
		EXPECT(obs_data_get_int(st, PROP_JXS_PORT) == 7002 && obs_data_get_int(st, PROP_JXS_CAMERA) == 2);
	}
	EXPECT(refs == 0);
	printf("refs=%d checked, %d failure(s)\n", refs, fails);
	return fails ? 1 : 0;
}
"#;

#[test]
fn the_dropdown_block_lifted_from_the_source_lists_the_declared_cameras_and_writes_the_pick() {
    let s = read(SOURCE);
    let block = pick_block(&s);
    let dir = scratch("lift");
    let src = dir.path().join("lift.cpp");
    let bin = dir.path().join("lift.bin");
    fs::write(
        &src,
        LIFT_HARNESS
            .replace("@DECIDE@", &repo(DECIDE).display().to_string())
            .replace("@BLOCK@", block),
    )
    .unwrap();
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
    run(
        Command::new(&cxx)
            .args(["-std=c++17", "-Wall", "-Wextra", "-Werror"])
            .arg(&src)
            .arg("-o")
            .arg(&bin),
        "compile the lifted dropdown block",
    );
    let out = run(&mut Command::new(&bin), "the lifted dropdown block");
    assert!(out.contains("refs=0 checked, 0 failure(s)"), "{out}");
}

/// Whitespace runs collapsed to one space (a wrapped call reads as one line).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of a C function: from its signature to the first line that is only `}`.
fn function_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("missing `{signature}`"));
    assert_eq!(src.matches(signature).count(), 1, "`{signature}` twice");
    let end = src[start..].find("\n}\n").expect("function end") + start;
    &src[start..end]
}

#[test]
fn the_kamera_list_is_the_first_property_with_its_callback_and_a_custom_default() {
    let s = read(SOURCE);
    assert!(s.contains("#define PROP_JXS_PICK \"jxs_pick\""));
    let props = squish(function_body(
        &s,
        "static obs_properties_t *jxs_source_getproperties(void *data)",
    ));
    let list = props
        .find("obs_property_t *pick = obs_properties_add_list(props, PROP_JXS_PICK, \"Kamera\", OBS_COMBO_TYPE_LIST, OBS_COMBO_FORMAT_STRING);")
        .expect("the Kamera list property");
    let fill = props
        .find("jxs_pick_list(pick, static_cast<jxs_source *>(data));")
        .expect("the list is filled for this input");
    let cb = props
        .find("obs_property_set_modified_callback(pick, jxs_pick_modified);")
        .expect("the pick writes the pair");
    let port = props
        .find("obs_properties_add_int(props, PROP_JXS_PORT,")
        .expect("the raw port stays");
    assert!(list < fill && fill < cb && cb < port, "{props}");
    assert!(props.contains("obs_properties_add_int(props, PROP_JXS_CAMERA,"));
    let defaults = function_body(&s, "static void jxs_source_getdefaults(");
    assert!(defaults
        .contains("obs_data_set_default_string(settings, PROP_JXS_PICK, CB_JXS_PICK_CUSTOM);"));
    // the receiver reads only the pair: the pick is a view of it
    let update = function_body(&s, "static void jxs_source_update(");
    assert!(!update.contains("PROP_JXS_PICK"), "{update}");
}

#[test]
fn the_list_reads_the_manifest_setup_strih_renders_and_the_seeder_reads() {
    let s = read(SOURCE);
    assert!(s.contains("#define JXS_SEED_MANIFEST \"/opt/camera-box/jxs-seed.json\""));
    let block = pick_block(&s);
    assert!(block.contains("obs_data_create_from_json_file(path)"));
    assert!(block.contains("jxs_declared_cameras(JXS_SEED_MANIFEST)"));
    assert!(read("scripts/obs_jxs_inputs.py")
        .contains("MANIFEST_PATH = \"/opt/camera-box/jxs-seed.json\""));
    assert!(read("scripts/lib/strih-jxs-provision.sh").contains(
        "STRIH_JXS_SEED_MANIFEST=\"${STRIH_JXS_SEED_MANIFEST:-/opt/camera-box/jxs-seed.json}\""
    ));
}

#[test]
fn an_input_that_cannot_listen_names_why_in_its_state_row() {
    // the design's limit: one UDP port binds one input, so a second input on the same camera stays
    // black, and its row in the JPEG XS state table (the status bar tooltip) says why
    let s = read(SOURCE);
    let entry = function_body(&s, "static void jxs_state_entry(");
    assert!(entry.contains("\"UDP %u, camera %u: cannot listen (%s)\""));
    let thread = function_body(&s, "static void jxs_thread(");
    let open = thread
        .find("rx = cbjxs_open(cfg.port, cfg.camera, cfg.max_bpp, err, sizeof err);")
        .unwrap();
    let why = thread
        .find("jxs_state_entry(s, cfg, err[0] ? err : \"no reason given\");")
        .expect("a failed open puts the reason into the row");
    let sleep = thread.find("jxs_sleep(s, JXS_REOPEN_DELAY_NS);").unwrap();
    assert!(open < why && why < sleep);
    // a successful open puts the plain detail back
    assert!(thread.contains("open_cfg = cfg;\n\t\t\tcbjxs_set_late_ms(rx, cb_jxs_late_ms(cfg.latency_ms));\n\t\t\tjxs_state_entry(s, cfg);"));
}
