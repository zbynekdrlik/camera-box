//! Issue 1363 (option C) — on LINUX, every DistroAV NDI sender closes its connected sockets with
//! RST (`SO_LINGER {1,0}`) right before it is destroyed, so no TIME_WAIT survives on its port and
//! the next OBS keeps the whole captured sender port map (2ME PGM on :5961 …).
//!
//! Root cause (measured on strih-lx + dev1, libndi 6.3.2): libndi binds each sender listener
//! WITHOUT `SO_REUSEADDR`. On Linux a connection left in TIME_WAIT on :5961 by the previous OBS
//! (60 s, fixed) makes the next OBS's first sender bind fail, and libndi's in-process port cursor
//! never retries a port it failed to bind — so the reserved program sender (the port-1185 pin)
//! lands on :5962 for the whole session and every sender shifts up by one. Windows binds over a
//! TIME_WAIT, which is why the pin always held there. The chosen fix (ROZHODNUTÉ on the ticket):
//! prevent the TIME_WAIT at stop time (linger-0 on the sender's CONNECTED sockets), and at the
//! :5961 reserve, if a TIME_WAIT still holds it (crash/kill), log one loud `WARN-1363` line and
//! continue — never wait on the loading thread. Windows compiles byte-identical code.
//!
//! libndi has no API for a sender's port (`send_get_source_name()->p_url_address` is NULL for a
//! local sender, measured), so the new `vendor/distroav/src/ndi-sender-port.cpp` learns it as the
//! single new TCP LISTENING socket in `/proc/self/fd` across the (serialized) `send_create`.
//!
//! Std-only, runs offline (the #1026 `rustc --test` recipe) — three facets:
//! - Facet A: source anchors (revert protection against a `git subtree pull`), incl. a structural
//!   "every sender destroy aborts first / every sender create is tracked" sweep and a
//!   "every #1363 line is Linux-only" preprocessor-region check (Windows byte-identical).
//! - Facet B: lift the pure `static inline` decision helpers VERBATIM, compile them under
//!   `-Werror -Wconversion -Wformat=2`, run a hand-written truth table (the table IS the spec).
//! - Facet C: compile the REAL `ndi-sender-port.cpp` with g++ against a stub `plugin-main.h`
//!   (a fake libndi whose `send_create` opens a real 0.0.0.0 listener) and prove on real loopback
//!   sockets: the tracked create finds the port; a live listener reads LIVE; without the abort the
//!   server-first close leaves a TIME_WAIT (the bug); with the abort the port is FREE again.
//!
//! The compilers are REQUIRED: a missing cc/g++ FAILS the gate, never skips (test strictness).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const PORT_H: &str = "vendor/distroav/src/ndi-sender-port.h";
const PORT_CPP: &str = "vendor/distroav/src/ndi-sender-port.cpp";
const NDI_OUTPUT: &str = "vendor/distroav/src/ndi-output.cpp";
const NDI_FILTER: &str = "vendor/distroav/src/ndi-filter.cpp";
const CMAKELISTS: &str = "vendor/distroav/CMakeLists.txt";
const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";
const WINDOWS_GENLOCK_FAST_WF: &str = ".github/workflows/windows-genlock-fast.yml";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}) — issue 1363: the Linux NDI sender-port linger module is gone \
             or was never applied",
            p.display()
        )
    })
}

/// Collapse every run of ASCII whitespace to one space so the anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Byte offsets of every REAL call `needle` in `text` — an occurrence immediately preceded by
/// `+`/`-` is inside a DistroAV debug log string (`"ndi_output_stop: +ndiLib->send_destroy(...)"`)
/// and is skipped.
fn real_calls(text: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find(needle) {
        let at = from + i;
        let prev = text[..at].chars().last();
        if prev != Some('+') && prev != Some('-') {
            out.push(at);
        }
        from = at + needle.len();
    }
    out
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors
// ----------------------------------------------------------------------------------------------

#[test]
fn sender_port_module_present_and_linux_only() {
    let h = squish(&vendor_file(PORT_H));
    for sig in [
        "#define NDI_SENDER_FIRST_TCP_PORT 5961",
        "#define NDI_MESSAGING_TCP_PORT 5960",
        "enum ndi_first_port_state {",
        "static inline int ndi_first_port_hold_state(",
        "static inline const char *ndi_first_port_hold_state_text(",
        "static inline int ndi_reserve_should_warn(",
        "static inline int ndi_socket_is_sender_connection(",
        "static inline int ndi_new_listen_port(",
        "NDIlib_send_instance_t ndi_sender_create_tracked(const NDIlib_send_create_t *desc, int *out_port);",
        "void ndi_sender_abort_connections_before_destroy(int port, const char *name);",
        "int ndi_sender_port_hold_state(int port);",
    ] {
        assert!(
            h.contains(sig),
            "{PORT_H}: issue 1363 — `{sig}` is missing; the Linux sender-port linger module was \
             reverted or renamed."
        );
    }

    let raw = vendor_file(PORT_CPP);
    let directives: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('#'))
        .collect();
    assert_eq!(
        directives.first().copied(),
        Some("#ifdef __linux__"),
        "{PORT_CPP}: issue 1363 — the whole translation unit must be wrapped in `#ifdef __linux__` \
         (the TIME_WAIT bind block is Linux kernel behaviour; Windows must stay byte-identical)"
    );
    assert!(
        directives.last().is_some_and(|d| d.starts_with("#endif")),
        "{PORT_CPP}: issue 1363 — the `#ifdef __linux__` wrapper must close at the end of the file"
    );

    let cpp = squish(&raw);
    for tok in [
        "\"/proc/self/fd\"",
        "SO_ACCEPTCONN",
        "SO_LINGER",
        "lg.l_onoff = 1;",
        "lg.l_linger = 0;",
        "std::lock_guard<std::mutex> lock(g_ndi_sender_create_mutex);",
        "ndi_new_listen_port(before.data(), before.size(), after.data(), after.size())",
        "ndi_socket_is_sender_connection(is_listener, has_peer, local_port, port)",
        "0x7f000002u",
        "ndi_first_port_hold_state(plain == 0, plain == EADDRINUSE, alias == 0, alias == EADDRINUSE)",
    ] {
        assert!(
            cpp.contains(tok),
            "{PORT_CPP}: issue 1363 — `{tok}` is missing from the Linux sender-port module."
        );
    }

    let cm = squish(&vendor_file(CMAKELISTS));
    assert!(
        cm.contains(
            "if(CMAKE_SYSTEM_NAME STREQUAL \"Linux\") target_sources( ${CMAKE_PROJECT_NAME} \
             PRIVATE src/ndi-sender-port.cpp src/ndi-sender-port.h )"
        ),
        "{CMAKELISTS}: issue 1363 — the Linux-only `target_sources` block for \
         src/ndi-sender-port.cpp is missing (the Windows source list must stay unchanged)"
    );
}

#[test]
fn every_sender_destroy_aborts_its_connections_first_on_linux() {
    // Two in ndi-output.cpp's program/preview path + the unadopted reservation (3), two in the
    // per-source republish filter (destroy + destroy-then-recreate). A NEW destroy site without
    // the abort fails here.
    for (file, want) in [(NDI_OUTPUT, 3usize), (NDI_FILTER, 2usize)] {
        let src = squish(&vendor_file(file));
        let calls = real_calls(&src, "ndiLib->send_destroy(");
        assert_eq!(
            calls.len(),
            want,
            "{file}: issue 1363 — expected {want} real `ndiLib->send_destroy(` sites, found {}; a \
             sender destroy was added or removed — every one must abort its connections first",
            calls.len()
        );
        for at in calls {
            let lo = at.saturating_sub(320);
            let window = &src[lo..at];
            let abort = window
                .rfind("#ifdef __linux__ ndi_sender_abort_connections_before_destroy(")
                .unwrap_or_else(|| {
                    panic!(
                        "{file}: issue 1363 — a `send_destroy` is not preceded by the Linux-only \
                         `ndi_sender_abort_connections_before_destroy(` call, so its connections \
                         close with FIN and leave a TIME_WAIT that shifts the next OBS's port:\n…{window}"
                    )
                });
            assert!(
                !window[abort..].contains("send_destroy("),
                "{file}: issue 1363 — the abort call belongs to a DIFFERENT destroy site:\n…{window}"
            );
            assert!(
                window[abort..].contains("#endif"),
                "{file}: issue 1363 — the abort call must sit in its own `#ifdef __linux__ … #endif` \
                 block before the destroy:\n…{window}"
            );
        }
    }
}

#[test]
fn every_sender_create_is_port_tracked_on_linux() {
    for (file, want) in [(NDI_OUTPUT, 2usize), (NDI_FILTER, 1usize)] {
        let src = squish(&vendor_file(file));
        let calls = real_calls(&src, "ndiLib->send_create(");
        assert_eq!(
            calls.len(),
            want,
            "{file}: issue 1363 — expected {want} real `ndiLib->send_create(` sites (each the \
             Windows `#else` twin of a tracked create), found {}",
            calls.len()
        );
        for at in calls {
            let window = &src[at.saturating_sub(400)..at];
            let tracked = window
                .rfind("= ndi_sender_create_tracked(&send_desc, &")
                .unwrap_or_else(|| {
                    panic!(
                    "{file}: issue 1363 — a `send_create` has no Linux `ndi_sender_create_tracked` \
                     twin, so the sender's port is never learned and its connections can never be \
                     aborted at stop:\n…{window}"
                )
                });
            let tail = &window[tracked..];
            assert!(
                tail.contains("#else") && window[..tracked].contains("#ifdef __linux__"),
                "{file}: issue 1363 — the tracked create must be the `#ifdef __linux__` branch and \
                 the plain `send_create` its `#else` (Windows byte-identical):\n…{window}"
            );
        }
    }
}

#[test]
fn reserve_warns_once_when_the_first_port_is_held() {
    let src = squish(&vendor_file(NDI_OUTPUT));
    let probe = src
        .find("const int first_port_state = ndi_sender_port_hold_state(NDI_SENDER_FIRST_TCP_PORT);")
        .expect(
            "issue 1363: ndi_output_reserve_main_sender no longer probes :5961 before the reserve",
        );
    let create = src
        .find("g_reserved_main_sender = ndi_sender_create_tracked(&send_desc, &g_reserved_main_port);")
        .expect("issue 1363: the :5961 reserve no longer records its port (tracked create)");
    assert!(
        probe < create,
        "issue 1363: the :5961 hold probe must run BEFORE the reserving send_create (after it, \
         the reserved sender itself holds :5961 and the probe reads LIVE)"
    );
    assert!(
        src.contains("if (ndi_reserve_should_warn(first_port_state, g_reserved_main_port))"),
        "issue 1363: the reserve no longer decides the WARN-1363 line via ndi_reserve_should_warn"
    );
    assert!(
        src.contains("\"WARN-1363 - ndi_output_reserve_main_sender:"),
        "issue 1363: the loud one-line WARN-1363 at the :5961 reserve is gone"
    );
    // The adopted (reserved) instance must bring its port along, or the program's stop cannot
    // abort its connections — the program is the one sender that matters most.
    assert!(
        src.contains("o->ndi_sender_port = ndi_output_take_adopted_port();"),
        "issue 1363: ndi_output_start no longer takes the adopted reservation's port"
    );
    // No waiting on the loading thread (option B was rejected on the ticket).
    let reserve_fn = &src[src
        .find("void ndi_output_reserve_main_sender(")
        .expect("reserve fn")..create];
    for banned in ["sleep", "usleep", "nanosleep", "this_thread"] {
        assert!(
            !reserve_fn.contains(banned),
            "issue 1363: the reserve path must NEVER wait for :5961 (it would delay OBS start in \
             the crash-recovery moment) — found `{banned}`"
        );
    }
}

/// Strip a `//` line comment (DistroAV's C++ files have no `//` inside string literals on the
/// lines this check inspects).
fn code_part(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

#[test]
fn every_1363_line_is_linux_only_so_windows_stays_byte_identical() {
    const TOKENS: [&str; 9] = [
        "ndi-sender-port.h",
        "ndi_sender_create_tracked",
        "ndi_sender_abort_connections_before_destroy",
        "ndi_sender_port_hold_state",
        "ndi_sender_port",
        "g_reserved_main_port",
        "g_taken_main_port",
        "ndi_output_take_adopted_port",
        "WARN-1363",
    ];
    for file in [NDI_OUTPUT, NDI_FILTER] {
        let text = vendor_file(file);
        // Stack of (is_linux_ifdef, in_else_branch).
        let mut stack: Vec<(bool, bool)> = Vec::new();
        let mut hits = 0usize;
        for (n, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("#if") {
                stack.push((
                    t == "#ifdef __linux__" || t == "#if defined(__linux__)",
                    false,
                ));
                continue;
            }
            if t.starts_with("#else") || t.starts_with("#elif") {
                if let Some(top) = stack.last_mut() {
                    top.1 = true;
                }
                continue;
            }
            if t.starts_with("#endif") {
                stack.pop();
                continue;
            }
            let code = code_part(line);
            if !TOKENS.iter().any(|tok| code.contains(tok)) {
                continue;
            }
            hits += 1;
            let linux_only = stack
                .iter()
                .any(|(is_linux, in_else)| *is_linux && !*in_else);
            assert!(
                linux_only,
                "{file}:{}: issue 1363 — this line is compiled on WINDOWS too, but the fix must \
                 be Linux-only (`#ifdef __linux__`) so Windows stays byte-identical:\n{line}",
                n + 1
            );
        }
        assert!(
            hits > 0,
            "{file}: issue 1363 — no #1363 wiring found at all (checked tokens {TOKENS:?})"
        );
    }
}

#[test]
fn windows_genlock_workflows_gate_on_the_1363_linger_patch() {
    for wf in [WINDOWS_GENLOCK_WF, WINDOWS_GENLOCK_FAST_WF] {
        let w = squish(&vendor_file(wf));
        for tok in [
            "#ifdef __linux__ ndi_sender_abort_connections_before_destroy(o->ndi_sender_port, name);",
            "#ifdef __linux__ ndi_sender_abort_connections_before_destroy(filter->ndi_sender_port,",
            "g_reserved_main_sender = ndi_sender_create_tracked(&send_desc, &g_reserved_main_port);",
        ] {
            assert!(
                w.contains(tok),
                "{wf}: issue 1363 — the build no longer asserts the `{tok}` SOURCE token; a subtree \
                 bump could drop the Linux linger fix while the version pin still passes. Re-add the \
                 pwsh source-patch gate (lock-step with this Rust guard)."
            );
        }
        // Presence alone would pass on ONE surviving site; the gates must COUNT both sites.
        for tok in [
            "#ifdef __linux__ ndi_sender_abort_connections_before_destroy(o->ndi_sender_port, name);'))).Count -ne 2",
            "#ifdef __linux__ ndi_sender_abort_connections_before_destroy(filter->ndi_sender_port,'))).Count -ne 2",
        ] {
            assert!(
                w.contains(tok),
                "{wf}: issue 1363 — the pwsh gate must COUNT both call sites of the abort token \
                 (`[regex]::Matches(...).Count -ne 2`), not merely check presence: `{tok}`"
            );
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure helpers VERBATIM, compile standalone, run the truth table
// ----------------------------------------------------------------------------------------------

/// Lift from `anchor` to the first `end` marker (inclusive) — never retype the shipped bytes.
fn lift(src: &str, anchor: &str, end: &str) -> String {
    let start = src.find(anchor).unwrap_or_else(|| {
        panic!("issue 1363: `{anchor}` not found in {PORT_H} — nothing to lift")
    });
    let stop = src[start..]
        .find(end)
        .map(|i| start + i + end.len())
        .unwrap_or_else(|| panic!("issue 1363: `{anchor}` has no terminating `{end:?}`"));
    src[start..stop].to_string()
}

fn lifted_pure_helpers() -> String {
    let h = vendor_file(PORT_H);
    let mut c = String::new();
    c.push_str(&lift(&h, "#define NDI_SENDER_FIRST_TCP_PORT", "\n"));
    c.push_str(&lift(&h, "#define NDI_MESSAGING_TCP_PORT", "\n"));
    c.push_str(&lift(&h, "enum ndi_first_port_state {", "\n};\n"));
    for sig in [
        "static inline int ndi_first_port_hold_state(",
        "static inline const char *ndi_first_port_hold_state_text(",
        "static inline int ndi_reserve_should_warn(",
        "static inline int ndi_socket_is_sender_connection(",
        "static inline int ndi_new_listen_port(",
    ] {
        c.push_str(&lift(&h, sig, "\n}\n"));
    }
    c
}

/// A scratch directory removed when dropped — on success AND when an assertion panics.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for Scratch {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "distroav_sender_port_linger_1363-{}-{name}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    Scratch(dir)
}

fn compile_and_run(compiler: &str, args: &[&str], sources: &[PathBuf], bin: &PathBuf) -> String {
    let out = Command::new(compiler)
        .args(args)
        .args(sources)
        .arg("-o")
        .arg(bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1363: could not run the compiler `{compiler}` ({e}). This gate compiles the \
                 vendored Linux sender-port code; it must FAIL rather than skip without a toolchain."
            )
        });
    assert!(
        out.status.success(),
        "issue 1363: the lifted/vendored sender-port code does NOT COMPILE standalone under \
         {args:?} — very likely a real compile error heading for the Linux genlock CI:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(bin)
        .output()
        .expect("issue 1363: the compiled harness failed to execute");
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    assert!(
        run.status.success(),
        "issue 1363: the harness exited non-zero:\n{stdout}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    stdout
}

/// One truth-table row: four C int arguments and the expected result.
type FourIntVector = ((i32, i32, i32, i32), i32);

#[test]
fn pure_helpers_compute_the_spec_truth_table() {
    // (plain_ok, plain_in_use, alias_ok, alias_in_use) -> state
    // 0 FREE, 1 TIME_WAIT (only closing connections hold it), 2 LIVE_LISTENER, 3 UNKNOWN
    let hold: &[FourIntVector] = &[
        ((1, 0, 0, 0), 0), // plain bind works: free
        ((1, 1, 1, 1), 0), // plain ok wins regardless of the (unused) alias args
        ((0, 1, 1, 0), 1), // in use on the wildcard, loopback alias binds: TIME_WAIT only
        ((0, 1, 0, 1), 2), // alias also in use: a live 0.0.0.0 listener
        ((0, 0, 1, 0), 3), // plain failed for another reason (EACCES…): unknown
        ((0, 1, 0, 0), 3), // alias failed for another reason (no 127.0.0.2): unknown
    ];
    // (state, landed_port) -> warn
    let warn: &[((i32, i32), i32)] = &[
        ((0, 5961), 0), // free + pinned: silent
        ((1, 5961), 0), // TIME_WAIT expired in between, pin held: silent
        ((2, 5961), 0),
        ((0, 5962), 1), // landed elsewhere: always loud
        ((1, 5962), 1), // the crash/kill case: loud
        ((2, 5963), 1),
        ((0, 0), 0), // port unknown but probe said free: the create already warned
        ((1, 0), 1), // port unknown and :5961 was held: loud
        ((2, 0), 1),
        ((3, 0), 1),
    ];
    // (is_listener, has_peer, local_port, sender_port) -> abort this socket
    let conn: &[FourIntVector] = &[
        ((0, 1, 5961, 5961), 1),  // an accepted connection on the sender's port
        ((1, 0, 5961, 5961), 0),  // the listener itself (no TIME_WAIT on a listener)
        ((0, 0, 5961, 5961), 0),  // bound, not connected
        ((0, 1, 5962, 5961), 0),  // another sender's connection
        ((0, 1, 0, 0), 0),        // sender port unknown: touch nothing
        ((0, 1, 45000, 5961), 0), // the client side of an in-process receiver
    ];
    // (before, after) -> the sender's new listening port (0 = none / ambiguous)
    let diff: &[(&[i32], &[i32], i32)] = &[
        (&[5960], &[5960, 5961], 5961),
        (&[5960, 5961], &[5960, 5961, 5962, 5962], 5962), // same port on v4 + v6
        (&[5960], &[5960, 5961, 5962], 0),                // two new listeners: ambiguous
        (&[5960, 5961], &[5960, 5961], 0),                // nothing new
        (&[], &[5961], 5961),
        // The FIRST send_create of a process (the :5961 program reservation) also opens
        // libndi's :5960 messaging listener — measured live on dev1 with libndi 6.3.2. The
        // messaging port is never a sender port, so the program's port is still identified.
        (&[], &[5960, 5961], 5961),
        (&[], &[5960, 5963], 5963),
        (&[5960], &[0, 5960, 5963], 5963), // a 0 entry is ignored
        (&[5960, 5961], &[5960, 5962], 5962), // a listener that vanished does not matter
    ];

    let mut c = String::from("#include <stddef.h>\n#include <stdio.h>\n");
    c.push_str(&lifted_pure_helpers());
    c.push_str("\nint main(void){\n");
    for ((a, b, x, y), _) in hold {
        c.push_str(&format!(
            "  printf(\"%d\\n\", ndi_first_port_hold_state({a}, {b}, {x}, {y}));\n"
        ));
    }
    for ((s, p), _) in warn {
        c.push_str(&format!(
            "  printf(\"%d\\n\", ndi_reserve_should_warn({s}, {p}));\n"
        ));
    }
    for ((l, peer, lp, sp), _) in conn {
        c.push_str(&format!(
            "  printf(\"%d\\n\", ndi_socket_is_sender_connection({l}, {peer}, {lp}, {sp}));\n"
        ));
    }
    for (k, (before, after, _)) in diff.iter().enumerate() {
        let arr = |v: &[i32]| {
            if v.is_empty() {
                "{0}".to_string()
            } else {
                format!(
                    "{{{}}}",
                    v.iter()
                        .map(|x| x.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
        };
        c.push_str(&format!(
            "  {{ const int b{k}[] = {}; const int a{k}[] = {}; \
             printf(\"%d\\n\", ndi_new_listen_port(b{k}, {}, a{k}, {})); }}\n",
            arr(before),
            arr(after),
            before.len(),
            after.len()
        ));
    }
    // The text helper: every state maps to a distinct non-empty text; out-of-range is handled.
    c.push_str(
        "  for (int s = 0; s < 5; s++) printf(\"T%d=%s\\n\", s, ndi_first_port_hold_state_text(s));\n",
    );
    c.push_str("  printf(\"TN=%s\\n\", ndi_first_port_hold_state_text(-1));\n  return 0;\n}\n");

    let dir = scratch("pure");
    let cfile = dir.join("pure.c");
    fs::write(&cfile, &c).expect("write the pure harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let stdout = compile_and_run(
        &cc,
        &[
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Wconversion",
            "-Werror",
            "-O1",
        ],
        &[cfile],
        &dir.join("pure.bin"),
    );

    let (nums, texts): (Vec<&str>, Vec<&str>) = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .partition(|l| !l.starts_with('T'));
    let want: Vec<i32> = hold
        .iter()
        .map(|(_, w)| *w)
        .chain(warn.iter().map(|(_, w)| *w))
        .chain(conn.iter().map(|(_, w)| *w))
        .chain(diff.iter().map(|(_, _, w)| *w))
        .collect();
    let got: Vec<i32> = nums
        .iter()
        .map(|l| l.trim().parse().expect("harness printed a non-integer"))
        .collect();
    assert_eq!(
        got, want,
        "issue 1363: the vendored pure sender-port helpers DIVERGED from the spec truth table \
         (order: hold_state, should_warn, is_sender_connection, new_listen_port)\n--- harness ---\n{c}"
    );

    let t: Vec<&str> = texts.iter().map(|l| l.split_once('=').unwrap().1).collect();
    assert_eq!(t.len(), 6, "issue 1363: text helper lines: {texts:?}");
    for (i, a) in t.iter().enumerate().take(4) {
        assert!(
            !a.is_empty(),
            "issue 1363: state {i} has an empty description"
        );
        for b in t.iter().take(4).skip(i + 1) {
            assert_ne!(
                a, b,
                "issue 1363: two first-port states share one description"
            );
        }
    }
    assert!(
        t[1].contains("TIME_WAIT") && t[2].contains("listener"),
        "issue 1363: the WARN-1363 text must name the cause (TIME_WAIT / live listener): {t:?}"
    );
    assert_eq!(
        t[4], t[5],
        "issue 1363: out-of-range states must share the fallback text"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet C — the REAL impure module on real loopback sockets (fake libndi, no NDI SDK needed)
// ----------------------------------------------------------------------------------------------

const STUB_PLUGIN_MAIN: &str = r#"// issue 1363 gate stub: only what ndi-sender-port.cpp uses from the real plugin-main.h.
#pragma once
#include <cstdarg>
#include <cstdio>
#define LOG_ERROR 100
#define LOG_WARNING 200
#define LOG_INFO 300
#define LOG_DEBUG 400
typedef struct NDIlib_send_instance_type *NDIlib_send_instance_t;
typedef struct {
	const char *p_ndi_name;
	const char *p_groups;
	bool clock_video;
	bool clock_audio;
} NDIlib_send_create_t;
typedef struct {
	NDIlib_send_instance_t (*send_create)(const NDIlib_send_create_t *p_create_settings);
	void (*send_destroy)(NDIlib_send_instance_t p_instance);
} NDIlib_v6;
extern const NDIlib_v6 *ndiLib;
static inline void obs_log(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static inline void obs_log(int level, const char *fmt, ...)
{
	va_list ap;
	va_start(ap, fmt);
	printf("LOG%d: ", level);
	vprintf(fmt, ap);
	printf("\n");
	va_end(ap);
}
"#;

const HARNESS: &str = r#"// issue 1363 gate: the real ndi-sender-port.cpp over real loopback sockets.
#include "plugin-main.h"
#include "ndi-sender-port.h"
#include <arpa/inet.h>
#include <cstdio>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

struct fake_sender {
	int listen_fd;
	int conn_fd;
};

// Like libndi: a 0.0.0.0 listener WITHOUT SO_REUSEADDR (here on an ephemeral port).
static NDIlib_send_instance_t fake_send_create(const NDIlib_send_create_t *)
{
	int fd = socket(AF_INET, SOCK_STREAM, 0);
	if (fd < 0)
		return nullptr;
	sockaddr_in a{};
	a.sin_family = AF_INET;
	a.sin_port = 0;
	a.sin_addr.s_addr = htonl(INADDR_ANY);
	if (bind(fd, (sockaddr *)&a, sizeof a) != 0 || listen(fd, 4) != 0) {
		close(fd);
		return nullptr;
	}
	return (NDIlib_send_instance_t) new fake_sender{fd, -1};
}

static void fake_send_destroy(NDIlib_send_instance_t p)
{
	auto *s = (fake_sender *)p;
	if (!s)
		return;
	if (s->conn_fd >= 0)
		close(s->conn_fd);
	close(s->listen_fd);
	delete s;
}

static const NDIlib_v6 fake_lib = {fake_send_create, fake_send_destroy};
const NDIlib_v6 *ndiLib = &fake_lib;

static int scenario(const char *label, bool abort_first)
{
	NDIlib_send_create_t desc{};
	desc.p_ndi_name = label;
	int port = -1;
	NDIlib_send_instance_t s = ndi_sender_create_tracked(&desc, &port);
	if (!s || port <= 0) {
		printf("%s.port_found=0\n", label);
		return 1;
	}
	printf("%s.port_found=1\n", label);
	auto *fs = (fake_sender *)s;
	int c = socket(AF_INET, SOCK_STREAM, 0);
	sockaddr_in a{};
	a.sin_family = AF_INET;
	a.sin_port = htons((uint16_t)port);
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	if (c < 0 || connect(c, (sockaddr *)&a, sizeof a) != 0)
		return 2;
	fs->conn_fd = accept(fs->listen_fd, nullptr, nullptr);
	if (fs->conn_fd < 0)
		return 3;
	char b = 'x';
	if (write(c, &b, 1) != 1 || read(fs->conn_fd, &b, 1) != 1)
		return 4;
	printf("%s.live_state=%d\n", label, ndi_sender_port_hold_state(port));
	if (abort_first)
		ndi_sender_abort_connections_before_destroy(port, label);
	ndiLib->send_destroy(s); // the sender closes FIRST (active close), as OBS does at stop
	usleep(100000);
	close(c);
	usleep(100000);
	printf("%s.after_state=%d\n", label, ndi_sender_port_hold_state(port));
	return 0;
}

int main()
{
	// An unknown port must be a harmless no-op, never a crash.
	ndi_sender_abort_connections_before_destroy(0, "unknown");
	int rc = scenario("control", false);
	if (rc == 0)
		rc = scenario("linger", true);
	fflush(stdout);
	return rc;
}
"#;

#[test]
fn real_module_aborts_the_connection_so_no_time_wait_survives() {
    let dir = scratch("real");
    fs::write(dir.join("plugin-main.h"), STUB_PLUGIN_MAIN).expect("write the stub header");
    fs::write(dir.join("ndi-sender-port.h"), vendor_file(PORT_H)).expect("copy the header");
    fs::write(dir.join("ndi-sender-port.cpp"), vendor_file(PORT_CPP)).expect("copy the module");
    fs::write(dir.join("harness.cpp"), HARNESS).expect("write the harness");
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
    let stdout = compile_and_run(
        &cxx,
        &[
            "-std=c++17",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Wconversion",
            "-Werror",
            "-O1",
            "-pthread",
        ],
        &[dir.join("ndi-sender-port.cpp"), dir.join("harness.cpp")],
        &dir.join("real.bin"),
    );
    let get = |key: &str| -> String {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("issue 1363: harness printed no `{key}`:\n{stdout}"))
            .to_string()
    };
    for label in ["control", "linger"] {
        assert_eq!(
            get(&format!("{label}.port_found")),
            "1",
            "issue 1363: the tracked create did not identify the sender's listening port:\n{stdout}"
        );
        assert_eq!(
            get(&format!("{label}.live_state")),
            "2",
            "issue 1363: a live 0.0.0.0 listener must classify as LIVE_LISTENER (2):\n{stdout}"
        );
    }
    assert_eq!(
        get("control.after_state"),
        "1",
        "issue 1363: the CONTROL (no abort) must reproduce the bug — a server-first close leaves a \
         TIME_WAIT that blocks a plain bind (1). If it reads FREE the harness no longer proves \
         anything:\n{stdout}"
    );
    assert_eq!(
        get("linger.after_state"),
        "0",
        "issue 1363: with the abort-before-destroy the port must be FREE again right after the \
         stop (no TIME_WAIT), so the next OBS binds it:\n{stdout}"
    );
    assert!(
        stdout.contains("1 connection(s) set to close with RST"),
        "issue 1363: the abort must hit exactly the ONE accepted connection (never the client side \
         or the listener) and log it:\n{stdout}"
    );
}
