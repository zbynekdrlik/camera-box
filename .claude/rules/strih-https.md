---
paths:
  - "scripts/lib/strih-https.sh"
  - "scripts/lib/shading-https.sh"
  - "scripts/dev1-shading-https-install.sh"
  - "scripts/nginx/**"
  - "scripts/strih-boxes/*.env"
  - "tests/python/test_strih_https_1399.py"
  - "tests/python/test_shading_https_install_808.py"
---

# The strih box serves its OWN intercom + shading HTTPS front (issue 1399)

Owner, 1.10.2026, during an outdoor production, when `interkom.newlevel.media` did not load at the
venue: "MA TO BYT SAMOBEZIACE NA STRIH NB NEMA TO BYT ZAVISLE NA INOM PC AKO DEV1". The names were A
records to dev1, whose nginx proxied to strih-snv. dev1 stays home and strih travels, so the phones
had no route. The hot fix copied dev1's sites and Let's Encrypt certificates to strih-snv BY HAND.
Nothing renewed them (first expiry 16.12.2026), and setup-strih did not provision them. Design
6142127441 (Approach 1) makes it provisioning.

## The facts (`scripts/strih-boxes/<box>.env`, validated by `scripts/lib/strih-box-facts.sh`)

| Box | `STRIH_HTTPS_INTERKOM_HOSTS` (first name keys the cert, the rest are SANs) | `STRIH_HTTPS_SHADING_HOST` |
|---|---|---|
| strih-snv | `interkom-lx.newlevel.media interkom.newlevel.media interkom-snv.newlevel.media` | `shading.newlevel.media` |
| strih-pp | `interkom-pp.newlevel.media` (ROZHODNUTE on issue 1412, 13.10.2026: off VDO.Ninja) | `shadingpp.newlevel.media` |
| stream-pp | `none` | `none` |

- The loader refuses: a name that is not a lower-case DNS name of two or more labels (the character
  sets are spelled out, never a bracket range, issue 1302), a `none` mixed into a list, two shading
  names, one name declared twice across the two facts, and any name on the stream role.
- Moving a name between boxes is one edit in each fact file plus the supervisor's DNS move. DNS
  is NEVER provisioning: the Cloudflare A records are the supervisor's own channel.

## The lib (`scripts/lib/strih-https.sh`, sourced by strih-provision.sh)

It reuses the dev1 renderers of `scripts/lib/shading-https.sh`, which take optional per-box arguments
for that. Their default output is byte-identical, pinned by the issue-808 tests against the committed
`scripts/nginx/*.conf`.
- `shading_https_site_content` takes `$7` cert path, `$8` key path and `$9` header.
- `shading_https_deploy_hook_content` takes what, by whom and the issue.

**The sites** (`strih_https_sites`) have LOCAL upstreams.
- interkom: `/` goes to the hub at `127.0.0.1:8790` (the box's intercom matrix `[http]` bind).
  `location = /janus` goes to `<STRIH_IP>:8188`, because Janus binds `ws_ip` = the box IP and never
  localhost.
- shading: `/` goes to bkshading at `127.0.0.1:8770` (its `default_bind`).
- Both ports are test-pinned to their sources. The SNV render keeps the live hot-fix routes.
- The files are `/etc/nginx/sites-available/{interkom,shading}`, the hot-fix names, so the hand sites
  are replaced in place. Each is enabled by a `../sites-available/<site>` symlink.

**setup-strih step 16i** (`strih_https_install`, strih role via `obs_box_step_applies 16i`):
1. It installs `nginx-light certbot python3-certbot-dns-cloudflare` with `--no-upgrade`, only those
   missing (an installed `nginx` satisfies nginx-light). nginx-light exists on 26.04: the 1.10.2026
   hot fix installed it on strih-snv. On 24.04 it is a small wrapper over `nginx`.
2. **The token is never handled.** setup never reads, prints or writes `/etc/letsencrypt/cloudflare.ini`.
   - A present ini gets mode 600, and as root owner root:root.
   - A missing ini is ONE `WARN` line naming the supervisor step. Issuing and renewing wait for it;
     nothing fails.
   - A test pins that the lib code holds no read of the ini.
3. It writes the renewal deploy hook `/etc/letsencrypt/renewal-hooks/deploy/nginx-reload.sh` (0755,
   `systemctl reload nginx`).
4. **The certificates.** Per site, `certbot certonly --dns-cloudflare ... --cert-name <first name>`
   runs ONLY when `/etc/letsencrypt/live/<first>/` is missing or its SAN set differs
   (`strih_https_cert_action`: issue / reissue / current).
   - `--cert-name` is always passed: the dev1 argv only adds it with aliases.
   - certbot gets `</dev/null` inside the site loop.
   - A failing certbot is a WARN, never a failed step. The site keeps the cert it has, every strih
     genlock deploy re-runs setup-strih, and item 45 grades the truth.
   - **A successful certbot run always tests and reloads nginx**, also when the site text is
     unchanged. A `certonly` fires no renewal deploy hook (those run on `certbot renew` only). A site
     written by an earlier run whose certbot failed already names the live dir, so without the
     reload nginx kept serving the old certificate (review finding).
5. **The site's cert pair.** It is the live cert when one exists. Otherwise it is the HAND COPY: the
   `ssl_certificate` / `ssl_certificate_key` pair the installed site names (else an enabled site
   declaring the first name), and only when both RESOLVE (`realpath -e`: no `..`, no symlink out)
   to files under `/etc/nginx/ssl/`. So a box keeps serving during the move. With neither, the site
   is not written (WARN).
6. **Write and reload.** Each site is written only on a difference, through a sibling temp + `mv -Tf`
   (atomic; a temp in sites-available is never loaded). The old file and symlink are kept aside, and
   `nginx -t` runs BEFORE any reload or start. If it fails, every changed file is restored
   and nothing is reloaded (rc 1). Then nginx is enabled and reloaded, or started, and
   `certbot.timer` is enabled.
7. Another enabled site that declares one of the names (a hand site under another name, e.g. on
   strih-pp) is named in a WARN, never removed.

**verify-strih item 45** (`strih_https_grade_report`):

| Row | Grades |
|---|---|
| `(https-nginx)` | nginx active, :443 listening (`ss -ltnH`), every declared site equal to its render (same cert-pair choice) and enabled |
| `(https-cert) <name>` | one per declared name: the cert nginx serves for that SNI on 127.0.0.1:443 (`openssl s_client -servername`) covers the name (exact or a one-label wildcard), is valid for more than 14 days (`-checkend`), and IS the `/etc/letsencrypt/live` one (its sha256 fingerprint = that of the live fullchain's first certificate; a mismatch = nginx was not reloaded); a hand copy is a NOTE until migrated |
| `(https-upstream) interkom` | through nginx: the hub `/api/state` 200 and the `/janus` WebSocket upgrade 101 (read from curl's `-D -` status line: after a 101 curl holds the connection until `--max-time`) |
| `(https-upstream) shading` | bkshading `/api/version` 200 |
| `(https-renewal)` | `certbot.timer` enabled, the deploy hook equal to its render and executable, the ini present with mode 600 owner root (stat only) |

A box that declares no site gets ONE NOTE. The stream role is not graded (a NOTE through the step gate).
Run item 45 as root: certbot creates `/etc/letsencrypt/live` mode 0700. A non-root run names the
unreadable dir in `(https-nginx)` rather than reading it as a missing certificate. The grader's
scratch files live in a private `mktemp -d` (verify runs as root; never a guessable `/tmp` name).

## The supervisor live runbook

It is the issue-1399 comment that follows the LANE-RETURN of this slice. In short:
1. Place the zone-scoped DNS token on each strih box: `python3 ~/devel/airuleset/airuleset.py secret
   exec` piped over ssh stdin into `/etc/letsencrypt/cloudflare.ini` (0600 root). It never goes into
   argv, a log or chat.
2. Run step 16i (`setup-strih.sh --box <box>`, or the next genlock deploy).
3. Read the item 45 rows.
4. Move `interkom-pp`'s DNS to 10.77.8.202.
5. Remove dev1's now-unused `interkom` + `shading` sites once both boxes serve from their live certs.

## Tests (Tier-0)

`tests/python/test_strih_https_1399.py` runs the real lib under `set -euo pipefail` with fake
`apt-get` / `dpkg-query` / `systemctl` / `nginx` / `certbot` / `openssl` / `curl` / `ss` on PATH.
- Seams: `STRIH_HTTPS_NGINX_DIR`, `STRIH_HTTPS_LE_DIR`, `STRIH_HTTPS_INI_OWNER`.
- The fakes are python source built from non-raw test strings, so a CR/LF in their output must be
  written `\\r\\n` in the test file. A single backslash puts a real line break inside a string
  literal, the fake dies, and every probe reads 000.
