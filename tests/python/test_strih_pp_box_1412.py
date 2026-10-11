"""issue 1412, slice A: the Poprad strih (strih-pp) as a strih-lx clone, plus the fresh-box gaps.

Covers:
  * the facts: the new keys STRIH_ROLE / STRIH_DANTE_GM / STRIH_PROGRAM_AUDIO_SAMPLER, strih-pp's values
    from the 11.10.2026 recon + owner rulings (only STRIH_NDI_RUNTIME_PEER stays TODO_OWNER);
  * the obs-fleet rows strih-pp / stream-pp (name rows, traveling, in no facet) + the python twin;
  * G5 scripts/lib/dantesync-install.sh: the pinned binary + the role config, both only when absent;
  * G6 scripts/lib/strih-ci-artifact.sh: the intercom-hub binary from a CI artifact, only when absent;
  * G7 + G11 scripts/lib/obs-box-role.sh: step 11b (xhci IRQ unit) and 16e (the sampler) by box fact;
  * intercom/intercom.strih-pp.toml against the hub's Matrix::from_toml rules (a python replica; the
    real loader runs in intercom/hub/tests/deployed_matrix_pp_1412.rs at CI);
  * the setup-strih / verify-strih wiring.

Tier-0: bash + pytest only (no cargo, no root, no rig, no network: downloads go through a fake curl).
"""
import json
import os
import subprocess
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
FLEET = SCRIPTS / "lib" / "obs-fleet.sh"
DS_INSTALL = SCRIPTS / "lib" / "dantesync-install.sh"
CI_ART = SCRIPTS / "lib" / "strih-ci-artifact.sh"
ROLE = SCRIPTS / "lib" / "obs-box-role.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
BOXES = SCRIPTS / "strih-boxes"
PP_TOML = REPO / "intercom" / "intercom.strih-pp.toml"

LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_LX_NTP_SERVER", "STRIH_NIC_IFACE",
         "STRIH_BOXES_DIR", "OBS_FLEET", "RIG_GRANDMASTER_IP", "GH_TOKEN", "STRIH_LX_HUB_SRC",
         "DANTESYNC_VERSION_PIN", "DANTESYNC_RELEASE_BASE", "DANTESYNC_NTP_UPSTREAM",
         "DANTESYNC_INSTALL_BIN", "DANTESYNC_INSTALL_CONFIG", "OBS_FLEET_HOME")

NEW_KEYS = ("STRIH_ROLE", "STRIH_DANTE_GM", "STRIH_PROGRAM_AUDIO_SAMPLER")
PEER = "10.77.9.61"


def _bash(body, env=None, sources=(FACTS,), path_prefix=None):
    harness = "set -uo pipefail\n" + "".join('. "%s"\n' % s for s in sources) + body
    full_env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prefix:
        full_env["PATH"] = "%s:%s" % (path_prefix, full_env["PATH"])
    if env:
        full_env.update(env)
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True, env=full_env,
                          cwd=str(REPO), timeout=60)


def _pp_filled(tmp_path, peer=PEER, extra=()):
    """strih-pp.env with the NDI runtime peer set to PEER (answered 11.10.2026 as SNV cam5)."""
    body = (BOXES / "strih-pp.env").read_text()
    assert body.count("STRIH_NDI_RUNTIME_PEER=10.77.9.65\n") == 1
    body = body.replace("STRIH_NDI_RUNTIME_PEER=10.77.9.65\n", "STRIH_NDI_RUNTIME_PEER=%s\n" % peer)
    for old, new in extra:
        assert body.count(old) == 1, old
        body = body.replace(old, new)
    (tmp_path / "strih-pp.env").write_text(body)
    return {"STRIH_BOXES_DIR": str(tmp_path)}


# --- the facts --------------------------------------------------------------------------------------

def test_the_new_keys_close_the_fact_list():
    r = _bash("strih_box_fact_keys")
    assert r.returncode == 0, r.stderr
    keys = r.stdout.split()
    i = keys.index("STRIH_HDMI_OUTPUT_BACKEND")
    assert tuple(keys[i + 1:i + 4]) == NEW_KEYS


def test_strih_lx_is_the_strih_role_on_the_snv_grandmaster_with_the_sampler():
    r = _bash('strih_box_load strih-lx || exit 9\nprintf "%s|%s|%s\\n" "$(strih_box_role)" '
              '"$(strih_lx_dante_gm)" "$(strih_lx_program_audio_sampler)"')
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "strih|video-clock.lan|on"


def test_strih_pp_loads_with_every_fact_answered():
    """11.10.2026: the last open fact (the NDI runtime peer) is answered -- SNV cam5, the fleet runtime."""
    r = _bash("strih_box_load strih-pp || exit 9\nstrih_lx_ndi_runtime_peer")
    assert r.returncode == 0, r.stderr
    assert "TODO_OWNER" not in r.stderr
    assert r.stdout.strip() == "10.77.9.65"


EXPECTED_PP = {
    "STRIH_HOSTNAME": "strih-pp", "STRIH_IP": "10.77.8.202", "STRIH_NDI_PREFIX": "STRIH-PP",
    "STRIH_DANTESYNC_ROLE": "server", "STRIH_DANTESYNC_UPSTREAM": "",
    "STRIH_INTERCOM_CONFIG": "intercom/intercom.strih-pp.toml", "STRIH_NIC_DRIVER": "r8169",
    "STRIH_NIC_OOT_DRIVER": "none", "STRIH_NIC_MIN_USB_MBPS": "none", "STRIH_NIC_MIN_LINK_MBPS": "2500",
    "STRIH_OBS_PROFILE": "strih-pp", "STRIH_OBS_COLLECTION": "strih-pp", "STRIH_NDI_RUNTIME_PEER": PEER,
    "STRIH_COMPANION_HOST": "10.77.8.205", "STRIH_CG_SENDER": "RESOLUME-PP (SP-program)",
    "STRIH_CAMERAS": "1 4 6 7", "STRIH_HDMI_OUTPUT_BACKEND": "vk-direct", "STRIH_ROLE": "strih",
    "STRIH_DANTE_GM": "10.77.8.110", "STRIH_PROGRAM_AUDIO_SAMPLER": "off",
    "STRIH_PROGRAM_SENDER": "none", "STRIH_VENUE": "pp",
}


def test_strih_pp_with_the_peer_answered_loads_the_recon_facts(tmp_path):
    r = _bash("strih_box_load strih-pp || exit 9\n"
              'for k in $(strih_box_fact_keys); do printf "%s=%s\\n" "$k" "$(strih_box_fact "$k")"; done\n'
              'printf "HOST=%s\\n" "$(strih_lx_host)"', env=_pp_filled(tmp_path))
    assert r.returncode == 0, r.stderr
    got = dict(ln.split("=", 1) for ln in r.stdout.splitlines())
    for k, v in EXPECTED_PP.items():
        assert got[k] == v, (k, got[k])
    # the fleet row is a NAME: dev1 reaches the PP LAN as 10.76.8.x, the box carries 10.77.8.x
    assert got["HOST"] == "strih-pp.lan"


def test_strih_pp_seed_manifest_takes_cameras_1_4_6_7_and_the_pp_cg(tmp_path):
    r = _bash("strih_box_load strih-pp || exit 9\nstrih_lx_seed_manifest_json", env=_pp_filled(tmp_path),
              sources=(FACTS, PROVISION))
    assert r.returncode == 0, r.stderr
    m = json.loads(r.stdout)
    pairs = [(i["sender"], i["input"]) for i in m["inputs"]]
    assert pairs[:4] == [("CAM1 (usb)", "NDI cam1"), ("CAM4 (usb)", "NDI cam4"),
                         ("CAM6 (usb)", "NDI cam6"), ("CAM7 (usb)", "NDI cam7")]
    assert ("STRIH-PP (2ME PGM)", "NDI 2ME PGM (mv)") in pairs
    assert ("RESOLUME-PP (SP-program)", "cg") in pairs
    assert m["outputs"][0] == "STRIH-PP (2ME PGM)"
    assert "RESOLUME-SNV" not in r.stdout and "STRIH-LX" not in r.stdout


@pytest.mark.parametrize("key,value", [
    ("STRIH_ROLE", "cutter"), ("STRIH_ROLE", ""), ("STRIH_DANTE_GM", "-x"), ("STRIH_DANTE_GM", "a b"),
    ("STRIH_PROGRAM_AUDIO_SAMPLER", "yes"), ("STRIH_PROGRAM_AUDIO_SAMPLER", "ON"),
])
def test_a_malformed_new_fact_refuses_by_name(tmp_path, key, value):
    line = next(ln for ln in (BOXES / "strih-lx.env").read_text().splitlines() if ln.startswith(key + "="))
    body = (BOXES / "strih-lx.env").read_text().replace(line, "%s=%s" % (key, value))
    (tmp_path / "strih-lx.env").write_text(body)
    r = _bash("strih_box_load strih-lx", env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.returncode != 0
    assert key in r.stderr, r.stderr


def test_rig_grandmaster_ip_overrides_the_gm_fact():
    r = _bash("strih_box_load strih-lx || exit 9\nstrih_lx_dante_gm", env={"RIG_GRANDMASTER_IP": "10.9.9.9"})
    assert r.stdout == "10.9.9.9"


def test_setup_and_verify_refuse_a_box_with_an_open_fact(tmp_path):
    body = (BOXES / "strih-pp.env").read_text().replace(
        "STRIH_NDI_RUNTIME_PEER=10.77.9.65\n", "STRIH_NDI_RUNTIME_PEER=TODO_OWNER\n")
    (tmp_path / "strih-pp.env").write_text(body)
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env["STRIH_BOXES_DIR"] = str(tmp_path)
    for script in (SETUP, VERIFY):
        r = subprocess.run(["bash", "-c", '. "%s" --box strih-pp\necho SOURCED' % script],
                           capture_output=True, text=True, cwd=str(REPO), timeout=60, env=env)
        assert r.returncode != 0 and "SOURCED" not in r.stdout, script
        assert "STRIH_NDI_RUNTIME_PEER is TODO_OWNER" in r.stderr


# --- the obs-fleet rows -----------------------------------------------------------------------------

@pytest.mark.parametrize("name", ["strih-pp", "stream-pp"])
def test_the_pp_boxes_are_traveling_linux_name_rows(name):
    r = _bash('printf "%%s|%%s|%%s\\n" "$(obs_fleet_host %s)" "$(obs_fleet_class %s)" "$(obs_fleet_home_check %s)"'
              % (name, name, name), sources=(FLEET,))
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "%s.lan|linux-genlock|traveling" % name


def test_the_pp_boxes_are_in_no_facet():
    facets = ("audio-lag av-step vb-matrix bundle-state network-reach obs-liveness genlock-lock render-freeze "
              "ndi-portmap obs-session burn-reconcile rig-restore ndi-sender win-baseline audio-mixer obs-handles")
    r = _bash('for f in %s; do printf "%%s=%%s\\n" "$f" "$(obs_fleet_facet_members "$f")"; done' % facets,
              sources=(FLEET,))
    assert r.returncode == 0, r.stderr
    assert r.stdout.count("=") == len(facets.split())
    assert "-pp" not in r.stdout, r.stdout


def test_a_pp_address_is_a_linux_box_for_the_class_gate():
    r = _bash("obs_fleet_refuse_linux_target strih-pp.lan some-windows-tool; echo rc=$?", sources=(FLEET,))
    assert "rc=1" in r.stdout


def test_the_python_twin_reads_the_pp_rows():
    r = subprocess.run(["python3", "-c", "import sys; sys.path.insert(0, %r); import obs_fleet_table as t; "
                        "print(t.fleet_name_for_host('stream-pp.lan'), t.fleet_class_for_host('strih-pp'))"
                        % str(SCRIPTS)], capture_output=True, text=True,
                       env={k: v for k, v in os.environ.items() if k not in LEAKY})
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["stream-pp", "linux-genlock"]


# --- G5: dantesync install --------------------------------------------------------------------------

def _gate_pin():
    for ln in (SCRIPTS / "dantesync-version-gate.sh").read_text().splitlines():
        if ln.startswith('DANTESYNC_VERSION_PIN="${DANTESYNC_VERSION_PIN:-'):
            return ln.split(":-", 1)[1].rstrip('}"')
    raise AssertionError("no pin line")


def test_the_install_pin_is_the_version_gates_pin():
    r = _bash("dantesync_install_pinned_version; echo; dantesync_install_release_url 1.2.3", sources=(DS_INSTALL,))
    assert r.returncode == 0, r.stderr
    pin, url = r.stdout.splitlines()
    assert pin == _gate_pin()
    assert url == "https://github.com/zbynekdrlik/dantesync/releases/download/v1.2.3/dantesync-linux-amd64"


def _config(role, gm):
    r = _bash('dantesync_install_config_json %s %s' % (role, gm), sources=(DS_INSTALL,))
    assert r.returncode == 0, r.stderr
    return json.loads(r.stdout)


def test_the_server_config_is_the_ntp_master_role():
    c = _config("server", "10.77.8.110")
    assert c["ntp_server_mode"] == {"enabled": True, "max_step_us": 100000, "port": 123, "stratum": 3}
    assert c["ntp_server"] == "162.159.200.1"
    assert c["http_status"] == {"enabled": True, "port": 8898}
    assert c["system"] == {"gm_allowlist": ["10.77.8.110"], "clock_discipline": "ptp_phase_lock"}


def test_the_client_config_is_the_video_role():
    c = _config("client", "video-clock.lan")
    assert c["ntp_server_mode"]["enabled"] is False
    assert "ntp_server" not in c
    assert c["system"]["gm_allowlist"] == ["video-clock.lan"]


@pytest.mark.parametrize("args", ["master 10.0.0.1", "server ''", "client 'a;b'", "server -x"])
def test_a_bad_role_or_grandmaster_renders_nothing(args):
    r = _bash("dantesync_install_config_json %s" % args, sources=(DS_INSTALL,))
    assert r.returncode != 0 and r.stdout == ""


def _fake_curl(tmp_path, version, sha_ok=True):
    """A curl that 'downloads' a dantesync reporting VERSION and its release .sha256, logging every call."""
    import hashlib
    bindir = tmp_path / "fakebin"
    bindir.mkdir()
    log = tmp_path / "curl.log"
    body = '#!/bin/sh\necho "dantesync %s"\n' % version
    sha = hashlib.sha256(body.encode()).hexdigest() if sha_ok else "0" * 64
    (tmp_path / "payload").write_text(body)
    (bindir / "curl").write_text(
        "#!/bin/bash\nprintf '%%s\\n' \"$*\" >> %s\nout=\"\"; url=\"\"\n"
        "while [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; -*) shift;; *) url=\"$1\"; shift;; esac; done\n"
        "case \"$url\" in *.sha256) printf '%s  dantesync-linux-amd64\\n' > \"$out\";; "
        "*) cp %s \"$out\";; esac\n" % (log, sha, tmp_path / "payload"))
    (bindir / "curl").chmod(0o755)
    return bindir, log


def test_a_download_whose_sha256_does_not_match_is_never_run_or_installed(tmp_path):
    bindir, _log = _fake_curl(tmp_path, _gate_pin(), sha_ok=False)
    b = tmp_path / "bin" / "dantesync"
    r = _bash("dantesync_install_binary", sources=(DS_INSTALL,), path_prefix=bindir,
              env={"DANTESYNC_INSTALL_BIN": str(b)})
    assert r.returncode != 0
    assert "SHA256 MISMATCH" in r.stderr
    assert not b.exists()


def test_the_install_path_defaults_to_the_restart_decisions_path(tmp_path):
    b = tmp_path / "ds"
    b.write_text("#!/bin/sh\necho dantesync 9.9.9\n")
    b.chmod(0o755)
    r = _bash("dantesync_install_binary", sources=(DS_INSTALL,), env={"STRIH_DANTESYNC_BIN": str(b)})
    assert r.returncode == 0 and "kept" in r.stdout and str(b) in r.stdout


def test_a_fresh_box_gets_the_pinned_binary_and_the_role_config(tmp_path):
    pin = _gate_pin()
    bindir, log = _fake_curl(tmp_path, pin)
    env = {"DANTESYNC_INSTALL_BIN": str(tmp_path / "bin" / "dantesync"),
           "DANTESYNC_INSTALL_CONFIG": str(tmp_path / "etc" / "config.json")}
    r = _bash("dantesync_install server 10.77.8.110", env=env, sources=(DS_INSTALL,), path_prefix=bindir)
    assert r.returncode == 0, r.stderr
    assert "/v%s/dantesync-linux-amd64" % pin in log.read_text()
    assert os.access(env["DANTESYNC_INSTALL_BIN"], os.X_OK)
    cfg = json.loads(Path(env["DANTESYNC_INSTALL_CONFIG"]).read_text())
    assert cfg["system"]["gm_allowlist"] == ["10.77.8.110"] and cfg["ntp_server_mode"]["enabled"] is True
    assert oct(Path(env["DANTESYNC_INSTALL_CONFIG"]).stat().st_mode & 0o777) == "0o644"


def test_an_existing_binary_and_config_are_never_touched(tmp_path):
    bindir, log = _fake_curl(tmp_path, _gate_pin())
    b = tmp_path / "dantesync"
    b.write_text("#!/bin/sh\necho dantesync 0.0.1\n")
    b.chmod(0o755)
    c = tmp_path / "config.json"
    c.write_text('{"mine": true}\n')
    r = _bash("dantesync_install server 10.77.8.110", sources=(DS_INSTALL,), path_prefix=bindir,
              env={"DANTESYNC_INSTALL_BIN": str(b), "DANTESYNC_INSTALL_CONFIG": str(c)})
    assert r.returncode == 0, r.stderr
    assert not log.exists(), "no download for a box that has a binary"
    assert b.read_text() == "#!/bin/sh\necho dantesync 0.0.1\n"
    assert c.read_text() == '{"mine": true}\n'


def test_a_download_that_is_not_the_pin_is_refused(tmp_path):
    bindir, _log = _fake_curl(tmp_path, "0.0.9")
    b = tmp_path / "bin" / "dantesync"
    r = _bash("dantesync_install_binary", sources=(DS_INSTALL,), path_prefix=bindir,
              env={"DANTESYNC_INSTALL_BIN": str(b)})
    assert r.returncode != 0
    assert "not the pin" in r.stderr
    assert not b.exists()


# --- G6: the intercom-hub binary --------------------------------------------------------------------

def test_a_prestaged_hub_artifact_is_installed_when_absent(tmp_path):
    art = tmp_path / "art"
    art.mkdir()
    (art / "intercom-hub").write_text("#!/bin/sh\necho hub\n")
    dest = tmp_path / "bin" / "intercom-hub"
    r = _bash('strih_intercom_hub_binary_install "%s"' % dest, sources=(CI_ART,), env={"STRIH_LX_HUB_SRC": str(art)})
    assert r.returncode == 0, r.stderr
    assert os.access(dest, os.X_OK) and dest.read_text() == "#!/bin/sh\necho hub\n"
    assert art.is_dir(), "a pre-staged dir is never removed"


def test_an_installed_hub_is_kept(tmp_path):
    dest = tmp_path / "intercom-hub"
    dest.write_text("old\n")
    dest.chmod(0o755)
    art = tmp_path / "art"
    art.mkdir()
    (art / "intercom-hub").write_text("new\n")
    r = _bash('strih_intercom_hub_binary_install "%s"' % dest, sources=(CI_ART,), env={"STRIH_LX_HUB_SRC": str(art)})
    assert r.returncode == 0, r.stderr
    assert dest.read_text() == "old\n"


def test_no_token_and_no_prestaged_dir_warns_and_installs_nothing(tmp_path):
    dest = tmp_path / "intercom-hub"
    r = _bash('strih_intercom_hub_binary_install "%s"' % dest, sources=(CI_ART,))
    assert r.returncode == 0
    assert "ABSENT" in r.stderr and not dest.exists()


def _fake_github(tmp_path, branch_of_newest="dev"):
    """A curl that answers the GitHub artifacts API with two artifacts (newest on BRANCH_OF_NEWEST, the
    older on main) and serves each zip; it records its argv and the header it read on stdin."""
    import zipfile
    for name in ("newest", "main"):
        with zipfile.ZipFile(tmp_path / ("%s.zip" % name), "w") as z:
            z.writestr("intercom-hub", "#!/bin/sh\necho %s\n" % name)
    api = {"artifacts": [
        {"expired": False, "archive_download_url": "https://dl/newest", "workflow_run": {"head_branch": branch_of_newest}},
        {"expired": False, "archive_download_url": "https://dl/main", "workflow_run": {"head_branch": "main"}},
    ]}
    (tmp_path / "api.json").write_text(json.dumps(api))
    bindir = tmp_path / "ghbin"
    bindir.mkdir()
    (bindir / "curl").write_text(
        "#!/bin/bash\nprintf 'ARGV %%s\\n' \"$*\" >> %(d)s/gh.log\n"
        "hdr=\"\"; for a in \"$@\"; do [ \"$a\" = @- ] && hdr=\"$(cat)\"; done\n"
        "printf 'HDR %%s\\n' \"$hdr\" >> %(d)s/gh.log\nout=\"\"; url=\"\"\n"
        "while [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; -H) shift 2;; -*) shift;; *) url=\"$1\"; shift;; esac; done\n"
        "case \"$url\" in *actions/artifacts*) cat %(d)s/api.json;; https://dl/newest) cp %(d)s/newest.zip \"$out\";; "
        "https://dl/main) cp %(d)s/main.zip \"$out\";; *) exit 22;; esac\n" % {"d": tmp_path})
    (bindir / "curl").chmod(0o755)
    return bindir


def test_the_hub_comes_from_a_main_run_with_the_token_never_in_argv(tmp_path):
    bindir = _fake_github(tmp_path)
    tmproot = tmp_path / "tmproot"
    tmproot.mkdir()
    dest = tmp_path / "bin" / "intercom-hub"
    r = _bash('strih_intercom_hub_binary_install "%s"' % dest, sources=(CI_ART,), path_prefix=bindir,
              env={"GH_TOKEN": "tok-SECRET-1412", "STRIH_CI_ARTIFACT_TMP": str(tmproot)})
    assert r.returncode == 0, r.stderr
    assert dest.read_text() == "#!/bin/sh\necho main\n", "the newest NON-main artifact must be skipped"
    log = (tmp_path / "gh.log").read_text()
    assert "tok-SECRET-1412" not in "".join(ln for ln in log.splitlines() if ln.startswith("ARGV"))
    assert log.count("HDR Authorization: token tok-SECRET-1412") == 2
    assert list(tmproot.iterdir()) == [], "the fetch's temp dir is removed after the install"


def _fake_paged_github(tmp_path, dev_on_page_1=100):
    """A curl for a busy repo: page 1 of the artifacts API holds DEV_ON_PAGE_1 dev artifacts only,
    page 2 holds the newest main one, page 3 is empty. It answers by the `page=` query parameter
    (page 1 when absent) and logs every API url it was asked for."""
    import zipfile
    for name in ("dev", "main"):
        with zipfile.ZipFile(tmp_path / ("%s.zip" % name), "w") as z:
            z.writestr("intercom-hub", "#!/bin/sh\necho %s\n" % name)
    dev = {"expired": False, "archive_download_url": "https://dl/dev", "workflow_run": {"head_branch": "dev"}}
    main = {"expired": False, "archive_download_url": "https://dl/main", "workflow_run": {"head_branch": "main"}}
    (tmp_path / "page1.json").write_text(json.dumps({"total_count": dev_on_page_1 + 1, "artifacts": [dev] * dev_on_page_1}))
    (tmp_path / "page2.json").write_text(json.dumps({"total_count": dev_on_page_1 + 1, "artifacts": [main]}))
    (tmp_path / "page3.json").write_text(json.dumps({"total_count": dev_on_page_1 + 1, "artifacts": []}))
    bindir = tmp_path / "ghbin"
    bindir.mkdir()
    (bindir / "curl").write_text(
        "#!/bin/bash\nprintf 'ARGV %%s\\n' \"$*\" >> %(d)s/gh.log\n"
        "for a in \"$@\"; do [ \"$a\" = @- ] && cat > /dev/null; done\nout=\"\"; url=\"\"\n"
        "while [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; -H) shift 2;; -*) shift;; *) url=\"$1\"; shift;; esac; done\n"
        "case \"$url\" in *actions/artifacts*) p=1; case \"$url\" in *page=2*) p=2;; *page=3*) p=3;; esac; "
        "[ \"$p\" -le 3 ] || p=3; cat %(d)s/page$p.json;; https://dl/dev) cp %(d)s/dev.zip \"$out\";; "
        "https://dl/main) cp %(d)s/main.zip \"$out\";; *) exit 22;; esac\n" % {"d": tmp_path})
    (bindir / "curl").chmod(0o755)
    return bindir


def test_the_hub_is_found_on_a_later_page_when_dev_artifacts_fill_the_first_1412(tmp_path):
    """Live 11.10.2026 (new strih notebook): dev CI fills the first page of the artifacts API, so the
    newest main hub artifact sits on page 2+. A fetch that reads one page found no main artifact and
    left the box without a hub. A branch-filtered fetch must page on until it finds one."""
    bindir = _fake_paged_github(tmp_path)
    tmproot = tmp_path / "tmproot"
    tmproot.mkdir()
    dest = tmp_path / "bin" / "intercom-hub"
    r = _bash('strih_intercom_hub_binary_install "%s"' % dest, sources=(CI_ART,), path_prefix=bindir,
              env={"GH_TOKEN": "tok-SECRET-1412", "STRIH_CI_ARTIFACT_TMP": str(tmproot)})
    assert r.returncode == 0, r.stderr
    assert dest.exists(), "the main hub on page 2 must be found:\n" + r.stderr
    assert dest.read_text() == "#!/bin/sh\necho main\n"
    log = (tmp_path / "gh.log").read_text()
    assert "tok-SECRET-1412" not in log, "the token never reaches curl's argv"
    assert "page=2" in log


def test_a_branch_fetch_with_no_match_on_any_page_fails_cleanly_1412(tmp_path):
    bindir = _fake_paged_github(tmp_path)
    (tmp_path / "page2.json").write_text(json.dumps({"artifacts": []}))
    tmproot = tmp_path / "tmproot"
    tmproot.mkdir()
    r = _bash('strih_ci_artifact_fetch intercom-hub-linux-amd64 "" main', sources=(CI_ART,), path_prefix=bindir,
              env={"GH_TOKEN": "t", "STRIH_CI_ARTIFACT_TMP": str(tmproot)})
    assert r.returncode == 1 and "could not fetch" in r.stderr
    assert list(tmproot.iterdir()) == [], "no temp dir left behind"
    assert "page=3" not in (tmp_path / "gh.log").read_text(), "an empty page ends the walk"


def test_bkshading_still_takes_the_newest_artifact_of_any_branch(tmp_path):
    bindir = _fake_github(tmp_path)
    r = _bash('d="$(strih_ci_artifact_fetch some-art "")" && cat "$d/intercom-hub" && strih_ci_artifact_cleanup "$d" ""',
              sources=(CI_ART,), path_prefix=bindir,
              env={"GH_TOKEN": "t", "STRIH_CI_ARTIFACT_TMP": str(tmp_path)})
    assert r.returncode == 0, r.stderr
    assert "echo newest" in r.stdout


def test_cleanup_never_removes_a_prestaged_dir_even_inside_an_old_fetch_leftover(tmp_path):
    old = tmp_path / "strih-ci-artifact.keep"
    pre = old / "x"
    pre.mkdir(parents=True)
    (pre / "intercom-hub").write_text("hub\n")
    env = {"STRIH_CI_ARTIFACT_TMP": str(tmp_path)}
    r = _bash('strih_ci_artifact_cleanup "%s" "%s"' % (pre, pre), sources=(CI_ART,), env=env)
    assert r.returncode == 0 and pre.is_dir()
    # a path outside the fetch's temp root, or not ending in /x, is never removed either
    other = tmp_path / "elsewhere" / "strih-ci-artifact.zz" / "x"
    other.mkdir(parents=True)
    r = _bash('strih_ci_artifact_cleanup "%s"; strih_ci_artifact_cleanup "%s"' % (other, old), sources=(CI_ART,), env=env)
    assert r.returncode == 0 and other.is_dir() and old.is_dir()


def test_the_new_libs_run_clean_under_the_callers_errexit(tmp_path):
    """setup-strih / verify-strih call these under set -euo pipefail: a skip, an absent artifact, a
    kept binary and the cleanup must all return without aborting the caller."""
    env = _pp_filled(tmp_path)
    b = tmp_path / "ds"
    b.write_text("#!/bin/sh\n")
    b.chmod(0o755)
    c = tmp_path / "cfg.json"
    c.write_text("{}\n")
    env.update({"DANTESYNC_INSTALL_BIN": str(b), "DANTESYNC_INSTALL_CONFIG": str(c)})
    body = ("set -euo pipefail\n" + "".join('. "%s"\n' % x for x in (FACTS, PROVISION)) +
            "strih_box_load strih-pp\n"
            'if WHY="$(obs_box_step_applies 11b)"; then echo RAN; else echo "SKIP11b"; fi\n'
            'if WHY="$(obs_box_step_applies 16e)"; then echo RAN; else echo "SKIP16e"; fi\n'
            'dantesync_install server "$(strih_lx_dante_gm)"\n'
            'strih_intercom_hub_binary_install "%s/hub"\n'
            'strih_ci_artifact_cleanup "" ""\nstrih_ci_artifact_cleanup /nonexistent/x\n'
            "echo END\n" % tmp_path)
    full = {k: v for k, v in os.environ.items() if k not in LEAKY}
    full.update(env)
    r = subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=full, cwd=str(REPO), timeout=60)
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines()[0:2] == ["SKIP11b", "SKIP16e"] and r.stdout.rstrip().endswith("END")


def test_an_artifact_without_the_binary_fails(tmp_path):
    art = tmp_path / "art"
    art.mkdir()
    (art / "README").write_text("x\n")
    r = _bash('strih_intercom_hub_binary_install "%s"' % (tmp_path / "hub"), sources=(CI_ART,),
              env={"STRIH_LX_HUB_SRC": str(art)})
    assert r.returncode != 0


# --- G7 + G11: the per-box step gate ----------------------------------------------------------------

def _applies(env, step):
    r = _bash('strih_box_load strih-%s || exit 9\nif WHY="$(obs_box_step_applies %s)"; then echo RUN; '
              'else echo "SKIP $WHY"; fi' % (env[0], step), env=env[1], sources=(FACTS, ROLE))
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_strih_lx_skips_the_irq_unit_and_runs_the_sampler():
    # issue 1411: the new strih-lx rig NIC is the 10 GbE Thunderbolt Aquantia (PCIe, not USB).
    assert _applies(("lx", None), "11b").startswith("SKIP the rig NIC is not a USB device")
    assert _applies(("lx", None), "16e") == "RUN"
    assert _applies(("lx", None), "16c") == "RUN"


def test_strih_pp_skips_the_irq_unit_and_the_sampler_by_fact(tmp_path):
    env = _pp_filled(tmp_path)
    assert _applies(("pp", env), "11b").startswith("SKIP the rig NIC is not a USB device")
    assert _applies(("pp", env), "16e").startswith("SKIP STRIH_PROGRAM_AUDIO_SAMPLER=off")
    assert _applies(("pp", env), "16c") == "RUN"


# --- intercom.strih-pp.toml vs the hub's Matrix::from_toml rules -------------------------------------

KNOWN_ROLES = {"cambox", "program_ref", "cutters", "phones", "speakers", "line34", "program_monitor",
               "program_out"}


def test_the_pp_matrix_satisfies_the_hub_load_rules():
    m = tomllib.loads(PP_TOML.read_text())
    assert m["hub"]["sample_rate"] == 48000 and m["hub"]["block_frames"] > 0
    assert "janus" not in m and "video" not in m and "point" not in m
    parts = m["participant"]
    assert len({p["name"] for p in parts}) == len(parts)
    assert all(p["role"] in KNOWN_ROLES and p["adapter"] in {"vban", "none", "janus", "pipewire"} for p in parts)
    assert not [p for p in parts if p["role"] == "cambox"], "the PP hub sends no VBAN to a cambox"
    assert not [p for p in parts if p.get("out_stream")], "no VBAN output at all"
    vban_in = {p["in_stream"] for p in parts if p["adapter"] == "vban" and p.get("in_stream")}
    outs = [p for p in parts if p["role"] == "program_out"]
    assert len(outs) == 1
    o = outs[0]
    assert o["adapter"] == "pipewire" and o["pipewire_target"] == "strih-program" and "pipewire_source" not in o
    assert o["source_streams"] == ["strih"] and set(o["source_streams"]) <= vban_in
    src = next(p for p in parts if p["name"] == "lv1")
    assert (src["role"], src["host"], src["in_channels"]) == ("program_ref", "lv1.lan", 2)


# --- the setup / verify wiring ----------------------------------------------------------------------

def test_setup_strih_wires_g5_g6_g7_g11_and_stays_in_budget():
    s = SETUP.read_text()
    assert len(s.splitlines()) <= 1000
    ds = s.index('dantesync_install "$DS_ROLE" "$(strih_lx_dante_gm)"')
    assert s.index('step 2 "') < ds < s.index('strih_dantesync_install "$DS_UNIT_TEXT"')
    hub = s.index("strih_intercom_hub_binary_install /usr/local/bin/intercom-hub")
    assert s.index("step 13 ") < hub < s.index("step 14 ")
    g11b = s.index('if WHY="$(obs_box_step_applies 11b)"; then')
    assert s.index('step "11b"') < g11b < s.index("strih_nic_irq_affinity_script_text >") < s.index("step 12 ")
    g16e = s.index('if WHY="$(obs_box_step_applies 16e)"; then')
    assert s.index('step "16e"') < g16e < s.index('strih_program_audio_install "${HERE}/..') < s.index("step 17 ")
    # 16c uses the ONE shared fetch, no inline copy of it
    assert 'strih_ci_artifact_fetch "$BKSH_ART"' in s and "archive_download_url" not in s


def test_verify_strih_gates_items_16_and_41_by_the_same_rule():
    v = VERIFY.read_text()
    a = v.index('if WHY="$(obs_box_step_applies 11b)"; then')
    assert a < v.index('IRQ_TARGET_IP="${STRIH_LX_TARGET_IP:-$(strih_lx_ip)}"')
    b = v.index('if WHY="$(obs_box_step_applies 16e)"; then')
    assert b < v.index('strih_program_audio_grade_report "${HERE}/.."')


def test_the_new_libs_are_source_only_and_sourced_by_strih_provision():
    p = PROVISION.read_text()
    for lib in (DS_INSTALL, CI_ART, ROLE):
        assert '. "$(dirname "${BASH_SOURCE[0]}")/%s"' % lib.name in p
        head = lib.read_text().splitlines()[:3]
        assert any("airuleset:script-ok" in ln for ln in head)
        assert not any(ln.startswith("set -") for ln in lib.read_text().splitlines())
