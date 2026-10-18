"""issue 1399 (design 6142127441): the intercom + shading TLS front end runs ON the strih box.

Owner 1.10.2026: "MA TO BYT SAMOBEZIACE NA STRIH NB NEMA TO BYT ZAVISLE NA INOM PC AKO DEV1". The hot
fix copied dev1's nginx sites and Let's Encrypt certificates to strih-snv BY HAND; nothing provisioned
them and nothing renews them. Pinned here:

  * the per-box facts STRIH_HTTPS_INTERKOM_HOSTS / STRIH_HTTPS_SHADING_HOST (the loader validates a DNS
    name list or `none`; the stream role declares none) with the values the design lists;
  * the SNV render keeps the routes of the live strih-snv hot-fix sites (issue body, 1.10.2026):
    interkom `/` -> 127.0.0.1:8790 (hub) + `/janus` -> <STRIH_IP>:8188 (Janus binds ws_ip = the box IP),
    shading `/` -> 127.0.0.1:8770 (bkshading);
  * the cert decision (missing -> issue, SAN set differs -> reissue, current -> nothing) and the
    certbot argv (DNS-01 through /etc/letsencrypt/cloudflare.ini, --cert-name <first host>);
  * setup-strih step 16i (`strih_https_install`): packages with --no-upgrade only when missing, the
    deploy hook, the sites written only on a difference, `nginx -t` BEFORE any reload (a failing test
    restores the old files and reloads nothing), certbot.timer enabled, an /etc/nginx/ssl hand copy
    used until a live cert exists, a missing cloudflare.ini = ONE WARN naming the supervisor step and
    never a secret in the output;
  * verify-strih item 45 (`strih_https_grade_rows`): nginx + :443 + the sites, each host's served cert
    (covers the name, > 14 days, from /etc/letsencrypt/live -- a hand copy is a NOTE), the upstreams
    through nginx, renewal; a box that declares no sites gets one NOTE;
  * the role gate (16i = the strih role only) and the setup / verify wiring within the line budget.

Every bash run sources the libs under the caller's `set -euo pipefail`; values reach bash as
ARGUMENTS, never inside the script text. Fake apt-get / dpkg-query / systemctl / nginx / certbot /
openssl / curl / ss on PATH. No root, no rig, no network.
"""
import os
import re
import stat
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-https.sh"
SHADING = SCRIPTS / "lib" / "shading-https.sh"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
ROLE = SCRIPTS / "lib" / "obs-box-role.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_BOXES_DIR", "OBS_FLEET")
FAKE_TOKEN = "cf-fake-zone-token-0123456789"

SNV_INTERKOM = ["interkom-lx.newlevel.media", "interkom.newlevel.media", "interkom-snv.newlevel.media"]
SNV_SHADING = "shading.newlevel.media"
PP_INTERKOM = ["interkom-pp.newlevel.media"]
PP_SHADING = "shadingpp.newlevel.media"

# The routes of the live strih-snv hot-fix sites (issue 1399 body, 1.10.2026: written by hand on the box
# with the same routes as dev1's): location -> proxy_pass.
HOTFIX_ROUTES = {
    "interkom": {"/": "http://127.0.0.1:8790", "= /janus": "http://10.77.9.202:8188"},
    "shading": {"/": "http://127.0.0.1:8770"},
}


# --- the fakes ---------------------------------------------------------------------------------------------

def _py(body):
    return "#!%s\n%s" % (sys.executable, body)


_FAKE_DPKG = _py("""import os, sys
pkg = sys.argv[-1]
try:
    inst = open(os.path.join(os.environ["FAKE_STATE"], "installed")).read().split()
except FileNotFoundError:
    inst = []
if pkg in inst:
    sys.stdout.write("install ok installed")
""")

_FAKE_APT = _py("""import os, sys
st = os.environ["FAKE_STATE"]
open(os.path.join(st, "calls"), "a").write("apt-get " + " ".join(sys.argv[1:]) + "\\n")
with open(os.path.join(st, "installed"), "a") as f:
    for a in sys.argv[1:]:
        if not a.startswith("-") and a != "install":
            f.write(a + "\\n")
""")

_FAKE_SYSTEMCTL = _py("""import os, sys
st = os.environ["FAKE_STATE"]
a = sys.argv[1:]
open(os.path.join(st, "calls"), "a").write("systemctl " + " ".join(a) + "\\n")
def flag(kind, u):
    return os.path.join(st, "%s-%s" % (kind, u))
verb = a[0]
if verb == "is-enabled":
    if os.path.exists(flag("enabled", a[1])):
        print("enabled")
    else:
        print("disabled"); sys.exit(1)
elif verb == "is-active":
    if os.path.exists(flag("active", a[1])):
        print("active")
    else:
        print("inactive"); sys.exit(3)
elif verb == "enable":
    rest = a[1:]
    now = rest and rest[0] == "--now"
    for u in rest[1:] if now else rest:
        open(flag("enabled", u), "w").close()
        if now:
            open(flag("active", u), "w").close()
elif verb in ("start", "restart", "reload"):
    for u in a[1:]:
        open(flag("active", u), "w").close()
""")

_FAKE_NGINX = _py("""import os, sys
st = os.environ["FAKE_STATE"]
open(os.path.join(st, "calls"), "a").write("nginx " + " ".join(sys.argv[1:]) + "\\n")
if "-t" in sys.argv[1:] and os.path.exists(os.path.join(st, "nginx-t-fails")):
    sys.stderr.write("nginx: configuration file test failed\\n"); sys.exit(1)
""")

# certbot: logs its argv, then writes live/<cert-name>/{fullchain,privkey}.pem with the -d names as SANs.
_FAKE_CERTBOT = _py("""import os, sys
st = os.environ["FAKE_STATE"]
a = sys.argv[1:]
sys.stdin.read()
open(os.path.join(st, "calls"), "a").write("certbot " + " ".join(a) + "\\n")
if os.path.exists(os.path.join(st, "certbot-fails")):
    sys.stderr.write("certbot: DNS API error\\n"); sys.exit(1)
names = [a[i + 1] for i, x in enumerate(a) if x == "-d"]
cn = a[a.index("--cert-name") + 1] if "--cert-name" in a else names[0]
d = os.path.join(os.environ["FAKE_LE"], "live", cn)
os.makedirs(d, exist_ok=True)
pem = "-----BEGIN CERTIFICATE-----\\nSAN=%s\\nEND=ok\\n-----END CERTIFICATE-----\\n" % ",".join(names)
open(os.path.join(d, "fullchain.pem"), "w").write(pem)
open(os.path.join(d, "privkey.pem"), "w").write("KEY\\n")
""")

# openssl: `s_client ... -servername H` prints the fake cert of H from $FAKE_STATE/served/H;
# `x509 [-in F]` reads the fake PEM (SAN=a,b / END=ok|soon) and answers -ext subjectAltName,
# -checkend N and -enddate.
_FAKE_OPENSSL = _py("""import os, sys
st = os.environ["FAKE_STATE"]
a = sys.argv[1:]
open(os.path.join(st, "calls"), "a").write("openssl " + " ".join(a) + "\\n")
if a[0] == "s_client":
    host = a[a.index("-servername") + 1]
    p = os.path.join(st, "served", host)
    if os.path.exists(p):
        sys.stdout.write("CONNECTED(00000003)\\n" + open(p).read() + "---\\n")
        sys.exit(0)
    sys.exit(1)
if a[0] == "x509":
    text = open(a[a.index("-in") + 1]).read() if "-in" in a else sys.stdin.read()
    if "BEGIN CERTIFICATE" not in text:
        sys.stderr.write("unable to load certificate\\n"); sys.exit(1)
    kv = dict(l.split("=", 1) for l in text.splitlines() if "=" in l)
    if "-fingerprint" in a:
        import hashlib
        block = text[text.index("-----BEGIN CERTIFICATE-----"):text.index("-----END CERTIFICATE-----")]
        print("sha256 Fingerprint=" + hashlib.sha256(block.encode()).hexdigest().upper())
    if "-ext" in a:
        print("X509v3 Subject Alternative Name: ")
        print("    " + ", ".join("DNS:" + n for n in kv.get("SAN", "").split(",") if n))
    if "-enddate" in a:
        print("notAfter=Dec 19 09:00:00 2026 GMT")
    if "-checkend" in a and kv.get("END") != "ok":
        print("Certificate will expire"); sys.exit(1)
    sys.exit(0)
sys.exit(2)
""")

# curl: answers from $FAKE_STATE/http/<host>_<path with / -> _>, absent = no answer. `-D -` prints the
# status line as received (how a 101 is read: curl keeps an upgraded connection open until --max-time),
# `-w %{http_code}` the code (000 when nothing answered).
_FAKE_CURL = _py("""import os, sys
st = os.environ["FAKE_STATE"]
a = sys.argv[1:]
open(os.path.join(st, "calls"), "a").write("curl " + " ".join(a) + "\\n")
url = [x for x in a if x.startswith("https://")][-1]
rest = url[len("https://"):]
host, _, path = rest.partition("/")
key = host + "_" + path.replace("/", "_")
p = os.path.join(st, "http", key)
code = open(p).read().strip() if os.path.exists(p) else ""
if "-D" in a and a[a.index("-D") + 1] == "-" and code:
    sys.stdout.write("HTTP/1.1 %s X\\r\\nServer: fake\\r\\n\\r\\n" % code)
if "-w" in a:
    sys.stdout.write(code or "000")
sys.exit(0 if code else 7)
""")

_FAKE_SS = _py("""import os, sys
st = os.environ["FAKE_STATE"]
if os.path.exists(os.path.join(st, "listen-443")):
    print("LISTEN 0 511 0.0.0.0:443 0.0.0.0:*")
""")

FAKES = {"dpkg-query": _FAKE_DPKG, "apt-get": _FAKE_APT, "systemctl": _FAKE_SYSTEMCTL,
         "nginx": _FAKE_NGINX, "certbot": _FAKE_CERTBOT, "openssl": _FAKE_OPENSSL,
         "curl": _FAKE_CURL, "ss": _FAKE_SS}


class Rig:
    """A temp root: fake binaries, the nginx + letsencrypt dirs, the fake state."""

    def __init__(self, tmp):
        self.tmp = tmp
        self.bin = tmp / "bin"
        self.state = tmp / "state"
        self.nginx = tmp / "etc-nginx"
        self.le = tmp / "etc-letsencrypt"
        for d in (self.bin, self.state, self.nginx / "sites-available", self.nginx / "sites-enabled",
                  self.le, self.state / "served", self.state / "http"):
            d.mkdir(parents=True, exist_ok=True)
        for name, body in FAKES.items():
            p = self.bin / name
            p.write_text(body)
            p.chmod(0o755)

    def env(self, **extra):
        e = {k: v for k, v in os.environ.items() if k not in LEAKY}
        e.update(PATH="%s:%s" % (self.bin, os.environ["PATH"]), FAKE_STATE=str(self.state),
                 FAKE_LE=str(self.le), STRIH_HTTPS_NGINX_DIR=str(self.nginx),
                 STRIH_HTTPS_LE_DIR=str(self.le), STRIH_HTTPS_INI_OWNER=self._me())
        e.update(extra)
        return e

    @staticmethod
    def _me():
        import pwd
        return pwd.getpwuid(os.getuid()).pw_name

    def installed(self, *pkgs):
        (self.state / "installed").write_text("".join(p + "\n" for p in pkgs))

    def calls(self):
        p = self.state / "calls"
        return p.read_text().splitlines() if p.exists() else []

    def ini(self, mode=0o600):
        p = self.le / "cloudflare.ini"
        p.write_text("dns_cloudflare_api_token = %s\n" % FAKE_TOKEN)
        p.chmod(mode)
        return p

    def live_cert(self, name, sans, end="ok"):
        d = self.le / "live" / name
        d.mkdir(parents=True, exist_ok=True)
        (d / "fullchain.pem").write_text(
            "-----BEGIN CERTIFICATE-----\nSAN=%s\nEND=%s\n-----END CERTIFICATE-----\n" % (",".join(sans), end))
        (d / "privkey.pem").write_text("KEY\n")
        return d

    def serve(self, host, sans, end="ok", serial=""):
        extra = "SERIAL=%s\n" % serial if serial else ""
        (self.state / "served" / host).write_text(
            "-----BEGIN CERTIFICATE-----\nSAN=%s\nEND=%s\n%s-----END CERTIFICATE-----\n"
            % (",".join(sans), end, extra))

    def http(self, host, path, code):
        (self.state / "http" / (host + "_" + path.lstrip("/").replace("/", "_"))).write_text(code)

    def flag(self, name):
        (self.state / name).write_text("")


def _bash(script, *args, env=None, sources=(FACTS, LIB)):
    pre = "set -euo pipefail\n" + "".join('. "%s"\n' % s for s in sources)
    e = env if env is not None else {k: v for k, v in os.environ.items() if k not in LEAKY}
    return subprocess.run(["bash", "-c", pre + script, "harness", *args],
                          capture_output=True, text=True, env=e, timeout=120)


def _install(rig, box="strih-snv", **extra):
    return _bash('strih_box_load "$1" || exit 9\nstrih_https_install', box, env=rig.env(**extra))


def _rows(rig, box="strih-snv", **extra):
    r = _bash('strih_box_load "$1" || exit 9\nstrih_https_grade_rows', box, env=rig.env(**extra))
    assert r.returncode == 0, r.stderr
    return [ln.split("|", 1) for ln in r.stdout.splitlines() if ln]


def _routes(site_text):
    """location -> proxy_pass of a rendered nginx site (the :443 block)."""
    out = {}
    for m in re.finditer(r"location\s+([^{]+?)\s*\{(.*?)\n\s*\}", site_text, re.S):
        pp = re.search(r"proxy_pass\s+(\S+);", m.group(2))
        out[m.group(1).strip()] = pp.group(1) if pp else None
    return out


def _server_names(site_text):
    return sorted(set(re.findall(r"server_name\s+([^;]+);", site_text)))


# --- the facts ---------------------------------------------------------------------------------------------

def _facts(box):
    r = _bash('strih_box_load "$1" || exit 9\nprintf "%s|%s\\n" "$(strih_lx_https_interkom_hosts)" '
              '"$(strih_lx_https_shading_host)"', box)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_the_https_keys_close_the_fact_list():
    r = _bash("strih_box_fact_keys", sources=(FACTS,))
    # issue 1424 part 2 appended the HDMI output list after them, issue 1453 the public-intercom switch.
    assert r.stdout.split()[-4:] == ["STRIH_HTTPS_INTERKOM_HOSTS", "STRIH_HTTPS_SHADING_HOST", "STRIH_HDMI_OUTPUTS",
                                     "STRIH_HTTPS_PUBLIC"]


def test_strih_snv_declares_the_three_interkom_names_and_shading():
    assert _facts("strih-snv") == "%s|%s" % (" ".join(SNV_INTERKOM), SNV_SHADING)


def test_strih_pp_declares_interkom_pp_and_shadingpp():
    assert _facts("strih-pp") == "%s|%s" % (" ".join(PP_INTERKOM), PP_SHADING)


def test_the_stream_role_declares_none():
    assert _facts("stream-pp") == "none|none"


def _boxes_copy(tmp_path):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text())
    return d


@pytest.mark.parametrize("box,key,value", [
    ("strih-snv", "STRIH_HTTPS_INTERKOM_HOSTS", "Interkom.newlevel.media"),
    ("strih-snv", "STRIH_HTTPS_INTERKOM_HOSTS", "interkom"),
    ("strih-snv", "STRIH_HTTPS_INTERKOM_HOSTS", "interkom.newlevel.media interkom.newlevel.media"),
    ("strih-snv", "STRIH_HTTPS_INTERKOM_HOSTS", "-x.newlevel.media"),
    ("strih-snv", "STRIH_HTTPS_INTERKOM_HOSTS", "a..newlevel.media"),
    ("strih-snv", "STRIH_HTTPS_INTERKOM_HOSTS", "none interkom.newlevel.media"),
    ("strih-snv", "STRIH_HTTPS_INTERKOM_HOSTS", "interkom.newlevel.media none"),
    ("strih-snv", "STRIH_HTTPS_SHADING_HOST", "shading.newlevel.media shading2.newlevel.media"),
    ("strih-snv", "STRIH_HTTPS_SHADING_HOST", "shading_x.newlevel.media"),
    # a name in BOTH facts: one name, two sites
    ("strih-snv", "STRIH_HTTPS_SHADING_HOST", "interkom.newlevel.media"),
    # the stream role serves no intercom/shading page
    ("stream-pp", "STRIH_HTTPS_INTERKOM_HOSTS", "interkom-pp.newlevel.media"),
    ("stream-pp", "STRIH_HTTPS_SHADING_HOST", "shadingpp.newlevel.media"),
])
def test_a_malformed_https_fact_refuses_by_name(tmp_path, box, key, value):
    d = _boxes_copy(tmp_path)
    f = d / ("%s.env" % box)
    lines = f.read_text().splitlines(keepends=True)
    hit = [i for i, ln in enumerate(lines) if ln.startswith(key + "=")]
    assert len(hit) == 1, key
    lines[hit[0]] = "%s=%s\n" % (key, value)
    f.write_text("".join(lines))
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e["STRIH_BOXES_DIR"] = str(d)
    r = _bash('strih_box_load "$1"', box, env=e, sources=(FACTS,))
    assert r.returncode != 0, r.stdout
    assert key in r.stderr, r.stderr


def test_a_bracket_range_never_validates_a_dns_name():
    """The issue-1302 locale trap: the name check spells its character sets out."""
    text = FACTS.read_text()
    for ln in text.splitlines():
        if "=~" in ln and "https" in ln.lower():
            assert not re.search(r"\[[^\]]*[a-zA-Z0-9]-[a-zA-Z0-9][^\]]*\]", ln), ln


# --- the render ------------------------------------------------------------------------------------------

def _sites(box="strih-snv"):
    r = _bash('strih_box_load "$1" || exit 9\nstrih_https_sites', box)
    assert r.returncode == 0, r.stderr
    return [ln.split("|") for ln in r.stdout.splitlines()]


def test_strih_snv_sites_carry_local_upstreams():
    assert _sites("strih-snv") == [
        ["interkom", "interkom-lx.newlevel.media", "interkom.newlevel.media interkom-snv.newlevel.media",
         "http://127.0.0.1:8790", "http://10.77.9.202:8188"],
        ["shading", "shading.newlevel.media", "", "http://127.0.0.1:8770", ""],
    ]


def test_strih_pp_sites_follow_the_pp_facts():
    assert _sites("strih-pp") == [
        ["interkom", "interkom-pp.newlevel.media", "", "http://127.0.0.1:8790", "http://10.77.8.202:8188"],
        ["shading", "shadingpp.newlevel.media", "", "http://127.0.0.1:8770", ""],
    ]


def test_the_stream_role_has_no_sites():
    assert _sites("stream-pp") == []


def _render(site, box="strih-snv", cert="", key=""):
    r = _bash('strih_box_load "$1" || exit 9\n'
              'while IFS="|" read -r s first aliases up janus; do\n'
              '  [ "$s" = "$2" ] || continue\n'
              '  strih_https_site_text "$s" "$first" "$aliases" "$up" "$janus" "$3" "$4"\n'
              'done < <(strih_https_sites)', box, site, cert or "/c/fullchain.pem", key or "/c/privkey.pem")
    assert r.returncode == 0, r.stderr
    return r.stdout


@pytest.mark.parametrize("site", ["interkom", "shading"])
def test_the_snv_render_keeps_the_live_hotfix_routes(site):
    text = _render(site)
    assert _routes(text) == HOTFIX_ROUTES[site], text


def test_the_snv_interkom_render_lists_every_name_on_one_cert():
    text = _render("interkom", cert="/x/live/interkom-lx.newlevel.media/fullchain.pem",
                   key="/x/live/interkom-lx.newlevel.media/privkey.pem")
    assert _server_names(text) == [" ".join(SNV_INTERKOM)]
    assert "ssl_certificate     /x/live/interkom-lx.newlevel.media/fullchain.pem;" in text
    assert "ssl_certificate_key /x/live/interkom-lx.newlevel.media/privkey.pem;" in text
    assert "listen 443 ssl http2;" in text and "return 301 https://$host$request_uri;" in text
    # the header names the box, never dev1's LAN IP
    assert "10.77.9.200" not in text and "dev1 nginx" not in text
    assert "strih-snv" in text and "10.77.9.202" in text


@pytest.mark.parametrize("box", ["strih-snv", "strih-pp"])
def test_the_upstream_ports_are_the_services_own_binds(box):
    """One source of truth: the hub port is the box's intercom matrix [http] bind, bkshading's is its
    default_bind (bkshading/service/src/config.rs)."""
    r = _bash('strih_box_load "$1" || exit 9\nstrih_lx_intercom_config', box, sources=(FACTS,))
    matrix = (REPO / r.stdout.strip()).read_text()
    hub = re.search(r'(?m)^bind = "0\.0\.0\.0:(\d+)"', matrix).group(1)
    cfg = (REPO / "bkshading" / "service" / "src" / "config.rs").read_text()
    bks = re.search(r'"0\.0\.0\.0:(\d+)"', cfg).group(1)
    sites = {row[0]: row for row in _sites(box)}
    assert sites["interkom"][3] == "http://127.0.0.1:%s" % hub
    assert sites["shading"][3] == "http://127.0.0.1:%s" % bks


def test_the_pp_render_follows_the_pp_box():
    text = _render("interkom", box="strih-pp")
    assert _routes(text) == {"/": "http://127.0.0.1:8790", "= /janus": "http://10.77.8.202:8188"}
    assert "10.77.9.202" not in text and "strih-snv" not in text


def test_the_dev1_default_render_is_unchanged():
    """The parametrisation keeps the committed dev1 confs byte-identical (the issue-808 drift guard)."""
    r = _bash('shading_https_site_content "$(shading_https_hostname)" "$(shading_https_upstream)"\n'
              'printf "=====\\n"\ninterkom_https_site_content', sources=(SHADING,))
    assert r.returncode == 0, r.stderr
    shading, interkom = r.stdout.split("=====\n")
    assert shading == (SCRIPTS / "nginx" / "shading.newlevel.media.conf").read_text()
    assert interkom == (SCRIPTS / "nginx" / "interkom.newlevel.media.conf").read_text()


# --- pure decisions -----------------------------------------------------------------------------------------

@pytest.mark.parametrize("want,have,action", [
    ("a.x b.x", "", "issue"),
    ("a.x b.x", "a.x b.x", "current"),
    ("a.x b.x", "b.x a.x", "current"),
    ("a.x b.x c.x", "a.x b.x", "reissue"),
    ("a.x", "a.x b.x", "reissue"),
])
def test_cert_action(want, have, action):
    r = _bash('strih_https_cert_action "$1" "$2"', want, have)
    assert r.returncode == 0, r.stderr
    assert r.stdout == action


def test_san_names_parse_the_openssl_ext_output():
    text = "X509v3 Subject Alternative Name: \n    DNS:a.newlevel.media, DNS:b.newlevel.media\n"
    r = _bash('strih_https_san_names <<<"$1"', text)
    assert r.stdout.split() == ["a.newlevel.media", "b.newlevel.media"]


@pytest.mark.parametrize("name,sans,ok", [
    ("a.x.media", "a.x.media b.x.media", True),
    ("c.x.media", "a.x.media b.x.media", False),
    ("c.x.media", "*.x.media", True),
    ("c.d.x.media", "*.x.media", False),
    ("x.media", "*.x.media", False),
])
def test_name_covered(name, sans, ok):
    r = _bash('strih_https_name_covered "$1" "$2"', name, sans)
    assert (r.returncode == 0) is ok


def test_certbot_argv_is_dns01_with_the_cert_name_always(tmp_path):
    rig = Rig(tmp_path)
    r = _bash('strih_https_certbot_argv "$1" "$2"', "shadingpp.newlevel.media", "", env=rig.env())
    a = r.stdout.splitlines()
    assert a[0] == "certonly"
    assert "--dns-cloudflare" in a
    assert a[a.index("--dns-cloudflare-credentials") + 1] == str(rig.le / "cloudflare.ini")
    assert a[a.index("--cert-name") + 1] == "shadingpp.newlevel.media"
    assert [a[i + 1] for i, x in enumerate(a) if x == "-d"] == ["shadingpp.newlevel.media"]
    assert "--non-interactive" in a and "--agree-tos" in a
    r = _bash('strih_https_certbot_argv "$1" "$2"', SNV_INTERKOM[0], " ".join(SNV_INTERKOM[1:]), env=rig.env())
    a = r.stdout.splitlines()
    assert [a[i + 1] for i, x in enumerate(a) if x == "-d"] == SNV_INTERKOM
    assert a.count("--cert-name") == 1 and a[a.index("--cert-name") + 1] == SNV_INTERKOM[0]


# --- setup-strih step 16i ------------------------------------------------------------------------------------

def _all_live(rig):
    rig.live_cert(SNV_INTERKOM[0], SNV_INTERKOM)
    rig.live_cert(SNV_SHADING, [SNV_SHADING])


def test_install_issues_both_certs_writes_the_sites_and_reloads(tmp_path):
    rig = Rig(tmp_path)
    rig.ini()
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = rig.calls()
    apt = [c for c in calls if c.startswith("apt-get")]
    assert len(apt) == 1 and "--no-upgrade" in apt[0]
    for p in ("nginx-light", "certbot", "python3-certbot-dns-cloudflare"):
        assert p in apt[0]
    certbot = [c for c in calls if c.startswith("certbot")]
    assert len(certbot) == 2
    assert "--cert-name interkom-lx.newlevel.media" in certbot[0]
    assert "--cert-name shading.newlevel.media" in certbot[1]
    for site, first in (("interkom", SNV_INTERKOM[0]), ("shading", SNV_SHADING)):
        text = (rig.nginx / "sites-available" / site).read_text()
        assert "ssl_certificate     %s/live/%s/fullchain.pem;" % (rig.le, first) in text
        link = rig.nginx / "sites-enabled" / site
        assert link.is_symlink() and os.readlink(link) == "../sites-available/%s" % site
    i_t = calls.index("nginx -t")
    later = calls[i_t + 1:]
    assert any(c in ("systemctl reload nginx", "systemctl enable --now nginx", "systemctl start nginx")
               for c in later), calls
    assert "systemctl enable --now certbot.timer" in calls
    hook = rig.le / "renewal-hooks" / "deploy" / "nginx-reload.sh"
    assert stat.S_IMODE(hook.stat().st_mode) == 0o755
    assert "systemctl reload nginx" in hook.read_text()
    assert FAKE_TOKEN not in r.stdout + r.stderr


def test_a_second_run_changes_nothing_and_issues_nothing(tmp_path):
    rig = Rig(tmp_path)
    rig.ini()
    assert _install(rig).returncode == 0
    (rig.state / "calls").write_text("")
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = rig.calls()
    assert not [c for c in calls if c.startswith(("certbot", "apt-get"))], calls
    assert "systemctl reload nginx" not in calls
    assert "unchanged" in r.stdout


def test_a_differing_san_set_reissues(tmp_path):
    rig = Rig(tmp_path)
    rig.ini()
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    rig.live_cert(SNV_INTERKOM[0], SNV_INTERKOM[:2])
    rig.live_cert(SNV_SHADING, [SNV_SHADING])
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    certbot = [c for c in rig.calls() if c.startswith("certbot")]
    assert len(certbot) == 1 and "--cert-name interkom-lx.newlevel.media" in certbot[0]
    assert "-d interkom-snv.newlevel.media" in certbot[0]


def test_a_missing_ini_is_one_warn_naming_the_supervisor_step_and_keeps_the_hand_copy(tmp_path):
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    ssl = rig.nginx / "ssl"
    ssl.mkdir()
    for n in ("interkom.crt", "interkom.key", "shading.crt", "shading.key"):
        (ssl / n).write_text("x\n")
    (rig.nginx / "sites-available" / "interkom").write_text(
        "server {\n    server_name interkom.newlevel.media;\n"
        "    ssl_certificate %s/interkom.crt;\n    ssl_certificate_key %s/interkom.key;\n}\n" % (ssl, ssl))
    (rig.nginx / "sites-available" / "shading").write_text(
        "server {\n    server_name shading.newlevel.media;\n"
        "    ssl_certificate %s/shading.crt;\n    ssl_certificate_key %s/shading.key;\n}\n" % (ssl, ssl))
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    out = r.stdout + r.stderr
    warns = [ln for ln in out.splitlines() if "cloudflare.ini" in ln and "WARN" in ln]
    assert len(warns) == 1, out
    assert "1399" in warns[0] and "supervisor" in warns[0]
    assert not [c for c in rig.calls() if c.startswith("certbot")]
    text = (rig.nginx / "sites-available" / "interkom").read_text()
    assert "ssl_certificate     %s/interkom.crt;" % ssl in text
    assert "ssl_certificate_key %s/interkom.key;" % ssl in text
    assert _routes(text) == {"/": "http://127.0.0.1:8790", "= /janus": "http://10.77.9.202:8188"}


def test_a_hand_copy_outside_nginx_ssl_is_never_used(tmp_path):
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    other = tmp_path / "elsewhere"
    other.mkdir()
    (other / "c.crt").write_text("x\n")
    (other / "c.key").write_text("x\n")
    (rig.nginx / "sites-available" / "shading").write_text(
        "server {\n    server_name shading.newlevel.media;\n"
        "    ssl_certificate %s/c.crt;\n    ssl_certificate_key %s/c.key;\n}\n" % (other, other))
    before = (rig.nginx / "sites-available" / "shading").read_text()
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    assert (rig.nginx / "sites-available" / "shading").read_text() == before
    assert "no certificate for shading.newlevel.media" in r.stdout + r.stderr


def test_the_ini_is_set_to_mode_600_and_never_printed(tmp_path):
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    ini = rig.ini(mode=0o644)
    _all_live(rig)
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    assert stat.S_IMODE(ini.stat().st_mode) == 0o600
    assert FAKE_TOKEN not in r.stdout + r.stderr
    assert not any(FAKE_TOKEN in c for c in rig.calls())


def test_a_failing_nginx_test_restores_the_old_sites_and_reloads_nothing(tmp_path):
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    rig.ini()
    _all_live(rig)
    old = "# the old interkom site\n"
    (rig.nginx / "sites-available" / "interkom").write_text(old)
    rig.flag("nginx-t-fails")
    r = _install(rig)
    assert r.returncode != 0
    calls = rig.calls()
    assert "nginx -t" in calls
    assert not [c for c in calls if c.startswith("systemctl") and "nginx" in c.split()[-1:]
                and c.split()[1] in ("reload", "restart", "start", "enable")], calls
    assert (rig.nginx / "sites-available" / "interkom").read_text() == old
    assert not (rig.nginx / "sites-available" / "shading").exists()
    assert not (rig.nginx / "sites-enabled" / "shading").is_symlink()


def test_a_failing_certbot_warns_and_keeps_serving(tmp_path):
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    rig.ini()
    rig.live_cert(SNV_INTERKOM[0], SNV_INTERKOM[:1])
    rig.live_cert(SNV_SHADING, [SNV_SHADING])
    rig.flag("certbot-fails")
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARN" in r.stdout + r.stderr and "certbot" in r.stdout + r.stderr
    assert (rig.nginx / "sites-available" / "interkom").exists()


def test_a_successful_reissue_after_a_failed_one_reloads_nginx(tmp_path):
    """Review finding: run 1's certbot fails, so the site already points at the live dir; run 2's
    certbot succeeds with the site text unchanged -- nginx must still be tested and reloaded, or it
    keeps serving the old certificate (a `certonly` fires no renewal deploy hook)."""
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    rig.ini()
    rig.live_cert(SNV_INTERKOM[0], SNV_INTERKOM[:1])
    rig.live_cert(SNV_SHADING, [SNV_SHADING])
    rig.flag("certbot-fails")
    assert _install(rig).returncode == 0
    (rig.state / "certbot-fails").unlink()
    (rig.state / "calls").write_text("")
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = rig.calls()
    assert any(c.startswith("certbot") for c in calls), calls
    assert "nginx -t" in calls, calls
    assert "systemctl reload nginx" in calls[calls.index("nginx -t"):], calls


@pytest.mark.parametrize("climbs", ["both", "cert", "key"])
def test_a_hand_copy_path_that_climbs_out_of_nginx_ssl_is_never_used(tmp_path, climbs):
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    (rig.nginx / "ssl").mkdir()
    out = rig.nginx / "outside"
    out.mkdir()
    (out / "c.crt").write_text("x\n")
    (out / "c.key").write_text("x\n")
    (rig.nginx / "ssl" / "c.crt").write_text("x\n")
    (rig.nginx / "ssl" / "c.key").write_text("x\n")
    climb = rig.nginx / "ssl" / ".." / "outside"
    inside = rig.nginx / "ssl"
    cert_dir = climb if climbs in ("both", "cert") else inside
    key_dir = climb if climbs in ("both", "key") else inside
    (rig.nginx / "sites-available" / "shading").write_text(
        "server {\n    server_name shading.newlevel.media;\n"
        "    ssl_certificate %s/c.crt;\n    ssl_certificate_key %s/c.key;\n}\n" % (cert_dir, key_dir))
    before = (rig.nginx / "sites-available" / "shading").read_text()
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    assert (rig.nginx / "sites-available" / "shading").read_text() == before
    assert "no certificate for shading.newlevel.media" in r.stdout + r.stderr


def test_a_box_that_declares_no_sites_installs_nothing(tmp_path):
    rig = Rig(tmp_path)
    d = _boxes_copy(tmp_path)
    f = d / "strih-pp.env"
    body = f.read_text()
    body = body.replace("STRIH_HTTPS_INTERKOM_HOSTS=interkom-pp.newlevel.media\n", "STRIH_HTTPS_INTERKOM_HOSTS=none\n")
    body = body.replace("STRIH_HTTPS_SHADING_HOST=shadingpp.newlevel.media\n", "STRIH_HTTPS_SHADING_HOST=none\n")
    # issue 1453: a box with no intercom site publishes nothing (the loader refuses STRIH_HTTPS_PUBLIC=yes).
    body = body.replace("STRIH_HTTPS_PUBLIC=yes\n", "STRIH_HTTPS_PUBLIC=no\n")
    f.write_text(body)
    r = _install(rig, box="strih-pp", STRIH_BOXES_DIR=str(d))
    assert r.returncode == 0, r.stdout + r.stderr
    assert rig.calls() == []
    assert "declares no" in r.stdout


def test_a_foreign_enabled_site_with_our_name_is_named(tmp_path):
    rig = Rig(tmp_path)
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    rig.ini()
    _all_live(rig)
    (rig.nginx / "sites-enabled" / "shadingpp-hand").write_text("server { server_name shading.newlevel.media; }\n")
    r = _install(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "shadingpp-hand" in r.stdout + r.stderr
    assert (rig.nginx / "sites-enabled" / "shadingpp-hand").exists()


# --- verify-strih item 45 ------------------------------------------------------------------------------------

def _healthy(rig):
    rig.installed("nginx-light", "certbot", "python3-certbot-dns-cloudflare")
    rig.ini()
    _all_live(rig)
    assert _install(rig).returncode == 0
    rig.flag("active-nginx")
    rig.flag("listen-443")
    for h in SNV_INTERKOM:
        rig.serve(h, SNV_INTERKOM)
    rig.serve(SNV_SHADING, [SNV_SHADING])
    rig.http(SNV_INTERKOM[0], "/api/state", "200")
    rig.http(SNV_INTERKOM[0], "/janus", "101")
    rig.http(SNV_SHADING, "/api/version", "200")


def test_a_healthy_box_passes_every_row(tmp_path):
    rig = Rig(tmp_path)
    _healthy(rig)
    rows = _rows(rig)
    assert len(rows) == 1 + 4 + 2 + 1, rows
    assert all(st == "OK" for st, _ in rows), rows
    joined = "\n".join(t for _, t in rows)
    for h in SNV_INTERKOM + [SNV_SHADING]:
        assert h in joined


def test_the_row_count_matches_the_declared_sites(tmp_path):
    rig = Rig(tmp_path)
    r = _bash('strih_box_load "$1" || exit 9\nstrih_https_row_count', "strih-snv", env=rig.env())
    assert r.stdout == "8"
    r = _bash('strih_box_load "$1" || exit 9\nstrih_https_row_count', "strih-pp", env=rig.env())
    assert r.stdout == "6"


@pytest.mark.parametrize("break_it,tag", [
    (lambda rig: (rig.state / "active-nginx").unlink(), "(https-nginx)"),
    (lambda rig: (rig.state / "listen-443").unlink(), "(https-nginx)"),
    (lambda rig: (rig.nginx / "sites-available" / "shading").write_text("# hand edit\n"), "(https-nginx)"),
    (lambda rig: (rig.nginx / "sites-enabled" / "interkom").unlink(), "(https-nginx)"),
    (lambda rig: rig.serve("interkom-snv.newlevel.media", SNV_INTERKOM[:2]), "(https-cert) interkom-snv"),
    (lambda rig: rig.serve(SNV_SHADING, [SNV_SHADING], end="soon"), "(https-cert) shading"),
    (lambda rig: (rig.state / "served" / SNV_SHADING).unlink(), "(https-cert) shading"),
    (lambda rig: rig.http(SNV_INTERKOM[0], "/api/state", "502"), "(https-upstream) interkom"),
    (lambda rig: rig.http(SNV_INTERKOM[0], "/janus", "400"), "(https-upstream) interkom"),
    (lambda rig: rig.http(SNV_SHADING, "/api/version", "000"), "(https-upstream) shading"),
    (lambda rig: (rig.state / "enabled-certbot.timer").unlink(), "(https-renewal)"),
    (lambda rig: (rig.le / "cloudflare.ini").unlink(), "(https-renewal)"),
    (lambda rig: (rig.le / "cloudflare.ini").chmod(0o644), "(https-renewal)"),
    (lambda rig: (rig.le / "renewal-hooks" / "deploy" / "nginx-reload.sh").unlink(), "(https-renewal)"),
])
def test_each_row_fails_by_name(tmp_path, break_it, tag):
    rig = Rig(tmp_path)
    _healthy(rig)
    break_it(rig)
    rows = _rows(rig)
    bad = [t for st, t in rows if st == "FAIL"]
    assert len(bad) == 1, rows
    assert bad[0].startswith(tag), bad


def test_a_served_cert_other_than_the_live_one_fails(tmp_path):
    """Review finding: `from /etc/letsencrypt/live` is CHECKED (fingerprint of the served leaf vs the
    live fullchain's first certificate), so an nginx still on an old certificate never reads OK."""
    rig = Rig(tmp_path)
    _healthy(rig)
    rig.serve(SNV_SHADING, [SNV_SHADING], serial="old")
    rows = _rows(rig)
    bad = [t for st, t in rows if st == "FAIL"]
    assert len(bad) == 1 and bad[0].startswith("(https-cert) shading"), rows
    assert "reload nginx" in bad[0]


def test_an_unreadable_live_dir_is_named_not_read_as_a_missing_cert(tmp_path):
    if os.geteuid() == 0:
        pytest.fail("needs a non-root user: root reads a 0000 directory")
    rig = Rig(tmp_path)
    _healthy(rig)
    live = rig.le / "live"
    live.chmod(0)
    try:
        rows = _rows(rig)
    finally:
        live.chmod(0o755)
    nginx_row = [t for st, t in rows if t.startswith("(https-nginx)")]
    assert nginx_row and "unreadable" in nginx_row[0] and "root" in nginx_row[0], rows


def test_a_hand_copy_cert_is_a_note_until_migrated(tmp_path):
    rig = Rig(tmp_path)
    _healthy(rig)
    ssl = rig.nginx / "ssl"
    ssl.mkdir()
    (ssl / "shading.crt").write_text("x\n")
    (ssl / "shading.key").write_text("x\n")
    import shutil
    shutil.rmtree(rig.le / "live" / SNV_SHADING)
    site = rig.nginx / "sites-available" / "shading"
    site.write_text(site.read_text()
                    .replace("%s/live/%s/fullchain.pem" % (rig.le, SNV_SHADING), "%s/shading.crt" % ssl)
                    .replace("%s/live/%s/privkey.pem" % (rig.le, SNV_SHADING), "%s/shading.key" % ssl))
    rows = _rows(rig)
    shading = [(st, t) for st, t in rows if t.startswith("(https-cert) shading")]
    assert shading and shading[0][0] == "NOTE", rows
    assert "hand copy" in shading[0][1]
    assert not [t for st, t in rows if st == "FAIL"], rows


def test_a_box_that_declares_no_sites_gets_one_note(tmp_path):
    rig = Rig(tmp_path)
    d = _boxes_copy(tmp_path)
    f = d / "strih-pp.env"
    body = f.read_text()
    body = body.replace("STRIH_HTTPS_INTERKOM_HOSTS=interkom-pp.newlevel.media\n", "STRIH_HTTPS_INTERKOM_HOSTS=none\n")
    body = body.replace("STRIH_HTTPS_SHADING_HOST=shadingpp.newlevel.media\n", "STRIH_HTTPS_SHADING_HOST=none\n")
    # issue 1453: a box with no intercom site publishes nothing (the loader refuses STRIH_HTTPS_PUBLIC=yes).
    body = body.replace("STRIH_HTTPS_PUBLIC=yes\n", "STRIH_HTTPS_PUBLIC=no\n")
    f.write_text(body)
    rows = _rows(rig, box="strih-pp", STRIH_BOXES_DIR=str(d))
    assert len(rows) == 1 and rows[0][0] == "NOTE", rows


def test_the_report_prints_through_the_callers_functions(tmp_path):
    rig = Rig(tmp_path)
    _healthy(rig)
    r = _bash('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }\n'
              'strih_box_load "$1" || exit 9\nstrih_https_grade_report; echo "RC=$?"', "strih-snv", env=rig.env())
    assert r.returncode == 0, r.stderr
    assert r.stdout.count("PASS ") == 8 and r.stdout.strip().endswith("RC=0"), r.stdout


# --- the role gate + wiring -------------------------------------------------------------------------------

@pytest.mark.parametrize("box,runs", [("strih-snv", True), ("strih-pp", True), ("stream-pp", False)])
def test_step_16i_is_the_strih_role_only(box, runs):
    r = _bash('strih_box_load "$1" || exit 9\nif WHY="$(obs_box_step_applies 16i)"; then echo RUN; '
              'else echo "SKIP $WHY"; fi', box, sources=(FACTS, ROLE))
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("RUN" if runs else "SKIP"), r.stdout


def test_setup_and_verify_wire_the_step_and_the_item():
    setup = SETUP.read_text()
    verify = VERIFY.read_text()
    assert re.search(r'(?m)^step "16i" ', setup)
    i = setup.index('step "16i"')
    body = setup[i:setup.index("\nstep ", i + 1)]
    assert "obs_box_step_applies 16i" in body and "strih_https_install" in body
    assert setup.index('step "16h"') < i < setup.index("step 17 ")
    assert "# 45) issue 1399" in verify
    j = verify.index("# 45) issue 1399")
    item = verify[j:verify.index("\n# 16) NIC", j)]
    assert "obs_box_step_applies 16i" in item and "strih_https_grade_report" in item
    assert verify.index("# 43) issue 1418") < j
    assert len(setup.splitlines()) <= 1000 and len(verify.splitlines()) <= 1000


def test_the_provision_lib_sources_the_https_lib():
    text = PROVISION.read_text()
    assert re.search(r'(?m)^  \. "\$\(dirname "\$\{BASH_SOURCE\[0\]\}"\)/strih-https\.sh"$', text)


def test_the_lib_never_reads_the_token():
    """setup never handles or prints the Cloudflare token: no cat / read / source of the ini."""
    text = LIB.read_text()
    code = "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))
    for bad in ("cat \"$ini\"", "< \"$ini\"", ". \"$ini\"", "shading_https_cf_ini_content",
                "cloudflare-newlevel", ".secrets"):
        assert bad not in code, bad
