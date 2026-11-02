"""issue 1417: the tally firmware lives in the repo without the venue WiFi credentials."""
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
INO = REPO / "tally/esp/WsTally/WsTally.ino"


def test_firmware_is_in_the_repo():
    s = INO.read_text()
    assert '#define HOST "atemrpi.local"' in s
    assert "#define PORT 9240" in s
    assert 'String message = "JOIN " + hostname;' in s


def test_firmware_reads_credentials_from_secrets_h():
    s = INO.read_text()
    assert '#include "secrets.h"' in s
    assert re.search(r'^\s*#define\s+(SSID|PASS)\s+"', s, re.M) is None
    assert "WiFi.begin(WIFI_SSID, WIFI_PASS);" in s


def test_secrets_h_is_ignored_and_the_example_is_not():
    ign = subprocess.run(["git", "check-ignore", "tally/esp/WsTally/secrets.h"], cwd=REPO)
    assert ign.returncode == 0
    ex = subprocess.run(["git", "check-ignore", "tally/esp/WsTally/secrets.h.example"], cwd=REPO)
    assert ex.returncode == 1
    t = (REPO / "tally/esp/WsTally/secrets.h.example").read_text()
    assert '#define WIFI_SSID "' in t and '#define WIFI_PASS "' in t


PP_INO = REPO / "tally/esp/TallyPP/TallyPP.ino"


def test_pp_build_is_in_the_repo_without_wifi_credentials():
    # the owner's PP light build (30.10.2026): dials tally.lan, reads TALLY-PVW / TALLY-PGM;
    # the WiFi values are left empty in git, never the venue's real ones
    s = PP_INO.read_text()
    assert '#define HOST "tally.lan"' in s
    assert "#define PORT 9240" in s
    assert re.search(r'^#define SSID ""$', s, re.M) and re.search(r'^#define PASS ""$', s, re.M)
    assert 'cmd.equals("TALLY-PGM") || cmd.equals("TALLY-PVW")' in s
