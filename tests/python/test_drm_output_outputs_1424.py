"""issue 1424 part 2 -- two fixed HDMI outputs at once (owner ROZHODNUTE 13.10.2026, answer "1"; main
design comment 6150029770): strih-pp drives HDMI-0 = the cutter's Multiview AND HDMI-1 = the vestibule
Program + its audio. The box side:

- the Python mirror of the config grammar (strih_scenes.drm_output_instances) against the SAME table
  the vendored C is lift-tested on (tests/fixtures/drm_output_outputs_parity.tsv, tests/
  drm_output_outputs_1424.rs); imag's classifier agrees on every row;
- the launcher takes EVERY armed connector out of X;
- the box fact STRIH_HDMI_OUTPUTS (strih-pp `HDMI-0:multiview HDMI-1:program`, strih-snv `auto`,
  stream-pp `none`) and setup-strih step 6 writing / migrating the "outputs" array without breaking the
  operator's persisted views;
- verify-strih item 4c grading every output on its own log lines (open line + audio line).

Every value reaches bash as a positional argument, never inside the script text; every helper runs
under the caller's real `set -euo pipefail`. Tier-0: python + bash subprocesses only.
"""
import importlib.util
import json
import os
import pathlib
import site
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_TSV = _ROOT / "tests" / "fixtures" / "drm_output_outputs_parity.tsv"
_FACTS = _SCRIPTS / "lib" / "strih-box-facts.sh"
_DRM_LIB = _SCRIPTS / "lib" / "strih-drm-output.sh"
_VERIFY = _SCRIPTS / "verify-strih.sh"
_SETUP = _SCRIPTS / "setup-strih.sh"
_START = _SCRIPTS / "strih-obs-start.sh"
_SCN = _SCRIPTS / "strih_scenes.py"
_CFG = _SCRIPTS / "drm_output_config.py"

PP_LIVE = '{"enabled":true,"connector":"HDMI-0","argb":2105376,"view":"multiview","backend":"vk-direct"}'
PP_ARRAY = ('{"enabled":true,"outputs":[{"connector":"HDMI-0","argb":2105376,"view":"multiview",'
            '"backend":"vk-direct"},{"connector":"HDMI-1","argb":2105376,"view":"program","backend":"vk-direct"}]}')
PP_SPEC = "HDMI-0:multiview HDMI-1:program"


def _load(name):
    spec = importlib.util.spec_from_file_location(name, _SCRIPTS / ("%s.py" % name))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_scn = _load("strih_scenes")


def _rows():
    rows = []
    for line in _TSV.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        name, config, verdict, _bad, outputs = line.split("\t")
        rows.append((name, "" if config == "<empty>" else config, verdict, outputs))
    assert len(rows) >= 40
    return rows


def _fmt(outputs):
    if not outputs:
        return "-"
    return ";".join("%s|%s|%s|%d%s" % (o["connector"], o["view"], o["backend"], o["argb"],
                                       "" if o["audio"] else "|noaudio") for o in outputs)


# --- the grammar: ONE table with the C --------------------------------------------------------------

@pytest.mark.parametrize("name,config,verdict,outputs", _rows(), ids=[r[0] for r in _rows()])
def test_python_grammar_matches_the_shared_c_table(name, config, verdict, outputs):
    got_verdict, got = _scn.drm_output_instances(config)
    assert (got_verdict, _fmt(got)) == (verdict, outputs), name


def test_lease_connectors_arm_every_output_of_a_valid_array_and_none_of_a_malformed_one():
    assert _scn.drm_output_lease_connectors(PP_ARRAY) == ["HDMI-0", "HDMI-1"]
    assert _scn.drm_output_lease_connectors(PP_LIVE) == ["HDMI-0"]
    bad = PP_ARRAY.replace('"view":"program"', '"view":"Program"')
    assert _scn.drm_output_lease_connectors(bad) == []
    assert _scn.drm_output_lease_connector(PP_ARRAY) == "HDMI-0"
    assert _scn.drm_output_lease_connector(bad) == ""


# imag_scenes reads with Python's lenient json (no NaN / 64-bit / overflow check); imag's config is a
# machine-written single object, so only these jansson-refusal rows are left out of the imag comparison.
_STRICT_JSON_ROWS = ("strict_",)


def test_imag_classifier_agrees_with_strih_on_every_table_row():
    imag = _load("imag_scenes")
    for name, config, _verdict, _outputs in _rows():
        if name.startswith(_STRICT_JSON_ROWS):
            continue
        assert _scn.drm_output_lease_connector(config) == imag.drm_output_lease_connector(config), name


def test_the_lease_connector_is_the_first_output_the_c_grammar_starts_on_every_row():
    # issue 1424 review: the single-object connector obeys the C's 1..63-byte bound too
    for name, config, _verdict, _outputs in _rows():
        armed = _scn.drm_output_lease_connectors(config)
        assert _scn.drm_output_lease_connector(config) == (armed[0] if armed else ""), name


def test_a_byte_order_mark_reads_like_the_c_reader():
    # the C reads the file through OBS's UTF-8 reader, which drops a BOM (the issue-1152/1346 loader did)
    for name, config, verdict, outputs in _rows():
        if config:
            got_verdict, got = _scn.drm_output_instances("\ufeff" + config)
            assert (got_verdict, _fmt(got)) == (verdict, outputs), name
    imag = _load("imag_scenes")
    assert imag.drm_output_lease_connector("\ufeff" + PP_LIVE) == "HDMI-0"


def test_the_grammar_lives_in_its_own_module_and_strih_scenes_re_exports_it():
    # issue 1424 review: strih_scenes.py stays under its line budget; ONE module carries the grammar
    assert len(_SCN.read_text().splitlines()) <= 1000
    assert len(_CFG.read_text().splitlines()) <= 1000
    cfg = _load("drm_output_config")
    for name in ("drm_output_instances", "drm_output_lease_connector", "drm_output_lease_connectors",
                 "write_drm_view", "write_drm_backend", "write_drm_outputs", "drm_json_loads"):
        assert getattr(_scn, name).__code__.co_code == getattr(cfg, name).__code__.co_code, name
    assert "def drm_output_instances(" not in _SCN.read_text()
    assert "from websocket" not in _CFG.read_text(), "the grammar module carries no OBS/WS dependency"


def test_setup_installs_the_grammar_module_beside_strih_scenes():
    s = _SETUP.read_text()
    line = [ln for ln in s.splitlines() if 'install -m 0755 "${HERE}/strih_scenes.py"' in ln]
    assert len(line) == 1
    assert 'install -m 0644 "${HERE}/drm_output_config.py" /usr/local/bin/drm_output_config.py' in line[0]


def test_the_launcher_takes_every_armed_connector_out_of_x():
    w = _START.read_text()
    classify = w.index("strih_scenes.drm_output_lease_connectors(")
    loop = w.index('for DRM_CONNECTOR in $DRM_CONNECTORS; do')
    off = w.index('xrandr --output "$DRM_CONNECTOR" --off')
    launch = w.index("OBS_PID=$!")
    assert classify < loop < off < launch


# --- the persisted views -----------------------------------------------------------------------------

def test_write_view_sets_only_the_named_output_of_an_array(tmp_path):
    p = tmp_path / "drm-output.json"
    p.write_text(PP_ARRAY + "\n")
    _scn.write_drm_view("multiview", str(p), "HDMI-1")
    assert p.read_text() == PP_ARRAY.replace('"view":"program"', '"view":"multiview"') + "\n"
    with pytest.raises(ValueError, match="name one with --connector"):
        _scn.write_drm_view("program", str(p))
    with pytest.raises(ValueError, match="drives no output 'DP-2'"):
        _scn.write_drm_view("program", str(p), "DP-2")


def test_write_view_on_the_single_form_keeps_today_s_behaviour(tmp_path):
    p = tmp_path / "drm-output.json"
    p.write_text(PP_LIVE + "\n")
    _scn.write_drm_view("program", str(p))
    assert json.loads(p.read_text())["view"] == "program"
    with pytest.raises(ValueError, match="drives 'HDMI-0', not 'HDMI-1'"):
        _scn.write_drm_view("program", str(p), "HDMI-1")


def test_write_backend_writes_the_box_fact_into_every_entry(tmp_path):
    p = tmp_path / "drm-output.json"
    p.write_text(PP_ARRAY + "\n")
    _scn.write_drm_backend("lease", str(p))
    cfg = json.loads(p.read_text())
    assert all("backend" not in e for e in cfg["outputs"])
    _scn.write_drm_backend("vk-direct", str(p))
    assert p.read_text() == PP_ARRAY + "\n"


def test_write_outputs_migrates_the_live_strih_pp_config_keeping_the_operator_view(tmp_path):
    p = tmp_path / "drm-output.json"
    p.write_text(PP_LIVE + "\n")
    _scn.write_drm_outputs(PP_SPEC, "vk-direct", str(p))
    assert p.read_text() == PP_ARRAY + "\n"
    # a second run changes nothing (idempotent)
    _scn.write_drm_outputs(PP_SPEC, "vk-direct", str(p))
    assert p.read_text() == PP_ARRAY + "\n"


def test_write_outputs_keeps_an_operator_view_that_differs_from_the_fact_default(tmp_path):
    p = tmp_path / "drm-output.json"
    flipped = PP_ARRAY.replace('"view":"multiview"', '"view":"X"').replace('"view":"program"', '"view":"multiview"')
    flipped = flipped.replace('"view":"X"', '"view":"program"')
    p.write_text(flipped + "\n")
    _scn.write_drm_outputs(PP_SPEC, "vk-direct", str(p))
    cfg = json.loads(p.read_text())
    assert [(e["connector"], e["view"]) for e in cfg["outputs"]] == [("HDMI-0", "program"), ("HDMI-1", "multiview")]


def test_write_outputs_keeps_other_top_level_keys_and_an_entry_s_argb(tmp_path):
    p = tmp_path / "drm-output.json"
    p.write_text('{"enabled":true,"program":false,"connector":"HDMI-1","argb":0,"view":"program"}\n')
    _scn.write_drm_outputs(PP_SPEC, "lease", str(p))
    assert p.read_text() == (
        '{"enabled":true,"program":false,"outputs":[{"connector":"HDMI-0","argb":2105376,"view":"multiview"},'
        '{"connector":"HDMI-1","argb":0,"view":"program"}]}\n')


@pytest.mark.parametrize("spec", ["auto", "none", "HDMI-0", "HDMI-0:Program", "HDMI-0:program HDMI-0:multiview",
                                  "A:program B:program C:program", "HDMI 0:program"])
def test_write_outputs_refuses_a_spec_that_is_no_connector_list(tmp_path, spec):
    p = tmp_path / "drm-output.json"
    p.write_text(PP_LIVE + "\n")
    with pytest.raises(ValueError):
        _scn.write_drm_outputs(spec, "vk-direct", str(p))
    assert p.read_text() == PP_LIVE + "\n"


def test_projector_cli_names_the_connector(tmp_path):
    home = tmp_path / "home"
    (home / ".camera-box").mkdir(parents=True)
    conf = home / ".camera-box" / "drm-output.json"
    conf.write_text(PP_ARRAY + "\n")
    env = dict(os.environ, HOME=str(home), PYTHONUSERBASE=site.getuserbase())
    r = subprocess.run([sys.executable, str(_SCN), "--projector", "multiview", "--connector", "HDMI-1"],
                       capture_output=True, text=True, env=env, check=False)
    assert r.returncode == 0, r.stderr
    assert "on HDMI-1" in r.stdout
    assert json.loads(conf.read_text())["outputs"][1]["view"] == "multiview"
    r = subprocess.run([sys.executable, str(_SCN), "--projector", "program"],
                       capture_output=True, text=True, env=env, check=False)
    assert r.returncode != 0 and "name one with --connector" in r.stderr


# --- bash: the fact, the fresh config, the verify grading ---------------------------------------------

def _bash(body, *args, env=None, path_prefix=None, libs=(_FACTS, _DRM_LIB)):
    script = "set -euo pipefail\n"
    for i in range(len(libs)):
        script += '. "${%d}"\n' % (i + 1)
    script += "shift %d\n" % len(libs) + body + "\n"
    e = dict(os.environ, LC_ALL="C.UTF-8", PYTHONUSERBASE=site.getuserbase())
    if env:
        e.update(env)
    if path_prefix:
        e["PATH"] = f"{path_prefix}:{e['PATH']}"
    return subprocess.run(["bash", "-c", script, "harness", *[str(x) for x in libs], *args],
                          capture_output=True, text=True, env=e, check=False)


def test_bash_config_json_is_byte_identical_to_the_python_writer(tmp_path):
    r = _bash('strih_drm_outputs_config_json "$1" "$2"', PP_SPEC, "vk-direct")
    assert r.returncode == 0, r.stderr
    assert r.stdout == PP_ARRAY + "\n"
    p = tmp_path / "drm-output.json"
    p.write_text(r.stdout)
    _scn.write_drm_outputs(PP_SPEC, "vk-direct", str(p))
    assert p.read_text() == r.stdout
    r = _bash('strih_drm_outputs_config_json "$1" lease', "HDMI-1:program")
    assert r.stdout == '{"enabled":true,"outputs":[{"connector":"HDMI-1","argb":2105376,"view":"program"}]}\n'
    assert _scn.drm_output_instances(r.stdout)[0] == "ok"


@pytest.mark.parametrize("spec", ["", "auto", "HDMI-0", "HDMI-0:Program", "HDMI-0:program HDMI-0:program",
                                  "A:program B:multiview C:program", "HDMI-0:program;x"])
def test_bash_config_json_refuses_a_bad_spec(spec):
    r = _bash('strih_drm_outputs_config_json "$1" vk-direct || echo "RC=$?"', spec)
    assert r.stdout == "RC=1\n"
    r = _bash('strih_drm_outputs_config_json "$1" vulkan || echo "RC=$?"', PP_SPEC)
    assert r.stdout == "RC=1\n"


XRANDR = """Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767
DP-2 connected primary 1920x1080+0+0 (normal left inverted right x axis y axis) 600mm x 340mm
HDMI-0 connected (normal left inverted right x axis y axis)
HDMI-1 disconnected (normal left inverted right x axis y axis)
"""


def test_xrandr_connected_names_one_output():
    body = ('for c in HDMI-0 HDMI-1 DP-2 HDMI DP-3; do '
            'if printf "%s" "$1" | strih_drm_xrandr_connected "$c"; then echo "$c yes"; else echo "$c no"; fi; done')
    r = _bash(body, XRANDR)
    assert r.stdout.split("\n")[:5] == ["HDMI-0 yes", "HDMI-1 no", "DP-2 yes", "HDMI no", "DP-3 no"]


def _facts(tmp_path, box, old, new):
    body = (_SCRIPTS / "strih-boxes" / ("%s.env" % box)).read_text()
    assert body.count(old) == 1, old
    d = tmp_path / "boxes"
    d.mkdir(exist_ok=True)
    for f in (_SCRIPTS / "strih-boxes").glob("*.env"):
        (d / f.name).write_text(f.read_text())
    (d / ("%s.env" % box)).write_text(body.replace(old, new))
    return {"STRIH_BOXES_DIR": str(d)}


def test_the_box_facts_list_the_outputs():
    r = _bash('for b in strih-pp strih-snv stream-pp; do ( strih_box_load "$b" && printf "%s=%s\\n" "$b" '
              '"$(strih_lx_hdmi_outputs)" ) || echo "$b FAILED"; done', libs=(_FACTS,))
    assert "strih-pp=HDMI-0:multiview HDMI-1:program:noaudio" in r.stdout, r.stdout + r.stderr
    assert "strih-snv=auto" in r.stdout
    assert "stream-pp=none" in r.stdout


@pytest.mark.parametrize("value", ["HDMI-0", "HDMI-0:Program", "HDMI-0:program HDMI-0:multiview",
                                   "A:program B:program C:program", "none", "", "HDMI-0:program:audio",
                                   "HDMI-0:program:noaudio:x", "HDMI-0:noaudio", "HDMI-0:program:"])
def test_the_fact_loader_refuses_a_bad_outputs_list(tmp_path, value):
    env = _facts(tmp_path, "strih-pp", "STRIH_HDMI_OUTPUTS=HDMI-0:multiview HDMI-1:program:noaudio",
                 "STRIH_HDMI_OUTPUTS=%s" % value)
    r = _bash('strih_box_load strih-pp && echo LOADED', env=env, libs=(_FACTS,))
    assert "LOADED" not in r.stdout
    assert "STRIH_HDMI_OUTPUTS" in r.stderr


def test_a_box_without_a_fixed_output_lists_none(tmp_path):
    env = _facts(tmp_path, "stream-pp", "STRIH_HDMI_OUTPUTS=none", "STRIH_HDMI_OUTPUTS=auto")
    r = _bash('strih_box_load stream-pp && echo LOADED', env=env, libs=(_FACTS,))
    assert "LOADED" not in r.stdout and "must be none exactly when" in r.stderr


PP_FACT = "HDMI-0:multiview HDMI-1:program:noaudio"
PP_FACT_ARRAY = PP_ARRAY.replace('"view":"program","backend":"vk-direct"}',
                                 '"view":"program","backend":"vk-direct","audio":false}')


def test_the_noaudio_fact_is_one_grammar_in_bash_and_python():
    r = _bash('strih_box_hdmi_outputs_ok "$1" && echo OK', PP_FACT)
    assert r.stdout == "OK\n"
    assert _scn.drm_output_spec_of(PP_FACT) == [("HDMI-0", "multiview", True), ("HDMI-1", "program", False)]
    for bad in ("HDMI-0:program:audio", "HDMI-0:program:noaudio:x", "HDMI-0:noaudio", "HDMI-0:program:"):
        r = _bash('strih_box_hdmi_outputs_ok "$1" || echo BAD', bad)
        assert r.stdout == "BAD\n", bad
        with pytest.raises(ValueError):
            _scn.drm_output_spec_of(bad)
    r = _bash('for c in HDMI-0 HDMI-1 DP-1; do strih_drm_outputs_spec_audio "$1" "$c"; echo; done', PP_FACT)
    assert r.stdout == "on\noff\non\n"


def test_the_noaudio_fact_writes_audio_false_byte_identically_in_bash_and_python(tmp_path):
    r = _bash('strih_drm_outputs_config_json "$1" vk-direct', PP_FACT)
    assert r.stdout == PP_FACT_ARRAY + "\n", r.stdout
    assert _scn.drm_output_instances(r.stdout)[1][1]["audio"] is False
    p = tmp_path / "drm-output.json"
    p.write_text(PP_LIVE + "\n")  # the live single-object strih-pp config migrates onto the fact
    _scn.write_drm_outputs(PP_FACT, "vk-direct", str(p))
    assert p.read_text() == PP_FACT_ARRAY + "\n"
    p.write_text(PP_FACT_ARRAY + "\n")  # the fact decides the audio, never the old config
    _scn.write_drm_outputs(PP_SPEC, "vk-direct", str(p))
    assert p.read_text() == PP_ARRAY + "\n"


def _fake_bin(tmp_path, xrandr_text):
    d = tmp_path / "bin"
    d.mkdir(exist_ok=True)
    (d / "xr.txt").write_text(xrandr_text)
    for name, body in [
        ("xrandr", '#!/bin/bash\ncat "%s/xr.txt"\n' % d),
        ("sudo", '#!/bin/bash\n[ "$1" = -u ] && shift 2\nexec "$@"\n'),
        ("id", '#!/bin/bash\necho 0\n'),
        ("apt-get", "#!/bin/bash\nexit 0\n"),
        ("wireplumber", "#!/bin/bash\nexit 0\n"),
        # install without -o/-g (non-root test): drop the ownership flags, keep the rest
        ("install", '#!/bin/bash\nargs=()\nwhile [ $# -gt 0 ]; do case "$1" in -o|-g) shift 2;; *) args+=("$1"); shift;; esac; done\n'
                    'exec /usr/bin/install "${args[@]}"\n'),
    ]:
        f = d / name
        f.write_text(body)
        f.chmod(0o755)
    return str(d)


_PROV = ('warn() { echo "WARN $*"; }; fail() { echo "FAIL $*"; exit 1; }\n'
         # issue 1424 layer 1: the X config step 6 also writes goes under the scratch dir
         'STRIH_X_CONF_DIR="$1/xorg.conf.d" STRIH_LEGACY_PROJ="$1/none.json" '
         'strih_drm_output_provision "$1/home" "$(whoami)" "$2" vk-direct "$3"\n')


def test_step6_writes_the_array_for_a_fresh_two_output_box(tmp_path):
    (tmp_path / "home").mkdir()
    fake = _fake_bin(tmp_path, XRANDR.replace("HDMI-1 disconnected", "HDMI-1 connected"))
    r = _bash(_PROV, str(tmp_path), str(_SCRIPTS), PP_SPEC, path_prefix=fake)
    assert r.returncode == 0, r.stdout + r.stderr
    conf = tmp_path / "home" / ".camera-box" / "drm-output.json"
    assert conf.read_text() == PP_ARRAY + "\n"
    assert "wrote" in r.stdout and "HDMI outputs HDMI-0:multiview HDMI-1:program" in r.stdout
    # issue 1424 layer 1: and the X config that keeps both fixed outputs out of X (DP-2 = the operator's)
    xconf = (tmp_path / "xorg.conf.d" / "15-camera-box-fixed-hdmi.conf").read_text()
    assert 'Option "MetaModes" "DP-2: nvidia-auto-select"' in xconf, r.stdout


def test_step6_writes_the_array_and_warns_when_one_listed_connector_is_unplugged(tmp_path):
    (tmp_path / "home").mkdir()
    fake = _fake_bin(tmp_path, XRANDR)
    r = _bash(_PROV, str(tmp_path), str(_SCRIPTS), PP_SPEC, path_prefix=fake)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARN   issue 1424: HDMI-1 is not connected in X RandR" in r.stdout
    assert (tmp_path / "home" / ".camera-box" / "drm-output.json").read_text() == PP_ARRAY + "\n"


def test_step6_skips_when_no_listed_connector_is_connected(tmp_path):
    (tmp_path / "home").mkdir()
    fake = _fake_bin(tmp_path, XRANDR.replace("HDMI-0 connected", "HDMI-0 disconnected"))
    r = _bash(_PROV, str(tmp_path), str(_SCRIPTS), PP_SPEC, path_prefix=fake)
    assert "SKIP issue 1424: none of" in r.stdout
    assert not (tmp_path / "home" / ".camera-box" / "drm-output.json").exists()


def test_step6_migrates_the_live_strih_pp_config_keeping_the_operator_view(tmp_path):
    camera = tmp_path / "home" / ".camera-box"
    camera.mkdir(parents=True)
    (camera / "drm-output.json").write_text(PP_LIVE.replace('"view":"multiview"', '"view":"program"') + "\n")
    fake = _fake_bin(tmp_path, XRANDR)
    r = _bash(_PROV, str(tmp_path), str(_SCRIPTS), PP_SPEC, path_prefix=fake)
    assert r.returncode == 0, r.stdout + r.stderr
    cfg = json.loads((camera / "drm-output.json").read_text())
    assert [(e["connector"], e["view"]) for e in cfg["outputs"]] == [("HDMI-0", "program"), ("HDMI-1", "program")]
    assert "the operator's views kept" in r.stdout


def test_step6_on_an_auto_box_keeps_today_s_single_output_write(tmp_path):
    camera = tmp_path / "home" / ".camera-box"
    camera.mkdir(parents=True)
    (camera / "drm-output.json").write_text(PP_LIVE + "\n")
    fake = _fake_bin(tmp_path, XRANDR)
    r = _bash(_PROV, str(tmp_path), str(_SCRIPTS), "auto", path_prefix=fake)
    assert r.returncode == 0, r.stdout + r.stderr
    assert (camera / "drm-output.json").read_text() == PP_LIVE + "\n"


LOG_TWO = """info: drm-output: autostart ENABLED from /home/op/.camera-box/drm-output.json — 2 outputs
info: drm-output: program scanout LIVE (vk-direct: a published frame reached 'HDMI-0')
info: drm-output: multiview bind LIVE (built-in Multiview rendered at 2560x1440 and blitted into the scanout buffer of 'HDMI-0')
info: drm-output audio: 'HDMI-0' matched hw:0,3 (48000 Hz, S32_LE, IEC958 switch on) -- mix 0 connected (muted while the view is multiview)
info: drm-output: program scanout LIVE (vk-direct: a published frame reached 'HDMI-1')
info: drm-output audio: pcm hw:0,7 card 0 device 7 delay 40.0 ms (on 'HDMI-1'; audio 52.0 ms behind the picture, present 30.0 ms, pcm delay 40.0 ms, 48000 Hz, IEC958 switch on)
"""


def _verify_real(tmp_path, log_text, conf_text, spec=PP_SPEC, xrandr=None):
    xrandr = xrandr if xrandr is not None else XRANDR.replace("HDMI-1 disconnected", "HDMI-1 connected")
    home = tmp_path / "home"
    (home / ".camera-box").mkdir(parents=True, exist_ok=True)
    conf = home / ".camera-box" / "drm-output.json"
    conf.write_text(conf_text)
    log = tmp_path / "obs.txt"
    log.write_text(log_text)
    fake = _fake_bin(tmp_path, xrandr)
    (pathlib.Path(fake) / "wireplumber").write_text("#!/bin/bash\necho 'Linked with libwireplumber 0.5.13'\n")
    pw = pathlib.Path(fake) / "pw-dump"
    pw.write_text("#!/bin/bash\necho '[]'\n")
    pw.chmod(0o755)
    root = tmp_path / "root"
    body = ('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }\n'
            'export SPEC_FACT="$6"; strih_lx_hdmi_outputs() { printf "%s" "$SPEC_FACT"; }\n'
            'drm_output_audio_wp_provision "$7" >/dev/null\n'
            'eval "$(declare -f drm_output_audio_verify_report | sed "1s/drm_output_audio_verify_report/_real_audio_report/")"\n'
            'drm_output_audio_verify_report() { _real_audio_report "$1" "$2" "$3" "$AUDIO_ROOT" "$5" "${6:-}"; }\n'
            'export AUDIO_ROOT="$7"\n'
            'if strih_drm_outputs_graded_here "$3"; then echo GRADED-HERE; fi\n'
            'strih_drm_outputs_verify_report "$1" "$2" "$3" "$4" vk-direct "$5"\n')
    return _bash(body, str(home), str(_SCN), str(conf), str(log), xrandr, spec, str(root), path_prefix=fake)


def test_verify_grades_both_outputs_and_both_audio_legs(tmp_path):
    r = _verify_real(tmp_path, LOG_TWO, PP_ARRAY + "\n")
    assert r.returncode == 0, r.stderr
    out = r.stdout.splitlines()
    assert out[0] == "GRADED-HERE"
    assert "PASS HDMI output HDMI-0: live (backend vk-direct), view multiview" in r.stdout
    assert "PASS HDMI output HDMI-1: live (backend vk-direct), view program" in r.stdout
    assert any(ln.startswith("PASS HDMI audio HDMI-0:") and "matched hw:0,3" in ln for ln in out), r.stdout
    assert any(ln.startswith("PASS HDMI audio HDMI-1:") and "pcm hw:0,7" in ln for ln in out), r.stdout
    assert "FAIL" not in r.stdout


def test_verify_never_lets_one_output_s_lines_grade_the_other(tmp_path):
    log = "\n".join(ln for ln in LOG_TWO.splitlines() if "'HDMI-1'" not in ln) + "\n"
    r = _verify_real(tmp_path, log, PP_ARRAY + "\n")
    assert "PASS HDMI output HDMI-0:" in r.stdout
    assert "FAIL HDMI output HDMI-1: armed but the newest OBS log has no 'drm-output: program scanout LIVE' line" in r.stdout


def test_verify_requires_the_multiview_bind_of_the_multiview_output(tmp_path):
    log = "\n".join(ln for ln in LOG_TWO.splitlines() if "multiview bind LIVE" not in ln) + "\n"
    r = _verify_real(tmp_path, log, PP_ARRAY + "\n")
    assert "FAIL HDMI output HDMI-0: the view is multiview but" in r.stdout
    assert "PASS HDMI output HDMI-1:" in r.stdout


def test_verify_reads_a_dead_present_loop_per_output(tmp_path):
    log = LOG_TWO + "info: drm-output: vk-direct present loop exited on 'HDMI-1' after 900 presents (0 publishes skipped behind a ready frame)\n"
    r = _verify_real(tmp_path, log, PP_ARRAY + "\n")
    assert "PASS HDMI output HDMI-0:" in r.stdout
    assert "FAIL HDMI output HDMI-1: its vk-direct present loop died" in r.stdout


def test_verify_names_an_audio_leg_that_gave_up_on_its_own_output(tmp_path):
    log = LOG_TWO + "warning: drm-output audio: NO AUDIO on 'HDMI-1' -- no PCM carries the monitor; never another device\n"
    r = _verify_real(tmp_path, log, PP_ARRAY + "\n")
    assert any(ln.startswith("PASS HDMI audio HDMI-0:") for ln in r.stdout.splitlines()), r.stdout
    assert "FAIL HDMI audio HDMI-1: the module gave up" in r.stdout


def test_a_multiview_output_whose_monitor_has_no_audio_is_report_only(tmp_path):
    # issue 1424 review: the Multiview output never plays sound, so its leg giving up is a NOTE
    log = LOG_TWO + "warning: drm-output audio: NO AUDIO on 'HDMI-0' -- no PCM carries the monitor; never another device\n"
    r = _verify_real(tmp_path, log, PP_ARRAY + "\n")
    assert "NOTE HDMI audio HDMI-0: no audio leg" in r.stdout, r.stdout
    assert "FAIL HDMI audio HDMI-0" not in r.stdout
    assert any(ln.startswith("PASS HDMI audio HDMI-1:") for ln in r.stdout.splitlines()), r.stdout


LOG_HUB = "\n".join(ln for ln in LOG_TWO.splitlines() if "pcm hw:0,7" not in ln) + (
    "\ninfo: drm-output audio: 'HDMI-1' off by config (owned elsewhere)\n")


def test_verify_grades_the_off_by_config_line_of_an_output_whose_pcm_the_hub_owns(tmp_path):
    r = _verify_real(tmp_path, LOG_HUB, PP_FACT_ARRAY + "\n", spec=PP_FACT)
    assert "PASS HDMI audio HDMI-1: off by config" in r.stdout, r.stdout
    assert any(ln.startswith("PASS HDMI audio HDMI-0:") for ln in r.stdout.splitlines()), r.stdout
    assert "FAIL" not in r.stdout, r.stdout


def test_verify_fails_an_audio_off_output_whose_log_lacks_the_line_or_opened_the_pcm(tmp_path):
    r = _verify_real(tmp_path, LOG_TWO, PP_FACT_ARRAY + "\n", spec=PP_FACT)
    assert "FAIL HDMI audio HDMI-1: the config turns its audio off" in r.stdout, r.stdout
    log = LOG_HUB + LOG_TWO.splitlines()[-1] + "\n"
    r = _verify_real(tmp_path, log, PP_FACT_ARRAY + "\n", spec=PP_FACT)
    assert "FAIL HDMI audio HDMI-1: the config turns its audio off" in r.stdout, r.stdout


def test_verify_fails_a_config_whose_audio_flag_is_not_the_fact(tmp_path):
    r = _verify_real(tmp_path, LOG_TWO, PP_ARRAY + "\n", spec=PP_FACT)
    assert "FAIL HDMI audio HDMI-1: the config has audio on but the box fact STRIH_HDMI_OUTPUTS says off" in r.stdout
    r = _verify_real(tmp_path, LOG_HUB, PP_FACT_ARRAY + "\n", spec=PP_SPEC)
    assert "FAIL HDMI audio HDMI-1: the config has audio off but the box fact STRIH_HDMI_OUTPUTS says on" in r.stdout


def test_the_single_output_grading_passes_the_view_so_a_multiview_monitor_without_audio_is_a_note():
    # review round 2: strih-snv runs the single form with view multiview
    v = _VERIFY.read_text()
    line = [ln for ln in v.splitlines() if ln.lstrip().startswith("ok)") and "drm_output_audio_verify_report" in ln]
    assert len(line) == 1 and '"$DRM_LOG" "" "" "$DRM_VIEW_V"' in line[0], line


def test_setup_names_a_missing_grammar_module():
    s = _SETUP.read_text()
    assert ('[ -f "${HERE}/strih_scenes.py" ] && [ -f "${HERE}/drm_output_config.py" ] || fail' in s)


def test_verify_fails_a_malformed_array_by_name(tmp_path):
    r = _verify_real(tmp_path, LOG_TWO, PP_ARRAY.replace('"view":"program"', '"view":"Program"') + "\n")
    assert 'FAIL HDMI outputs:' in r.stdout and '"outputs" is malformed (view-unknown)' in r.stdout


def test_verify_fails_a_config_that_is_not_on_the_fact_yet(tmp_path):
    r = _verify_real(tmp_path, LOG_TWO, PP_LIVE + "\n")
    assert "GRADED-HERE" in r.stdout
    assert "drives a single output but the box fact STRIH_HDMI_OUTPUTS lists" in r.stdout
    one = '{"enabled":true,"outputs":[{"connector":"HDMI-0","argb":2105376,"view":"multiview","backend":"vk-direct"}]}'
    r = _verify_real(tmp_path, LOG_TWO, one + "\n")
    assert "drives 'HDMI-0' but the box fact STRIH_HDMI_OUTPUTS lists 'HDMI-0 HDMI-1'" in r.stdout


def test_an_auto_box_keeps_the_single_output_grading(tmp_path):
    r = _verify_real(tmp_path, LOG_TWO, PP_LIVE + "\n", spec="auto")
    assert "GRADED-HERE" not in r.stdout


def test_verify_marks_an_output_x_does_not_list_as_report_only(tmp_path):
    r = _verify_real(tmp_path, LOG_TWO, PP_ARRAY + "\n", xrandr=XRANDR)
    assert "NOTE HDMI output HDMI-1: armed in" in r.stdout
    assert "PASS HDMI output HDMI-0:" in r.stdout


def test_verify_strih_dispatches_the_array_form_without_growing_past_its_budget():
    v = _VERIFY.read_text()
    assert len(v.splitlines()) <= 1000 and len(_SETUP.read_text().splitlines()) <= 1000
    assert "strih_drm_outputs_graded_here" not in v, "the dispatch lives in the lib (issue 1424 review)"
    line = [ln for ln in v.splitlines() if "strih_drm_outputs_verify_dispatch" in ln]
    assert len(line) == 1 and len(line[0]) <= 120
    assert "per-output)" in v


def test_the_dispatch_grades_only_an_array_box_and_never_a_box_without_an_output(tmp_path):
    conf = tmp_path / "drm-output.json"
    conf.write_text(PP_ARRAY + "\n")
    body = ('strih_drm_outputs_verify_report() { echo "REPORT $*"; }\n'
            'strih_drm_outputs_verify_dispatch "$1" H S "$2" L F X && echo GRADED || echo SINGLE\n')
    r = _bash(body, "lease-not-live", str(conf))
    assert r.stdout == "REPORT H S %s L F X\nGRADED\n" % conf, r.stdout + r.stderr
    r = _bash(body, "skip-no-output", str(conf))
    assert r.stdout == "SINGLE\n"
    conf.write_text(PP_LIVE + "\n")
    r = _bash('strih_lx_hdmi_outputs() { echo auto; }\n' + body, "ok", str(conf))
    assert r.stdout == "SINGLE\n"


def test_the_drm_lib_has_no_second_copy_of_the_fact_grammar():
    # issue 1424 review: ONE validator, in the facts lib
    d = _DRM_LIB.read_text()
    assert "strih_drm_outputs_spec_ok" not in d
    assert "^${c}+:(program|multiview)$" not in d
    assert "strih_box_hdmi_outputs_ok" in d
    assert "strih_box_hdmi_outputs_ok() {" in _FACTS.read_text()
