"""Issues 1513 + 1510: the Dante program device's named latency terms and stream-snv's program TX.

obs v0.4.27 (dlink v0.6.112, dstack 244) takes the device latency as two named keys in
~/.camera-box/dante-input.toml: `network_latency_ms` (a, Dante Controller's latency) + `receive_buffer_ms`
(b, this PC's RX room) = the Dante latency P; `latency_ms` is refused. The owner (issue 1510): every term
explicit per device in the box facts, no latency change (P stays 13.041666 ms = 1 + 12.041666). Issue 1513
(owner yes): stream-snv's `stream-obs` also sends the OBS program (track 1) to Dante as `OBS program L/R`;
with a TX and no (b) dlink would lower P to 8.365 ms, so a TX without an explicit (b) is refused.

Covers the four facts in the ONE loader, the TOML render, and verify item 9's new `(program-dante)` rows
(the TOML terms, the start line's `latency split dante_latency_ns=`, the TX status tokens over the window,
the network-latency NOTE). Every bash run is under the callers' `set -euo pipefail`.
"""
import json
import os
import subprocess
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN",
         "STRIH_DANTE_INPUT_SITE_FILE", "STRIH_DANTE_INPUT_TOML", "STRIH_DANTE_INPUT_IFACE", "STRIH_DANTE_INPUT_KIND_FILE",
         "STRIH", "STREAM", "STRIH_DANTE_RX_PIN_ROOT", "STRIH_DANTE_RX_PIN_OWNER", "OBS_BOX_ONLINE_CPUS")
SITE = "# test site file (fake names)\nRX1=Program L|foh-tx|MASTER-L\nRX2=Program R|foh-tx|MASTER-R\n"
TERMS = ("STRIH_DANTE_NETWORK_LATENCY_MS", "STRIH_DANTE_RECEIVE_BUFFER_MS", "STRIH_DANTE_TX", "STRIH_DANTE_TX_TRACK")


def _bash(body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _facts(box):
    out = {}
    for line in (BOXES / ("%s.env" % box)).read_text().splitlines():
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            out[k] = v
    return out


def _boxes_with(tmp_path, box, pairs):
    d = tmp_path / "boxes"
    d.mkdir(exist_ok=True)
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text())
    f = d / ("%s.env" % box)
    lines = f.read_text().splitlines()
    for k, v in pairs.items():
        assert any(line.startswith(k + "=") for line in lines), k
        lines = ["%s=%s" % (k, v) if line.startswith(k + "=") else line for line in lines]
    f.write_text("\n".join(lines) + "\n")
    return d


def _load(tmp_path, box, pairs):
    d = _boxes_with(tmp_path, box, pairs)
    return _bash('strih_box_load "$1"', box, env={"STRIH_BOXES_DIR": str(d)})


# --- the facts --------------------------------------------------------------------------------------------

def test_the_four_terms_are_required_facts_at_the_end_of_the_key_list():
    r = _bash("strih_box_fact_keys")
    assert r.returncode == 0, r.stderr
    # issue 1502 appended the rig-NIC rename after them
    assert r.stdout.split()[-5:] == list(TERMS) + ["STRIH_NIC_RENAME"]


def test_every_dante_program_box_keeps_todays_latency_explicitly():
    # P = (a) + (b) = 1 + 12.041666 = 13.041666 ms, today's P on strih-obs and stream-obs (no latency change)
    for box in ("strih-snv", "stream-snv"):
        f = _facts(box)
        assert f["STRIH_DANTE_NETWORK_LATENCY_MS"] == "1", box
        assert f["STRIH_DANTE_RECEIVE_BUFFER_MS"] == "12.041666", box


def test_only_stream_snv_sends_the_program_to_dante():
    assert _facts("stream-snv")["STRIH_DANTE_TX"] == "OBS program L,OBS program R"
    assert _facts("stream-snv")["STRIH_DANTE_TX_TRACK"] == "1"
    for box in ("strih-snv", "strih-pp", "stream-pp"):
        f = _facts(box)
        assert (f["STRIH_DANTE_TX"], f["STRIH_DANTE_TX_TRACK"]) == ("none", "none"), box
    for box in ("strih-pp", "stream-pp"):
        f = _facts(box)
        assert (f["STRIH_DANTE_NETWORK_LATENCY_MS"], f["STRIH_DANTE_RECEIVE_BUFFER_MS"]) == ("none", "none"), box


def test_every_box_loads():
    r = _bash('for b in strih-snv stream-snv strih-pp stream-pp; do (strih_box_load "$b" >/dev/null) || exit 1; done')
    assert r.returncode == 0, r.stderr


def test_a_tx_without_an_explicit_receive_buffer_is_refused(tmp_path):
    r = _load(tmp_path, "stream-snv", {"STRIH_DANTE_RECEIVE_BUFFER_MS": "none"})
    assert r.returncode != 0 and "STRIH_DANTE_TX" in r.stderr and "8.365" in r.stderr, r.stderr


@pytest.mark.parametrize("key", ["STRIH_DANTE_NETWORK_LATENCY_MS", "STRIH_DANTE_RECEIVE_BUFFER_MS"])
def test_a_program_device_needs_both_terms_explicit(tmp_path, key):
    r = _load(tmp_path, "strih-snv", {key: "none"})
    assert r.returncode != 0 and key in r.stderr and "explicit" in r.stderr, r.stderr


@pytest.mark.parametrize("key,value", [("STRIH_DANTE_NETWORK_LATENCY_MS", "1"),
                                       ("STRIH_DANTE_RECEIVE_BUFFER_MS", "12.041666"),
                                       ("STRIH_DANTE_TX", "OBS program L,OBS program R"),
                                       ("STRIH_DANTE_TX_TRACK", "1")])
def test_a_box_without_a_program_device_sets_every_term_none(tmp_path, key, value):
    r = _load(tmp_path, "strih-pp", {key: value})
    assert r.returncode != 0 and key in r.stderr and "no " in r.stderr, r.stderr


@pytest.mark.parametrize("value", ["0", "3", "1.5", "0.250", "01", "x", "-1", "١"])
def test_the_network_latency_is_one_of_dante_controllers_values(tmp_path, value):
    r = _load(tmp_path, "strih-snv", {"STRIH_DANTE_NETWORK_LATENCY_MS": value})
    assert r.returncode != 0 and "STRIH_DANTE_NETWORK_LATENCY_MS" in r.stderr, r.stderr


@pytest.mark.parametrize("value", ["0.25", "0.5", "1", "2", "5"])
def test_every_dante_controller_latency_loads(tmp_path, value):
    r = _load(tmp_path, "strih-snv", {"STRIH_DANTE_NETWORK_LATENCY_MS": value})
    assert r.returncode == 0, r.stderr


# review round 1: a leading zero renders `05.0`, which TOML refuses (OBS would not load the file)
@pytest.mark.parametrize("value", ["0", "0.0", "51", "12.0416666", "12.", ".5", "1e3", "12,04", "١٢", "05", "00.5", "05.5"])
def test_a_malformed_receive_buffer_is_refused(tmp_path, value):
    r = _load(tmp_path, "strih-snv", {"STRIH_DANTE_RECEIVE_BUFFER_MS": value})
    assert r.returncode != 0 and "STRIH_DANTE_RECEIVE_BUFFER_MS" in r.stderr, r.stderr


@pytest.mark.parametrize("pairs,word", [
    ({"STRIH_DANTE_TX": "OBS program L"}, "two"),
    ({"STRIH_DANTE_TX": "a,b,c"}, "two"),
    ({"STRIH_DANTE_TX": "a,"}, "two"),
    ({"STRIH_DANTE_TX": " a,b"}, "two"),
    ({"STRIH_DANTE_TX": "a" * 32 + ",b"}, "two"),
    ({"STRIH_DANTE_TX": "x,x"}, "two different"),   # review round 1: the obs config refuses equal names
    ({"STRIH_DANTE_TX_TRACK": "none"}, "STRIH_DANTE_TX_TRACK"),
    ({"STRIH_DANTE_TX_TRACK": "7"}, "STRIH_DANTE_TX_TRACK"),
    ({"STRIH_DANTE_TX_TRACK": "0"}, "STRIH_DANTE_TX_TRACK"),
    ({"STRIH_DANTE_TX": "none"}, "STRIH_DANTE_TX_TRACK"),
])
def test_a_malformed_tx_is_refused(tmp_path, pairs, word):
    r = _load(tmp_path, "stream-snv", pairs)
    assert r.returncode != 0 and word in r.stderr, r.stderr


# review round 1: the fact parser already refuses a quote / backslash, the validator refuses them too
# (a label lands raw in a TOML basic string)
@pytest.mark.parametrize("tx", ['a"b,c', 'a\\b,c'])
def test_the_validator_refuses_a_quote_or_backslash_in_a_label(tx):
    r = _bash('_strih_box_dante_terms_ok box 1 1 12.041666 "$1" 1', tx)
    assert r.returncode != 0 and "STRIH_DANTE_TX" in r.stderr, r.stderr


def test_the_bracket_sets_are_spelled_out_never_a_locale_range():
    # ci-testing-gotchas: under en_US.UTF-8 a bash `[0-9]` range also takes non-ASCII digits
    src = (SCRIPTS / "lib" / "strih-box-dante-facts.sh").read_text()
    body = src[src.index("_strih_box_dante_terms_ok()"):]
    body = body[:body.index("\n}\n")]
    for line in body.splitlines():
        if "=~" in line:
            regex = line.split("=~", 1)[1].split("]]", 1)[0]
            assert "0-9" not in regex and "a-z" not in regex and "A-Z" not in regex, line


# --- the toml -----------------------------------------------------------------------------------------------

def _render(tmp_path, box, iface):
    s = tmp_path / "site.conf"
    s.write_text(SITE)
    return _bash('strih_box_load "$2" >/dev/null; rows="$(strih_dante_input_site_rows "$1")"; '
                 'strih_dante_input_toml "$3" "$(strih_dante_input_ports "$(strih_lx_dante_devices)")" "$rows"',
                 s, box, iface)


def test_strih_snv_toml_states_both_terms_and_no_tx(tmp_path):
    r = _render(tmp_path, "strih-snv", "enp7s0.70")
    assert r.returncode == 0, r.stderr
    d = tomllib.loads(r.stdout)
    assert d["network_latency_ms"] == 1.0 and d["receive_buffer_ms"] == 12.041666
    assert isinstance(d["network_latency_ms"], float) and isinstance(d["receive_buffer_ms"], float)
    assert "tx" not in d and "tx_track" not in d and "latency_ms" not in d


# review round 1: the renderer runs inside obs_box_write_if_changed_rendered with errexit off, so a failed
# fact read must fail the render, never write a file without the terms
def test_a_failed_fact_read_fails_the_render(tmp_path):
    s = tmp_path / "site.conf"
    s.write_text(SITE)
    r = _bash('strih_box_load stream-snv >/dev/null; rows="$(strih_dante_input_site_rows "$1")"; set +e\n'
              'strih_box_fact() { [ "$1" != STRIH_DANTE_TX ] || return 1; printf "%s" "${STRIH_BOX_FACTS[$1]}"; }\n'
              'strih_dante_input_toml enp56s0.70 "4440 8800 4455 30000" "$rows"; echo "rc=$?"', s)
    assert r.stdout.strip().endswith("rc=1"), r.stdout + r.stderr


def test_stream_snv_toml_sends_the_program_and_keeps_the_rx_latency(tmp_path):
    r = _render(tmp_path, "stream-snv", "enp56s0.70")
    assert r.returncode == 0, r.stderr
    d = tomllib.loads(r.stdout)
    assert d["tx"] == ["OBS program L", "OBS program R"] and d["tx_track"] == 1
    assert d["network_latency_ms"] == 1.0 and d["receive_buffer_ms"] == 12.041666
    assert d["rx"] == ["Program L", "Program R"] and len(d["subscribe"]) == 2
    # the keys sit before the first [[subscribe]] table (TOML: a key after it belongs to the table)
    head = r.stdout.split("[[subscribe]]", 1)[0]
    assert "tx_track = 1" in head and "receive_buffer_ms = 12.041666" in head


# --- verify item 9 -----------------------------------------------------------------------------------------

MEDIA_CLOCK = "11:59:59.000: dante-input dlink:  INFO dstack::clock::linked: media clock source source=Internal from=Service"
PTP_PORT_DROPIN = ("# issue 1485: written by camera-box setup-strih step 13 on a Dante-program box; every run rewrites it.\n"
                   "# The strih OBS dante-input (a desktop-user process under systemd --user) follows Dante PTP in process\n"
                   "# and binds UDP 319 (ROZHODNUTE 6628785864). Every unprivileged process may bind 319-1023 on this box.\n"
                   "net.ipv4.ip_unprivileged_port_start = 319\n")


def _start(dev, track):
    return ("12:00:00.000: dante-input start: config=/home/newlevel/.camera-box/dante-input.toml name=%s "
            "iface=enp56s0.70 network_latency_ms=Some(1.0) receive_buffer_ms=Some(12.041666) obs_rate=48000 "
            "sample_rate=48000 device_rate=96000 converts=true rx=[] tx=[] tx_track=%d ports=Some(4440)/Some(8800)/Some(4455) "
            "rx_port_base=Some(30000) subscriptions=2 state_file=x" % (dev, track))


# the live v0.4.27 shapes read on stream-snv (13.11.2026, before the terms were written: None / (default))
SPLIT = "12:00:00.001: dante-input start: latency split dante_latency_ns=13041666 rx_path_ns=14999999 tx_path_ns=28677082"
DLINK_DEFAULT = ("12:00:00.001: dante-input dlink:  INFO dstack::link: dlink latency: Dante latency P 13.041666 ms = "
                 "network latency (a) 1 ms (default) + receive buffer (b) 12.041666 ms (default)")
TXLINE = ("12:00:00.002: dante-input start: TX of OBS track 1 on [\"OBS program L\", \"OBS program R\"]: OBS 48000 Hz into "
          "a FIFO held at 22.667 ms (the app buffer) by dlink's ASRC player, read at 48000 Hz; tx_path_ns=19677083 "
          "obs_to_wire_ns=42343749")
NETLAT = ("12:00:05.000: dante-input dlink:  INFO dstack::rx: flow stats flow=0 network_latency_min_us=120 "
          "network_latency_p99_us=410 network_latency_max_us=980")


def _status(t, f, tx=True, **k):
    v = dict(clock="media", holdover="0", gm="001dc1fffe0a", polls="1", re_anchors="0", rx_packets="10", rx_loss="0",
             rx_gaps="0", rx_late="0", socket_drops="0", blocks="5", underruns="0", overruns="0", delivered_frames=str(f),
             rx_holes="0", rx_written_late="0", late_blocks="0", rebootstraps="0", max_process_us="80",
             obs_dropped="0", max_obs_us="40", wire_late="0", wake_delay_us="300", wake_delay_peak_us="4000")
    if tx:
        v.update(tx_track="1", tx_played="96000", tx_underruns="0", tx_recentres="0", tx_overflows="0",
                 tx_ppm="-1.5", tx_priming="0")
    v.update(k)
    return "%s: dante-input status: %s" % (t, " ".join("%s=%s" % i for i in v.items() if i[1] is not None))


TOML_STREAM = ('name = "stream-obs"\nnetwork_latency_ms = 1.0\nreceive_buffer_ms = 12.041666\n'
               'tx = ["OBS program L", "OBS program R"]\ntx_track = 1\n[[subscribe]]\n')
TOML_STRIH = 'name = "strih-obs"\nnetwork_latency_ms = 1.0\nreceive_buffer_ms = 12.041666\n[[subscribe]]\n'


def _grade(tmp_path, box, log_lines, nxt, toml):
    log = tmp_path / "obs.log"
    log.write_text("\n".join([MEDIA_CLOCK] + log_lines) + "\n")
    pin = tmp_path / "obs-release.pin"
    pin.write_text("OBS_RELEASE_DSTACK_VERSION=v0.6.112\n")
    ic_pin = tmp_path / "intercom-release.pin"
    ic_pin.write_text("INTERCOM_RELEASE_DSTACK_VERSION=v0.6.112\n")
    kf = tmp_path / "kind"
    kf.write_text("dante_input")
    dest = tmp_path / "dante-input.toml"
    dest.write_text(toml)
    sleep_fn = 'sleep() { printf "%%s\\n" %s >> %s; }' % (json.dumps(nxt), json.dumps(str(log)))
    body = ('ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; note() { echo "NOTE $*"; }; %s; '
            'strih_box_load "$5" >/dev/null; strih_dante_input_grade "$1" "$2" "$4"') % sleep_fn
    root = tmp_path / "rxroot"
    (root / "etc" / "sysctl.d").mkdir(parents=True, exist_ok=True)
    (root / "etc" / "sysctl.d" / "60-strih-dante-ptp-port.conf").write_text(PTP_PORT_DROPIN)
    (root / "proc" / "sys" / "net" / "ipv4").mkdir(parents=True, exist_ok=True)
    (root / "proc" / "sys" / "net" / "ipv4" / "ip_unprivileged_port_start").write_text("319\n")
    ch = tmp_path / "channels"
    ch.write_text("channel_verdict=ok\nchannel_detail=ch1 -40.0 dB ch2 -41.0 dB\n")
    return _bash(body, "scn", "127.0.0.1", "unused", log, box,
                 env={"STRIH_DANTE_INPUT_KIND_FILE": str(kf), "STRIH_DANTE_INPUT_TOML": str(dest),
                      "STRIH_PROGRAM_VBAN_CHANNELS_FILE": str(ch), "STRIH_DANTE_INPUT_WAIT_S": "0",
                      "STRIH_DANTE_RX_PIN_ROOT": str(root), "STRIH_DANTE_PTP_OBS_PIN": str(pin),
                      "STRIH_DANTE_PTP_INTERCOM_PIN": str(ic_pin)})


def _stream(tmp_path, l1_kw=None, l2_kw=None, extra=(SPLIT, TXLINE, NETLAT), toml=TOML_STREAM, start_track=1):
    lines = [_start("stream-obs", start_track)] + list(extra) + [_status("12:00:10.000", 4800, **(l1_kw or {}))]
    nxt = _status("12:00:20.000", 484800, **dict({"tx_played": "576000"}, **(l2_kw or {})))
    return _grade(tmp_path, "stream-snv", lines, nxt, toml)


def test_item9_passes_a_healthy_tx_device(tmp_path):
    r = _stream(tmp_path)
    assert r.returncode == 0, r.stderr
    assert "BAD" not in r.stdout, r.stdout
    assert "OK (program-dante) latency terms" in r.stdout, r.stdout
    assert "OK (program-dante) dante latency 13041666 ns" in r.stdout, r.stdout
    assert "OK (program-dante) TX of OBS track 1" in r.stdout, r.stdout
    assert "OK (program-dante) tx: tx_played 96000 -> 576000" in r.stdout, r.stdout
    assert "NOTE (program-dante) network latency (report-only): min 120 us p99 410 us max 980 us" in r.stdout, r.stdout


@pytest.mark.parametrize("key", ["tx_underruns", "tx_recentres", "tx_overflows"])
def test_item9_fails_a_tx_counter_that_grows_over_the_window(tmp_path, key):
    r = _stream(tmp_path, l1_kw={key: "3"}, l2_kw={key: "5"})
    assert ("BAD (program-dante) tx: %s +2" % key) in r.stdout, r.stdout


def test_item9_names_a_flat_tx_counter_as_history(tmp_path):
    r = _stream(tmp_path, l1_kw={"tx_underruns": "4"}, l2_kw={"tx_underruns": "4"})
    assert "BAD" not in r.stdout, r.stdout
    assert "tx_underruns=4" in r.stdout


def test_item9_fails_a_priming_tx(tmp_path):
    r = _stream(tmp_path, l2_kw={"tx_priming": "1"})
    assert "BAD (program-dante) tx: tx_priming=1" in r.stdout, r.stdout


def test_item9_fails_a_tx_that_plays_nothing(tmp_path):
    r = _stream(tmp_path, l2_kw={"tx_played": "96000"})
    assert "BAD (program-dante) tx: tx_played did not advance" in r.stdout, r.stdout


def test_item9_notes_a_tx_rate_off_48000(tmp_path):
    r = _stream(tmp_path, l2_kw={"tx_played": "200000"})   # 104000 in 10 s = 10400/s
    assert "BAD" not in r.stdout, r.stdout
    assert "NOTE (program-dante) tx rate 10400 frames/s" in r.stdout, r.stdout


def test_item9_takes_the_rate_across_midnight(tmp_path):
    # review round 1: 23:59:55 -> 00:00:05 is 10 s, 480000 frames = 48000/s, no rate NOTE
    lines = [_start("stream-obs", 1), SPLIT, TXLINE, NETLAT, _status("23:59:55.000", 4800)]
    nxt = _status("00:00:05.000", 484800, tx_played="576000")
    r = _grade(tmp_path, "stream-snv", lines, nxt, TOML_STREAM)
    assert "BAD" not in r.stdout and "tx rate" not in r.stdout, r.stdout


def test_item9_fails_a_tx_device_whose_line_has_no_tx(tmp_path):
    r = _stream(tmp_path, l1_kw={"tx_track": "0"}, l2_kw={"tx_track": "0"})
    assert "BAD (program-dante) tx: tx_track=0" in r.stdout, r.stdout


def test_item9_fails_a_tx_device_without_its_tx_start_line(tmp_path):
    r = _stream(tmp_path, extra=(SPLIT, NETLAT))
    assert "BAD (program-dante) no 'dante-input start: TX of OBS track 1' line" in r.stdout, r.stdout


def test_item9_fails_a_changed_dante_latency(tmp_path):
    split = SPLIT.replace("13041666", "8365000")
    r = _stream(tmp_path, extra=(split, TXLINE, NETLAT))
    assert "BAD (program-dante) dante latency 8365000 ns" in r.stdout and "13041666" in r.stdout, r.stdout


def test_item9_fails_a_start_without_the_latency_split(tmp_path):
    r = _stream(tmp_path, extra=(TXLINE, NETLAT))
    assert "BAD (program-dante) no 'dante-input start: latency split' line" in r.stdout, r.stdout


@pytest.mark.parametrize("toml", [
    TOML_STREAM.replace("receive_buffer_ms = 12.041666\n", ""),
    TOML_STREAM.replace("network_latency_ms = 1.0", "network_latency_ms = 2.0"),
    TOML_STREAM.replace("tx_track = 1\n", ""),
    TOML_STREAM.replace('tx = ["OBS program L", "OBS program R"]\n', ""),
])
def test_item9_fails_a_toml_whose_terms_differ_from_the_facts(tmp_path, toml):
    r = _stream(tmp_path, toml=toml)
    assert "BAD (program-dante) latency terms" in r.stdout and "setup-strih" in r.stdout, r.stdout


def test_item9_names_dlinks_own_latency_line(tmp_path):
    line = DLINK_DEFAULT.replace(" (default)", "")
    r = _stream(tmp_path, extra=(SPLIT, line, TXLINE, NETLAT))
    assert "BAD" not in r.stdout, r.stdout
    assert ("NOTE (program-dante) dlink latency: Dante latency P 13.041666 ms = network latency (a) 1 ms + "
            "receive buffer (b) 12.041666 ms") in r.stdout, r.stdout


def test_item9_names_a_default_term_while_the_facts_declare_it(tmp_path):
    # the live 13.11.2026 shape: OBS started before the TOML carried the terms
    r = _stream(tmp_path, extra=(SPLIT, DLINK_DEFAULT, TXLINE, NETLAT))
    assert "NOTE (program-dante) dlink latency:" in r.stdout and "(default)" in r.stdout, r.stdout
    assert "a term reads (default) while the box facts declare it: restart OBS" in r.stdout, r.stdout


def test_item9_notes_a_log_without_network_latency_fields(tmp_path):
    r = _stream(tmp_path, extra=(SPLIT, TXLINE))
    assert "NOTE (program-dante) network latency (report-only): no network_latency_p99_us field" in r.stdout, r.stdout


def _strih(tmp_path, l1_kw=None, l2_kw=None, tx=True, toml=TOML_STRIH):
    lines = [_start("strih-obs", 0), SPLIT, NETLAT, _status("12:00:10.000", 4800, tx=tx, **dict({"tx_track": "0", "tx_played": "0"} if tx else {}, **(l1_kw or {})))]
    nxt = _status("12:00:20.000", 484800, tx=tx, **dict({"tx_track": "0", "tx_played": "0"} if tx else {}, **(l2_kw or {})))
    return _grade(tmp_path, "strih-snv", lines, nxt, toml)


def test_item9_passes_an_rx_only_device_with_zero_tx_tokens(tmp_path):
    r = _strih(tmp_path)
    assert "BAD" not in r.stdout, r.stdout
    assert "OK (program-dante) tx: none on this device (tx_track=0)" in r.stdout, r.stdout


def test_item9_fails_an_rx_only_device_that_sends(tmp_path):
    r = _strih(tmp_path, l2_kw={"tx_track": "1"})
    assert "BAD (program-dante) tx: tx_track=1 on a device whose facts declare no TX" in r.stdout, r.stdout


def test_item9_notes_an_older_line_without_tx_tokens(tmp_path):
    r = _strih(tmp_path, tx=False)
    assert "BAD" not in r.stdout, r.stdout
    assert "NOTE (program-dante) tx: the status line carries no tx_track" in r.stdout, r.stdout


def test_the_new_rows_stay_out_of_verify_strih():
    # verify-strih.sh is at its 1000-line budget: the rows live in the lib's grade
    assert len((SCRIPTS / "verify-strih.sh").read_text().splitlines()) <= 1000
    assert len((SCRIPTS / "lib" / "strih-box-facts.sh").read_text().splitlines()) <= 1000
    assert len((SCRIPTS / "lib" / "strih-dante-input.sh").read_text().splitlines()) <= 1000
