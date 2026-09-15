"""#1299 (YELLOW-1 review fix) — MECHANICAL C++-producer ↔ Python-consumer JSON-contract gate.

The decision path has a real C-vs-Rust parity gate; the JSON path did not — only a hand-mirrored
fixture asserted that `genlock_build_lock_json` (OBSBasicStatusBar.cpp) and
`genlock_lock_facet_from_log` (bundle_state_gather.py) agree on key NAMES/shape. A rename or field
reorder on the C++ side would pass every other gate yet silently break the live facet. This closes
that: it LIFTS the two pure builder functions VERBATIM from the vendored .cpp (the established
vendored-libobs / qpsk-marker lift-and-compile pattern), compiles them with `g++ -Wformat=2
-Werror`, runs them to emit REAL `genlock-lock-json:` lines, and feeds each through the actual
Python parser — asserting the reshaped facet matches the C++'s own decided values. Degradation is
fail-safe (a contract break → state=None → UNKNOWN, never a false page), but this makes the break
LOUD at CI instead of silent.

Lift-compile doctrine: FAIL LOUD when no compiler is present (a parity gate that silently skips is
worse than none). CI has g++; locally it runs too.
"""
import json
import pathlib
import shutil
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

_CPP = _ROOT / "vendor" / "obs-studio" / "frontend" / "widgets" / "OBSBasicStatusBar.cpp"

# The struct the builder walks — a minimal stand-in (the real one lives in an anon namespace with
# Qt types around it; these are the only fields the builder reads).
_STUB_HEADER = """#include <cstdio>
#include <cstdint>
#include <string>
#include <vector>
struct GenlockInputRow {
\tstd::string name;
\tbool locked = false;
\tbool connected = true;
\tuint32_t latency_ms = 0;
\tuint64_t underruns = 0;
\tuint64_t relocks = 0;
\tuint64_t late_holds = 0;
\tuint32_t depth = 0;
};
"""

# A main that emits three representative lines to stdout, one per line (no log prefix/tag — the
# test wraps each as a real genlock-lock-json: line before feeding the parser).
_MAIN = r'''
int main() {
    std::vector<GenlockInputRow> ins;
    GenlockInputRow a; a.name="NDI cam1"; a.locked=true; a.connected=true; a.latency_ms=3; a.underruns=0; a.relocks=1; a.late_holds=0; a.depth=2;
    GenlockInputRow b; b.name="weird \" name \\ x"; b.locked=false; b.connected=false; b.latency_ms=13; b.depth=4;
    ins.push_back(a); ins.push_back(b);
    // signature (#1299 v2): (state, reason, n_inputs, n_locked, n_absent, latency_ms, clock, output, recent_event, qpc_drift, inputs)
    printf("%s\n", genlock_build_lock_json("LOCKED","none",7,7,1,3,"locked","stamping",false,0,ins).c_str());
    printf("%s\n", genlock_build_lock_json("UNLOCKED","clock",7,0,0,0,"absent","not-stamping",true,-5,{}).c_str());
    printf("%s\n", genlock_build_lock_json("LOCKED","none",7,7,0,3,"locked","absent",false,0,{}).c_str());
    return 0;
}
'''


def _lift(sig: str, src: str) -> str:
    i = src.index(sig)
    j = src.index("\n}\n", i)
    return src[i:j + 3]


def _build_lift_tu() -> str:
    src = _CPP.read_text()
    return (
        _STUB_HEADER
        + _lift("void genlock_json_append_escaped(std::string &out, const char *s)", src) + "\n"
        + _lift("std::string genlock_build_lock_json(", src) + "\n"
        + _MAIN
    )


def test_cpp_builder_output_roundtrips_through_the_python_parser(tmp_path):
    gpp = shutil.which("g++") or shutil.which("c++")
    if gpp is None:
        pytest.fail("no g++/c++ compiler — cannot verify the C++↔Python JSON contract (never skip "
                    "a parity gate silently; install a compiler or run on CI)")

    tu = tmp_path / "gl_json_lift.cpp"
    tu.write_text(_build_lift_tu())
    binp = tmp_path / "gl_json_lift"
    cp = subprocess.run(
        [gpp, "-std=c++17", "-Wall", "-Wextra", "-Wformat=2", "-Werror", str(tu), "-o", str(binp)],
        capture_output=True, text=True,
    )
    assert cp.returncode == 0, f"lifted builder failed to compile under -Werror:\n{cp.stderr}"

    run = subprocess.run([str(binp)], capture_output=True, text=True)
    assert run.returncode == 0, run.stderr
    lines = [ln for ln in run.stdout.splitlines() if ln.strip()]
    assert len(lines) == 3, f"expected 3 emitted lines, got {lines}"

    # Each emitted object is valid JSON, and feeding it through the parser (wrapped as a real log
    # line) yields a facet whose NAMES/VALUES match the C++'s own decided fields.
    for raw in lines:
        obj = json.loads(raw)  # the C++ emitted valid JSON
        log_line = f"10:00:00.000: genlock-lock-json: {raw} (#1299)"
        facet = bsg.genlock_lock_facet_from_log(log_line)
        assert facet is not None, f"parser rejected a real builder line: {raw}"
        # top-level verdict fields survive verbatim
        assert facet["state"] == obj["state"]
        assert facet["reason"] == obj["reason"]
        assert facet["n_inputs"] == obj["n_inputs"]
        assert facet["n_locked"] == obj["n_locked"]
        assert facet["n_absent"] == obj["n_absent"]  # #1299 v2 additive field
        assert facet["latency_ms"] == obj["latency_ms"]
        assert facet["recent_event"] == obj["recent_event"]
        assert facet["qpc_drift_ms"] == obj["qpc_drift_ms"]
        assert facet["source"] == "log"
        # clock/output string tokens reshaped consistently
        assert facet["clock"] == {"state": obj["clock"]}
        assert facet["output"]["present"] == (obj["output"] != "absent")
        assert facet["output"]["stamping_wallclock"] == (obj["output"] == "stamping")
        # inputs array -> name-keyed map, 1:1 with the emitted records (incl. escaped names)
        assert set(facet["inputs"]) == {r["name"] for r in obj["inputs"]}
        for r in obj["inputs"]:
            got = facet["inputs"][r["name"]]
            assert got["locked"] == r["locked"]
            assert got["connected"] == r["connected"]  # #1299 v2 per-input field
            assert got["latency_ms"] == r["latency_ms"]
            assert got["underruns"] == r["underruns"]
            assert got["relocks"] == r["relocks"]
            assert got["late_holds"] == r["late_holds"]
            assert got["depth"] == r["depth"]
