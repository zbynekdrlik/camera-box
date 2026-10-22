"""issue 1414 -- box names carry the venue, never `lx` (owner 12.10.2026).

The SNV Linux strih is `strih-snv` (hostname, fact file, OBS profile + collection, the NDI prefix
`STRIH-SNV`), the SNV stream box `stream-snv` and the CG box `resolume-snv` in the fleet rows, the
same `<role>-<venue>` shape the Poprad boxes already carry (`strih-pp`, `stream-pp`, `resolume-pp`).

The transition aliases are declared in exactly two lookups and nowhere else:
  * the box-facts loader accepts `--box strih-lx` as strih-snv with ONE warning line;
  * `obs_fleet_name_for_host` (+ its python twin) keeps the old names resolving to the renamed rows
    (`strih-lx` / `STRIH-LX` / `strih-lx.lan` / `strih.lan`, `stream`, `resolume`).

The sweep at the bottom pins that no `strih-lx` / `STRIH-LX` literal is left anywhere else: a line
may keep the old name only when it cites this issue (`1414`, an alias site or a history note), or
when the file is history / vendored (the dated logs, vendor/).

Tier-0: bash functions run directly, files are read -- no cargo, no rig.
"""
import os
import pathlib
import re
import subprocess
import sys

import pytest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib"
BOXES = SCRIPTS / "strih-boxes"
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

# Only strih.lan resolves (to the strih address, like the rig DNS); nothing else touches DNS.
_RESOLVER_STUB = r'''
obs_fleet_resolve_host() {
  case "$1" in strih.lan) printf '10.77.9.202' ;; esac
  return 0
}
'''


def _bash(body: str, env: dict | None = None):
    full_env = {k: v for k, v in os.environ.items()
                if not k.startswith(("STRIH_", "OBS_FLEET", "DANTESYNC_"))}
    full_env.update(env or {})
    return subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=full_env,
                          timeout=60)


def _fleet(body: str):
    return _bash(f'. "{LIB}/obs-fleet.sh"\n{_RESOLVER_STUB}\n{body}')


def _facts(text: str) -> dict:
    out = {}
    for line in text.splitlines():
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        out[k] = v
    return out


# --- the fact file --------------------------------------------------------------------------------

def test_the_snv_strih_fact_file_is_strih_snv():
    assert not (BOXES / "strih-lx.env").exists(), "strih-lx.env must be renamed to strih-snv.env"
    f = _facts((BOXES / "strih-snv.env").read_text())
    assert f["STRIH_HOSTNAME"] == "strih-snv"
    assert f["STRIH_NDI_PREFIX"] == "STRIH-SNV"
    assert f["STRIH_OBS_PROFILE"] == "strih-snv"
    assert f["STRIH_OBS_COLLECTION"] == "strih-snv"
    assert f["STRIH_INTERCOM_CONFIG"] == "intercom/intercom.strih-snv.toml"
    assert f["STRIH_IP"] == "10.77.9.202"
    assert (REPO / "intercom" / "intercom.strih-snv.toml").is_file()
    assert not (REPO / "intercom" / "intercom.strih-lx.toml").exists()


def test_the_intercom_file_names_the_strih_snv_interkom_output():
    text = (REPO / "intercom" / "intercom.strih-snv.toml").read_text()
    assert 'ndi_source_name = "STRIH-SNV (interkom)"' in text


def test_the_default_box_is_strih_snv():
    r = _bash(f'. "{LIB}/strih-box-facts.sh"; printf "%s|" "$STRIH_BOX_DEFAULT"; '
              'strih_box_loaded_name; echo; strih_box_known | tr "\\n" " "')
    assert r.returncode == 0, r.stderr
    default, rest = r.stdout.split("|", 1)
    loaded, known = rest.split("\n", 1)
    assert default == "strih-snv"
    assert loaded == "strih-snv"
    assert "strih-snv" in known.split() and "strih-lx" not in known.split()


def test_box_strih_lx_is_an_alias_of_strih_snv_with_one_warning():
    r = _bash(f'. "{LIB}/strih-box-facts.sh"\n'
              'strih_box_load strih-lx || exit 9\n'
              'printf "%s %s %s" "$(strih_box_loaded_name)" "$(strih_box_fact STRIH_HOSTNAME)" '
              '"$(strih_box_fact STRIH_NDI_PREFIX)"')
    assert r.returncode == 0, r.stderr
    assert r.stdout == "strih-snv strih-snv STRIH-SNV"
    warn = [line for line in r.stderr.splitlines() if line.strip()]
    assert len(warn) == 1, r.stderr
    assert "strih-lx" in warn[0] and "strih-snv" in warn[0] and "1414" in warn[0]


def test_an_unknown_box_still_refuses():
    r = _bash(f'. "{LIB}/strih-box-facts.sh"\nstrih_box_load strih-zz; echo "rc=$?"')
    assert "rc=1" in r.stdout
    assert "no fact file" in r.stderr


# --- the obs-fleet table + its python twin --------------------------------------------------------

SNV_ROWS = {
    "strih-snv": ("10.77.9.202", "linux-genlock", "always"),
    "stream-snv": ("10.77.9.204", "linux-genlock", "always"),  # Linux since issue 1462
    "resolume-snv": ("resolume.lan", "windows-genlock", "traveling"),
}


def test_the_snv_fleet_rows_carry_the_venue():
    import obs_fleet_table
    rows = {r[0]: r[1:] for r in obs_fleet_table.fleet_rows()}
    for name, want in SNV_ROWS.items():
        assert rows.get(name) == want, (name, rows)
    for old in ("strih-lx", "stream", "resolume"):
        assert old not in rows, f"old row name {old} is still in OBS_FLEET"
    r = _fleet('for n in strih-snv stream-snv resolume-snv; do '
               'printf "%s|%s|%s|%s\\n" "$n" "$(obs_fleet_host $n)" "$(obs_fleet_class $n)" '
               '"$(obs_fleet_home_check $n)"; done')
    assert r.returncode == 0, r.stderr
    for line in r.stdout.splitlines():
        name, host, cls, check = line.split("|")
        assert SNV_ROWS[name] == (host, cls, check)


FACETS = ["audio-lag", "av-step", "vb-matrix", "bundle-state", "network-reach", "obs-liveness",
          "genlock-lock", "render-freeze", "ndi-portmap", "obs-session", "burn-reconcile",
          "rig-restore", "ndi-sender", "win-baseline", "audio-mixer", "obs-handles"]


@pytest.mark.parametrize("facet", FACETS)
def test_every_facet_names_the_renamed_boxes(facet):
    r = _fleet(f'obs_fleet_facet_members {facet}')
    assert r.returncode == 0, r.stderr
    members = r.stdout.split()
    assert members, facet
    for m in members:
        # issue 1441: the PP Linux boxes joined four facets; every member still carries its venue
        # (<role>-<venue>), never a pre-rename name.
        assert m in ("strih-snv", "stream-snv", "resolume-snv", "imag", "strih-pp", "stream-pp"), (facet, members)


def test_the_resolume_ahk_fact_follows_the_rename():
    r = _fleet('printf "%s %s" "$(obs_fleet_has_ahk resolume-snv)" "$(obs_fleet_has_ahk strih-snv)"')
    assert r.stdout == "1 0"


ALIASES = [
    ("strih-snv", "strih-snv"),
    ("STRIH-SNV", "strih-snv"),
    ("strih-snv.lan", "strih-snv"),
    ("10.77.9.202", "strih-snv"),
    ("strih-lx", "strih-snv"),
    ("STRIH-LX", "strih-snv"),
    ("strih-lx.lan", "strih-snv"),
    ("strih.lan", "strih-snv"),
    ("stream", "stream-snv"),
    ("stream-snv", "stream-snv"),
    ("stream.lan", "stream-snv"),
    ("10.77.9.204", "stream-snv"),
    ("resolume", "resolume-snv"),
    ("resolume.lan", "resolume-snv"),
    ("resolume-snv", "resolume-snv"),
    ("strih-pp", "strih-pp"),
    ("stream-pp.lan", "stream-pp"),
]


@pytest.mark.parametrize("host,want", ALIASES)
def test_name_for_host_keeps_the_old_names_resolving(host, want):
    r = _fleet(f'obs_fleet_name_for_host "{host}"; echo " rc=$?"')
    assert r.stdout.rstrip("\n") == f"{want} rc=0", (host, r.stdout, r.stderr)


@pytest.mark.parametrize("host,want", ALIASES)
def test_the_python_twin_resolves_the_same_names(host, want):
    import obs_fleet_table

    def resolve(h):
        return "10.77.9.202" if h == "strih.lan" else ""

    assert obs_fleet_table.fleet_name_for_host(host, resolve=resolve) == want


def test_the_retired_windows_strih_name_still_resolves_to_nothing():
    r = _fleet('obs_fleet_name_for_host strih; echo " rc=$?"')
    assert r.stdout.rstrip("\n") == " rc=1"
    import obs_fleet_table
    assert obs_fleet_table.fleet_name_for_host("strih", resolve=lambda h: "") is None


def test_the_alias_table_is_declared_once_in_each_twin():
    sh = (LIB / "obs-fleet.sh").read_text()
    py = (SCRIPTS / "obs_fleet_table.py").read_text()
    assert sh.count("OBS_FLEET_ALIASES=") == 1
    import obs_fleet_table
    r = _bash(f'. "{LIB}/obs-fleet.sh"; printf "%s" "$OBS_FLEET_ALIASES"')
    bash_aliases = dict(pair.split("=") for pair in r.stdout.split())
    assert bash_aliases == obs_fleet_table.FLEET_ALIASES
    assert bash_aliases == {"strih-lx": "strih-snv", "stream": "stream-snv",
                            "resolume": "resolume-snv"}
    assert "FLEET_ALIASES" in py


# --- the dantesync fleet --------------------------------------------------------------------------

def test_the_dantesync_fleet_names_follow_the_obs_fleet():
    r = _bash(f'. "{LIB}/dantesync-fleet.sh"; printf "%s" "$DANTESYNC_FLEET"')
    assert r.returncode == 0, r.stderr
    names = [line.split("|")[0] for line in r.stdout.splitlines() if line.strip()]
    for name in ("strih-snv", "stream-snv", "resolume-snv"):
        assert name in names, names
    for old in ("strih-lx", "stream", "resolume"):
        assert old not in names, names
    rows = {line.split("|")[0]: line for line in r.stdout.splitlines() if line.strip()}
    assert rows["strih-snv"].startswith("strih-snv|obs:strih-snv|linux|ntp-master|")
    assert rows["stream-snv"].startswith("stream-snv|obs:stream-snv|windows|video|")
    assert rows["resolume-snv"].startswith("resolume-snv|obs:resolume-snv|windows|video|obsfleet|")


def test_the_dantesync_python_twin_rows_carry_the_new_names():
    import dantesync_fleet
    names = [row.split("|")[0] for row in dantesync_fleet.rows()]
    for name in ("strih-snv", "stream-snv", "resolume-snv"):
        assert name in names, names
    for old in ("strih-lx", "stream", "resolume"):
        assert old not in names, names


# --- the consumers that named the box -------------------------------------------------------------

def test_the_ndi_portmap_baseline_names_strih_snv():
    text = (SCRIPTS / "ndi-portmap-baseline.json").read_text()
    assert "STRIH-LX" not in text and "strih-lx" not in text
    assert "STRIH-SNV (2ME PGM)" in text


def test_the_preview_source_default_is_strih_snv():
    src = (REPO / "src" / "preview_source.rs").read_text()
    assert 'pub const DEFAULT_PREVIEW_SOURCE: &str = "STRIH-SNV (interkom)";' in src


def test_the_hub_default_receiver_name_is_strih_snv():
    src = (REPO / "intercom" / "hub" / "src" / "matrix.rs").read_text()
    assert '"STRIH-SNV (interkom)".to_string()' in src


# --- the sweep ------------------------------------------------------------------------------------

OLD = re.compile(r"strih-lx|STRIH-LX")
# The two RETIRED on-box files setup-strih still REMOVES keep the name they have on the box (a rename
# here would stop the cleanup finding them): the issue-1346 projector seed and the old logind drop-in.
RETIRED_ONBOX_NAMES = ("strih-lx-projector.json", "90-strih-lx.conf")
# History or vendored text that keeps the old name as it was written: the dated logs + plans, the
# vendored OBS/DistroAV sources (measurement-history comments), and this test.
HISTORY_PATHS = (
    "docs/autopilot-log.md",
    "docs/superpowers/",
    "vendor/",
    "tests/python/test_box_names_venue_1414.py",
)


def _tracked_files():
    out = subprocess.run(["git", "ls-files", "-z"], cwd=REPO, capture_output=True, check=True)
    return [p for p in out.stdout.decode().split("\0") if p]


def test_no_tracked_path_carries_strih_lx():
    bad = [p for p in _tracked_files() if OLD.search(p) and not p.startswith(HISTORY_PATHS)]
    assert bad == [], f"rename these files to strih-snv: {bad}"


def test_no_strih_lx_literal_outside_the_alias_and_history_sites():
    bad = []
    for path in _tracked_files():
        if path.startswith(HISTORY_PATHS):
            continue
        full = REPO / path
        try:
            data = full.read_bytes()
        except (FileNotFoundError, IsADirectoryError):
            continue
        if b"\0" in data[:8192]:
            continue
        text = data.decode("utf-8", errors="replace")
        for n, line in enumerate(text.splitlines(), 1):
            for retired in RETIRED_ONBOX_NAMES:
                line = line.replace(retired, "")
            if OLD.search(line) and "1414" not in line:
                bad.append(f"{path}:{n}: {line.strip()[:140]}")
    assert bad == [], f"{len(bad)} strih-lx literal(s) left:\n" + "\n".join(bad[:80])


# --- the live rename of the SNV strih's own OBS collection: obs_collection_port.py --preset strih-snv

def _port_mod():
    import importlib.util
    spec = importlib.util.spec_from_file_location("obs_collection_port_1414", SCRIPTS / "obs_collection_port.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _pre_rename_collection():
    """The pre-1414 strih collection shape: the self-loop inputs read STRIH-LX (...)."""
    def src(name, sid, settings=None, filters=None):
        return {"id": sid, "versioned_id": sid, "name": name, "uuid": "u-" + name,
                "settings": settings or {}, "filters": filters or [], "volume": 1.0, "sync": 0,
                "mixers": 255}

    def scene(name, items):
        return src(name, "scene", {"items": [{"name": n, "source_uuid": "u-" + n, "visible": True,
                                              "id": i + 1} for i, n in enumerate(items)],
                                   "id_counter": len(items)})

    srcs = [
        scene("Cam 2", ["NDI cam2"]),
        scene("2ME PVW", ["NDI 2ME PVW"]),
        scene("MV", ["NDI 2ME PGM (mv)"]),
        scene("CG", ["cg"]),
        scene("Interkom", ["interkom web", "ASIO zvuk"]),
        src("NDI cam2", "ndi_source", {"ndi_source_name": "CAM2 (usb)", "genlock_fifo": True}),
        src("NDI 2ME PVW", "ndi_source", {"ndi_source_name": "STRIH-LX (2ME PVW)", "ndi_sync": 1}),
        src("NDI 2ME PGM (mv)", "ndi_source", {"ndi_source_name": "STRIH-LX (2ME PGM)", "ndi_sync": 1}),
        src("cg", "ndi_source", {"ndi_source_name": "RESOLUME-SNV (SP-program)"}),
        src("ASIO zvuk", "pulse_input_capture", {"device_id": "strih-program-source"}),
        src("interkom web", "browser_source", {"url": "https://vdo.ninja/?room=snv"},
            filters=[{"id": "shader_filter", "versioned_id": "shader_filter", "name": "glow",
                      "settings": {}},
                     {"id": "ndi_filter", "versioned_id": "ndi_filter", "name": "NDI out",
                      "settings": {"ndi_filter_ndiname": "interkom"}}]),
    ]
    return {"name": "strih-lx", "current_scene": "Cam 2", "current_program_scene": "2ME PVW",  # issue 1414
            "scene_order": [{"name": s["name"]} for s in srcs if s["id"] == "scene"],
            "sources": srcs, "groups": [], "modules": {}, "transitions": [],
            "current_transition": "Fade"}


def test_the_strih_snv_preset_renames_the_self_loop_senders_only():
    m = _port_mod()
    coll = _pre_rename_collection()
    out, rep = m.port(coll, m.preset_options("strih-snv"))
    by = {s["name"]: s for s in out["sources"]}
    assert by["NDI 2ME PVW"]["settings"]["ndi_source_name"] == "STRIH-SNV (2ME PVW)"
    assert by["NDI 2ME PGM (mv)"]["settings"]["ndi_source_name"] == "STRIH-SNV (2ME PGM)"
    assert by["cg"]["settings"]["ndi_source_name"] == "RESOLUME-SNV (SP-program)"
    assert by["NDI cam2"]["settings"] == coll["sources"][5]["settings"]
    assert out["name"] == "strih-snv"
    # every scene and every input kept, the operator's filters kept (a rename drops nothing)
    assert [e["name"] for e in out["scene_order"]] == [e["name"] for e in coll["scene_order"]]
    assert sorted(by) == sorted(s["name"] for s in coll["sources"])
    assert [f["name"] for f in by["interkom web"]["filters"]] == ["glow", "NDI out"]
    assert not [ln for ln in rep if ln.startswith(("DROP", "WARN"))], rep
    assert sum(1 for ln in rep if ln.startswith("SENDER")) == 2


def test_the_strih_snv_preset_follows_the_fact_file():
    m = _port_mod()
    f = _facts((BOXES / "strih-snv.env").read_text())
    o = m.preset_options("strih-snv")
    assert list(o["sender_prefixes"].values()) == [f["STRIH_NDI_PREFIX"]]
    assert o["name"] == f["STRIH_OBS_COLLECTION"]
    assert o["program_audio_input"] is None


# --- review round 1: one alias table, every lookup, every CLI ---------------------------------------

@pytest.mark.parametrize("old,new,host", [
    ("strih-lx", "strih-snv", "10.77.9.202"),
    ("stream", "stream-snv", "10.77.9.204"),
    ("resolume", "resolume-snv", "resolume.lan"),
])
def test_every_fleet_fact_lookup_takes_an_old_name(old, new, host):
    """The alias resolves inside the ONE row lookup, so every fact reader and every `--box` CLI
    (dantesync-maintenance-gate, win-baseline-check, the role tokens `--box stream`) addresses the
    renamed row -- never a per-CLI canonicalization that one tool forgets."""
    r = _fleet(f'obs_fleet_host {old}; echo " rc=$?"; obs_fleet_class {old}; echo; '
               f'obs_fleet_canonical_name {old}')
    lines = r.stdout.split("\n")
    assert lines[0] == f"{host} rc=0", r.stdout + r.stderr
    assert lines[1] == _fleet(f"obs_fleet_class {new}").stdout
    assert lines[2] == new
    import obs_fleet_table
    assert obs_fleet_table.fleet_host(old) == host
    assert obs_fleet_table.fleet_class(old) == obs_fleet_table.fleet_class(new)


def test_an_exact_row_beats_an_alias():
    """A table override that still carries an old-named row (a test fixture, a box config) keeps
    addressing THAT row: the alias is only a fallback when no row has the name."""
    r = _bash(f'. "{LIB}/obs-fleet.sh"; obs_fleet_host stream; echo; obs_fleet_canonical_name stream',
              env={"OBS_FLEET": "stream|1.2.3.4|windows-genlock|always"})
    assert r.stdout.split("\n")[:2] == ["1.2.3.4", "stream"], r.stdout + r.stderr


def test_the_canonical_name_case_folds_and_needs_the_target_row():
    r = _fleet('for n in STRIH-SNV STREAM Resolume-SNV nosuchbox; do '
               'printf "%s|" "$(obs_fleet_canonical_name "$n")"; done')
    assert r.stdout == "strih-snv|stream-snv|resolume-snv|nosuchbox|"
    # an alias whose renamed row is not in the table is no alias
    r = _bash(f'. "{LIB}/obs-fleet.sh"; obs_fleet_canonical_name stream; echo " rc=$?"',
              env={"OBS_FLEET": "imag|imag|linux-genlock|always"})
    assert r.stdout.rstrip("\n") == "stream rc=0"


def test_the_box_facts_loader_takes_its_alias_from_the_fleet_table():
    """One declared alias table: the box-facts loader reuses the fleet one, so `--box strih-lx`
    and `obs_fleet_canonical_name strih-lx` can never disagree."""
    facts = (LIB / "strih-box-facts.sh").read_text()
    assert "STRIH_BOX_ALIASES" not in facts
    assert "_obs_fleet_alias_of" in facts


def test_the_deploy_planner_and_retention_drivers_take_the_old_names():
    r = _bash(f'. "{SCRIPTS}/deploy-genlock-fleet.sh"; fleet_normalize_boxes strih-lx,stream,resolume,imag')
    assert r.stdout.strip() == "strih-snv,stream-snv,imag,resolume-snv", r.stdout + r.stderr


def test_setup_and_verify_strih_carry_the_loaded_box_name_after_an_alias():
    """After `--box strih-lx` the scripts work on strih-snv: the banner names its real fact file,
    the nested verify run gets the new name (no second alias warning)."""
    for script in ("setup-strih.sh", "verify-strih.sh"):
        text = (SCRIPTS / script).read_text()
        assert 'STRIH_FACT_BOX="$(strih_box_loaded_name)"' in text, script


def test_setup_strih_removes_the_pre_rename_role_files():
    text = (SCRIPTS / "setup-strih.sh").read_text()
    assert "rm -f /opt/camera-box/strih-lx-seed.json" in text
    assert 'rm -f "$GENLOCK_DIR/strih-lx-profile-facts.txt"' in text


@pytest.mark.parametrize("box,role", [("stream-snv", "stream"), ("stream", "stream"),
                                      ("resolume-snv", "resolume")])
def test_the_launch_plan_hands_role_tokens_to_the_role_tools(box, role):
    """latency_pins_verify.py and mark-imag-restart.sh take a ROLE (strih|stream|imag|resolume),
    never a fleet name: the plan for `--box stream-snv` must print `--box stream`."""
    r = subprocess.run(["bash", str(SCRIPTS / "launch-obs-genlock.sh"), "--box", box],
                       capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    assert f"latency_pins_verify.py --box {role} " in r.stdout
    assert f"mark-imag-restart.sh --box {role} " in r.stdout
    assert "--box stream-snv " not in r.stdout and "--box resolume-snv " not in r.stdout


def test_the_strih_pp_preset_still_renames_a_pre_rename_self_loop():
    """A strih collection exported before the live rename still carries STRIH-LX self-loops; the
    Poprad port renames them too (issue 1414)."""
    m = _port_mod()
    out, rep = m.port(_pre_rename_collection(), m.preset_options("strih-pp")
                      | {"url_hosts": {"presenter.lan": "presenter.lan"}})
    by = {s["name"]: s for s in out["sources"]}
    assert by["NDI 2ME PVW"]["settings"]["ndi_source_name"] == "STRIH-PP (2ME PVW)"
    assert by["NDI 2ME PGM (mv)"]["settings"]["ndi_source_name"] == "STRIH-PP (2ME PGM)"
    assert not [ln for ln in rep if ln.startswith("WARN sender") and "STRIH-" in ln], rep



# --- review round 2: the last name-keyed facts take an old name too ---------------------------------

def test_the_ahk_fact_and_the_home_force_list_take_an_old_name():
    r = _fleet('obs_fleet_has_ahk resolume; obs_fleet_has_ahk RESOLUME-SNV; obs_fleet_has_ahk stream')
    assert r.stdout == "110", r.stdout + r.stderr
    for listed, asked in [("resolume-snv", "resolume"), ("resolume", "resolume-snv")]:
        q = _bash(f'. "{LIB}/obs-fleet.sh"; obs_fleet_is_home {asked}; echo "rc=$?"',
                  env={"OBS_FLEET_HOME": listed})
        assert q.stdout.strip() == "rc=0", (listed, asked, q.stdout + q.stderr)
    q = _bash(f'. "{LIB}/obs-fleet.sh"; obs_fleet_is_home stream-snv; echo "rc=$?"',
              env={"OBS_FLEET_HOME": "resolume"})
    assert q.stdout.strip() == "rc=1"


def test_setup_strih_removes_the_old_seed_only_after_the_new_seeder_is_installed():
    """A run that dies between the two never leaves the box's old seeder without its manifest."""
    text = (SCRIPTS / "setup-strih.sh").read_text()
    assert (text.index('install -m 0755 "${HERE}/strih_scenes.py"')
            < text.index("rm -f /opt/camera-box/strih-lx-seed.json"))
