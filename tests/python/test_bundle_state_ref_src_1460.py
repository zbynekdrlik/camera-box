"""issue 1460 -- the :8899 server's A/V reference source follows the box's own seed manifest.

The #1265 ts_lag band and the #1325 buffered_ms series watch ONE reference source. It was `mbc`
everywhere, so on stream-pp (program audio `Zvuk z mixu`) the band read UNKNOWN forever. Now:
AUDIO_REF_BAND_SRC wins, else a STREAM-role seed manifest's `program_audio_input` (the manifest
setup-strih writes; only the stream role carries `dev_scene`), else `mbc`. Values below are the
manifests read live 28.10.2026 on stream-pp / stream-snv / strih-snv.
"""

import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

_STREAM_PP = {"mode": "update-only", "dev_scene": {"scene": "Development"},
              "program_audio_input": "Zvuk z mixu", "inputs": []}
_STREAM_SNV = dict(_STREAM_PP, program_audio_input="mbc")
_STRIH_SNV = {"mode": "update-only", "program_audio_input": "ASIO zvuk", "inputs": []}


def _manifest(tmp_path, obj):
    p = tmp_path / "strih-seed.json"
    p.write_text(json.dumps(obj) if not isinstance(obj, str) else obj, encoding="utf-8")
    return str(p)


def test_the_stream_pp_manifest_names_its_program_audio(tmp_path):
    assert bsg.audio_ref_band_src({}, _manifest(tmp_path, _STREAM_PP)) == "Zvuk z mixu"


def test_the_stream_snv_manifest_keeps_mbc(tmp_path):
    assert bsg.audio_ref_band_src({}, _manifest(tmp_path, _STREAM_SNV)) == "mbc"


def test_a_strih_manifest_is_not_the_av_reference(tmp_path):
    # strih-snv's program input is the cutter's ASIO capture, not the stream A/V reference.
    assert bsg.audio_ref_band_src({}, _manifest(tmp_path, _STRIH_SNV)) == "mbc"


def test_the_env_wins_over_the_manifest(tmp_path):
    env = {"AUDIO_REF_BAND_SRC": "other"}
    assert bsg.audio_ref_band_src(env, _manifest(tmp_path, _STREAM_PP)) == "other"


def test_no_manifest_falls_back_to_mbc(tmp_path):
    assert bsg.audio_ref_band_src({}, str(tmp_path / "absent.json")) == "mbc"


def test_a_broken_or_odd_manifest_falls_back_never_raises(tmp_path):
    for obj in ("{not json", "[1, 2]", dict(_STREAM_PP, program_audio_input="none"),
                dict(_STREAM_PP, program_audio_input="  "), dict(_STREAM_PP, program_audio_input=7)):
        assert bsg.audio_ref_band_src({}, _manifest(tmp_path, obj)) == "mbc", obj


def test_the_server_reads_it_through_the_facade():
    src = (_SCRIPTS / "bundle-state-server.py").read_text(encoding="utf-8")
    assert "bsg.audio_ref_band_src(os.environ)" in src
    assert 'os.environ.get("AUDIO_REF_BAND_SRC"' not in src
