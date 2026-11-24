//! Issue 1540 slice 2 -- the read-only HTTP endpoint of the cambox video counters.
//! RED: the API shape only; every connection is closed unanswered.

use std::net::TcpListener;

/// Not implemented yet: accepts and drops every connection.
pub fn serve(listener: TcpListener, _render: impl Fn() -> String) {
    for conn in listener.incoming() {
        drop(conn);
    }
}
