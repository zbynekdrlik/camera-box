//! #877 — regression gate for the aux-NDI-sender TEARDOWN ORDERING in
//! `vendor/distroav/src/ndi-filter.cpp`.
//!
//! Background: disabling all three aux NDI senders (interkom / MULTIVIEW / Grading) at once was
//! observed to wedge the strih PROGRAM output to 0 fps. Investigation (see the ticket's design
//! comment) established that (a) a mere OBS *disable* never reaches any sender-destroy in the
//! current code — `render_video()` in libobs skips a disabled filter before `ndi_filter_render_video`
//! — and (b) the one in-code way to reintroduce a destroy-race wedge is to break the teardown
//! ORDERING the ticket mandates preserving:
//!
//!   * `ndi_filter_destroy`  MUST `video_output_close(...)` (stop + JOIN the raw_video send worker)
//!     BEFORE `ndi_sender_destroy(...)` — never `send_destroy` while a send can still be in flight.
//!   * `ndi_sender_destroy`  MUST acquire the sender mutex(es) BEFORE `send_destroy(...)` — so no
//!     synchronous `send_send_video_v2` can be running under the mutex when `send_destroy` runs.
//!
//! This is a STATIC text gate (the same idiom as `genlock_release_cadence.rs` /
//! `aux_sender_budget_879.rs` for vendored code that CI is the first compile of): it proves the C
//! still *says* the right ordering. Per `vendored-libobs-change-safety.md` a gate is a lie until
//! watched go red, so `ordering_checker_rejects_a_reordered_fixture` runs the SAME checker over a
//! deliberately reordered fixture and requires it to FAIL — the mutation proof, baked in.

use std::path::PathBuf;

fn ndi_filter_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/distroav/src/ndi-filter.cpp");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("#877: cannot read {}: {e}", p.display()))
}

/// Slice the body of a top-level C++ function. `sig` is a substring unique to the function's
/// DEFINITION line (not a forward declaration); the body runs from there to the first column-0
/// closing brace (`"\n}"`). Inner blocks here close tab-indented (`"\n\t}"`), so `"\n}"` reliably
/// marks the function's own end. Scoping to the enclosing function (never a fixed byte window) is
/// the anchor-safety rule from `vendored-libobs-change-safety.md`.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("#877: signature anchor not found: {sig:?}"));
    let rest = &src[start..];
    let end = rest
        .find("\n}")
        .unwrap_or_else(|| panic!("#877: no column-0 closing brace after {sig:?}"));
    &rest[..end]
}

/// True iff, within `body`, every marker in `before` appears (each at least once) and strictly
/// PRECEDES the first occurrence of `after`. Returns false if any marker is missing or the order
/// is wrong — the discriminating predicate the mutation proof exercises.
fn all_before(body: &str, before: &[&str], after: &str) -> bool {
    let after_at = match body.find(after) {
        Some(i) => i,
        None => return false,
    };
    before.iter().all(|m| match body.find(m) {
        Some(i) => i < after_at,
        None => false,
    })
}

#[test]
fn ndi_filter_destroy_joins_before_destroying_the_sender() {
    let src = ndi_filter_src();
    let body = fn_body(&src, "void ndi_filter_destroy(void *data)\n{");
    assert!(
        all_before(body, &["video_output_close("], "ndi_sender_destroy("),
        "#877: ndi_filter_destroy must video_output_close (stop+join the raw_video send worker) \
         BEFORE ndi_sender_destroy — reordering reintroduces the send-in-flight destroy race that \
         wedged the program to 0 fps.\nBody was:\n{body}"
    );
}

#[test]
fn ndi_sender_destroy_locks_before_send_destroy() {
    let src = ndi_filter_src();
    let body = fn_body(&src, "void ndi_sender_destroy(ndi_filter_t *filter)\n{");
    assert!(
        all_before(
            body,
            &[
                "pthread_mutex_lock(&filter->ndi_sender_video_mutex)",
                "pthread_mutex_lock(&filter->ndi_sender_audio_mutex)",
            ],
            "ndiLib->send_destroy(",
        ),
        "#877: ndi_sender_destroy must hold BOTH sender mutexes BEFORE send_destroy, so no \
         synchronous send_send_video_v2 is in flight under the mutex when the sender is destroyed.\n\
         Body was:\n{body}"
    );
}

/// The mutation proof: the checker MUST reject a body whose ordering was reversed. Without this a
/// green result would not prove the checker discriminates (vendored-libobs-change-safety.md:
/// "a parity or mutation gate is a LIE until you watch it go red").
#[test]
fn ordering_checker_rejects_a_reordered_fixture() {
    // interkom-style teardown with the WRONG order: destroy before join.
    let broken = "\
void ndi_filter_destroy(void *data)
{
\tauto f = (ndi_filter_t *)data;
\tndi_sender_destroy(f);
\tvideo_output_close(f->video_output);
}
";
    let body = fn_body(broken, "void ndi_filter_destroy(void *data)\n{");
    assert!(
        !all_before(body, &["video_output_close("], "ndi_sender_destroy("),
        "#877: the ordering checker must REJECT a reordered (destroy-before-join) body — it is \
         blind otherwise"
    );

    // A missing marker must also fail closed.
    let missing =
        "void ndi_sender_destroy(ndi_filter_t *filter)\n{\n\tndiLib->send_destroy(filter->ndi_sender);\n}\n";
    let mbody = fn_body(missing, "void ndi_sender_destroy(ndi_filter_t *filter)\n{");
    assert!(
        !all_before(
            mbody,
            &["pthread_mutex_lock(&filter->ndi_sender_video_mutex)"],
            "ndiLib->send_destroy(",
        ),
        "#877: checker must fail closed when a required lock is absent"
    );
}
