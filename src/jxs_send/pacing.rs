//! Issue 1387: the clock half of the JPEG XS send pacing. The schedule itself is pure
//! (`crate::jxs_link::pace`); [`super::JxsSocket::send_slice_paced`] asks it for each send's
//! departure and waits here, on the sending thread, with an absolute `CLOCK_MONOTONIC` sleep.

/// `CLOCK_MONOTONIC` in nanoseconds (the pace schedule's clock).
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer; CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Sleep until `CLOCK_MONOTONIC` reaches `at_ns` (an absolute deadline, so a late wake never
/// shifts the next one; returns at once when it already passed; a signal resumes the wait).
pub fn sleep_until_monotonic_ns(at_ns: u64) {
    let ts = libc::timespec {
        tv_sec: (at_ns / 1_000_000_000) as libc::time_t,
        tv_nsec: (at_ns % 1_000_000_000) as libc::c_long,
    };
    loop {
        // SAFETY: `ts` is a valid timespec; the remainder pointer is unused with TIMER_ABSTIME.
        let rc = unsafe {
            libc::clock_nanosleep(
                libc::CLOCK_MONOTONIC,
                libc::TIMER_ABSTIME,
                &ts,
                std::ptr::null_mut(),
            )
        };
        if rc != libc::EINTR {
            return;
        }
    }
}

/// Timer slack of the calling thread down to 1 ns while it paces: the default 50 us slack would
/// delay most wake-ups of a ~0.2 ms send schedule by up to a quarter of it. Per thread, so only
/// the sending thread changes.
#[cfg_attr(not(feature = "jxs"), allow(dead_code))]
pub(super) fn tighten_timer_slack() {
    // SAFETY: PR_SET_TIMERSLACK takes a plain integer and touches only this thread.
    if unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, 1 as libc::c_ulong) } != 0 {
        tracing::warn!(
            "jxs send: the pacing thread's timer slack stays at its default ({}) -- each paced send may wake up to ~50 us late, the schedule catches it up (issue 1387)",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::net::UdpSocket;
    use std::os::fd::AsRawFd;

    /// Issue 1387: a paced frame over loopback. Timed on the SENDING side with the schedule's own
    /// clock: the schedule anchors at or after `t0` and the last send waits for its departure, so
    /// `t1 - t0 >= last_departure` holds exactly, whatever the runner's load (a descheduled thread
    /// only adds time). The upper bound is generous; the receiver proves every datagram arrived.
    #[test]
    fn a_paced_frame_is_spread_over_its_schedule_and_arrives_whole() {
        use jxs_link::pace::{segments_per_send, send_ranges, wire_bytes, PaceSchedule};
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        // a 1 MB burst would overflow a small default receive buffer
        set_int_opt(rx.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, 4 << 20);
        rx.set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let port = rx.local_addr().unwrap().port();
        let tx = JxsSocket::connect_with(&format!("127.0.0.1:{port}"), false).unwrap();
        // 40 slices of a 4 bpp 1080p frame (15360 bytes each)
        let slice: Vec<u8> = (0..15_360u32).map(|i| (i % 251) as u8).collect();
        let bufs: Vec<Vec<u8>> = (0..40u8)
            .map(|i| {
                let base = jxs_link::PacketHeader {
                    camera_id: 100,
                    timecode_100ns: 42,
                    slice_index: i + 1,
                    slice_count: 41,
                    offset: 0,
                    slice_len: 0,
                };
                let mut b = Vec::new();
                jxs_link::build_slice_datagrams(base, &slice, &mut b).unwrap();
                b
            })
            .collect();
        let datagrams: usize = bufs.iter().map(|b| b.len().div_ceil(DATAGRAM_LEN)).sum();
        // the schedule's span: every send ready at once, at 500 Mb/s
        let mut plan = PaceSchedule::new(500).unwrap();
        let mut last_departure = 0;
        for b in &bufs {
            for r in send_ranges(b.len(), segments_per_send(true)) {
                last_departure = plan.depart(0, wire_bytes(r.len()));
            }
        }
        assert!(
            (9_000_000..12_000_000).contains(&last_departure),
            "~10.4 ms of wire at 500 Mb/s: {last_departure} ns"
        );
        let reader = std::thread::spawn(move || {
            let mut d = [0u8; 2048];
            let mut n = 0usize;
            while n < datagrams && rx.recv(&mut d).is_ok() {
                n += 1;
            }
            n
        });
        let mut pace = PaceSchedule::new(500).unwrap();
        let mut paced_ns = 0;
        let t0 = monotonic_ns();
        for b in &bufs {
            let sent = tx.send_slice_paced(b, Some(&mut pace));
            assert_eq!(sent.errors, 0);
            paced_ns += sent.paced_ns;
        }
        let t1 = monotonic_ns();
        assert_eq!(reader.join().unwrap(), datagrams, "every datagram arrived");
        assert!(
            t1 - t0 >= last_departure,
            "{} ns < the schedule's {last_departure} ns: sent faster than the pace",
            t1 - t0
        );
        assert!(t1 - t0 < 500_000_000, "{} ns", t1 - t0);
        // the waits it reports are what held the sends back: at most the whole span
        assert!(
            paced_ns > 0 && paced_ns <= t1 - t0,
            "{paced_ns} of {}",
            t1 - t0
        );
        // line rate: the same frame, no waits, every send at once
        let line = bufs
            .iter()
            .map(|b| tx.send_slice_paced(b, None).paced_ns)
            .sum::<u64>();
        assert_eq!(line, 0);
    }
}
