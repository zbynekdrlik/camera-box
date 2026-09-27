"""issue 1380 (ROZHODNUTIE 27.9.2026, main) -- scripts/cg-chain-verify.sh without the removed stream input.

The owner removed the stream OBS input `NDI obs hudba` on 27.9.2026. The CG-chain verdict tool's
DEFAULT hop list is now `cg-obs strih`; the stream hop runs only when asked for (`--hops` or the
`CG_CHAIN_HOPS` env), its input name is overridable (`CG_CHAIN_STREAM_SRC`), and when that input has
no `genlock-fifo audit` line in the window the hop reports a named ABSENT row, never a false FAIL.
The strih hop keeps failing on a missing `cg` (that input is expected).
"""
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_TOOL = _ROOT / "scripts" / "cg-chain-verify.sh"


def _audit(src, received):
    return (f"12:00:0{received % 10}.000: genlock-fifo audit '{src}': received={received} "
            f"dropped_due=0 underruns=0 relocks=0 late_holds=0 backward_regime=0 "
            f"ts_head_skew_ms=1 latency_ms=3 locked=1\n")


def _log(tmp_path, name, sources):
    text = "".join(_audit(src, n) for n in (10, 20) for src in sources)
    p = tmp_path / f"{name}.log"
    p.write_text(text)
    return p


def _run(tmp_path, args=(), env_extra=None, logs=None):
    env = {k: v for k, v in os.environ.items() if not k.startswith("CG_CHAIN_")}
    for hop, path in (logs or {}).items():
        env[f"CG_CHAIN_{hop}_LOG"] = str(path)
    env.update(env_extra or {})
    return subprocess.run(["bash", str(_TOOL), *args], capture_output=True, text=True, env=env)


def _logs(tmp_path, stream_sources=("NDI obs hudba",)):
    return {"CG_OBS": _log(tmp_path, "cg", ["sp-1_video"]),
            "STRIH": _log(tmp_path, "strih", ["cg"]),
            "STREAM": _log(tmp_path, "stream", list(stream_sources))}


def test_the_default_hop_list_has_no_stream_hop(tmp_path):
    r = _run(tmp_path, logs=_logs(tmp_path))
    rows = [ln.split()[0] for ln in r.stdout.splitlines() if ln and not ln.startswith(("HOP", "OVERALL"))]
    assert "stream" not in rows, r.stdout
    assert "cg-obs" in rows and "strih" in rows


def test_the_tool_no_longer_names_the_removed_input():
    assert "NDI obs hudba" not in _TOOL.read_text()


def test_an_explicit_stream_hop_with_its_input_absent_is_absent_not_fail(tmp_path):
    # The stream log carries no audit line for the default stream CG input.
    logs = _logs(tmp_path, stream_sources=("NDI 2ME PGM",))
    r = _run(tmp_path, args=("--hops", "strih stream"), logs=logs)
    assert r.returncode == 0, r.stdout + r.stderr
    stream_rows = [ln for ln in r.stdout.splitlines() if ln.startswith("stream")]
    assert stream_rows and stream_rows[0].split()[-1] == "ABSENT", r.stdout
    assert "OVERALL: PASS" in r.stdout


def test_the_stream_hop_via_env_and_an_overridden_input_is_verified(tmp_path):
    logs = _logs(tmp_path, stream_sources=("CG feed",))
    r = _run(tmp_path, env_extra={"CG_CHAIN_HOPS": "strih stream", "CG_CHAIN_STREAM_SRC": "CG feed"},
             logs=logs)
    assert r.returncode == 0, r.stdout + r.stderr
    stream_rows = [ln for ln in r.stdout.splitlines() if ln.startswith("stream")]
    assert stream_rows and "CG feed" in stream_rows[0] and stream_rows[0].split()[-1] == "PASS"


def test_a_missing_strih_cg_input_still_fails(tmp_path):
    logs = _logs(tmp_path)
    logs["STRIH"] = _log(tmp_path, "strih-nocg", ["NDI cam1"])
    r = _run(tmp_path, args=("--hops", "strih"), logs=logs)
    assert r.returncode == 3, r.stdout
    assert "OVERALL: FAIL" in r.stdout
