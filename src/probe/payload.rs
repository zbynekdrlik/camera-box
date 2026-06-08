//! QR payload: a compact ASCII string carrying frame identity + emission time.
//!
//! Wire format: `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` where crc32 is the
//! ISO-HDLC CRC of the body `{run_id}.{frame_id}.{gen_ts_ns}`. ASCII (not binary)
//! so it round-trips cleanly through QR text decoding.

use crc::{Crc, CRC_32_ISO_HDLC};

const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payload {
    pub run_id: u32,
    pub frame_id: u32,
    pub gen_ts_ns: i64,
}

impl Payload {
    fn body(&self) -> String {
        format!("{}.{}.{}", self.run_id, self.frame_id, self.gen_ts_ns)
    }

    /// Encode to the wire string (with CRC).
    pub fn encode(&self) -> String {
        let body = self.body();
        let crc = CRC32.checksum(body.as_bytes());
        format!("P{}.{}", body, crc)
    }

    /// Decode from the wire string. Returns None on malformed input or CRC mismatch.
    pub fn decode(s: &str) -> Option<Payload> {
        let rest = s.strip_prefix('P')?;
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() != 4 {
            return None;
        }
        let run_id: u32 = parts[0].parse().ok()?;
        let frame_id: u32 = parts[1].parse().ok()?;
        let gen_ts_ns: i64 = parts[2].parse().ok()?;
        let crc: u32 = parts[3].parse().ok()?;
        let p = Payload {
            run_id,
            frame_id,
            gen_ts_ns,
        };
        if CRC32.checksum(p.body().as_bytes()) != crc {
            return None;
        }
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_fields() {
        let p = Payload {
            run_id: 42,
            frame_id: 9001,
            gen_ts_ns: 1_234_567_890,
        };
        assert_eq!(Payload::decode(&p.encode()), Some(p));
    }

    #[test]
    fn zero_values_roundtrip() {
        let p = Payload {
            run_id: 0,
            frame_id: 0,
            gen_ts_ns: 0,
        };
        assert_eq!(Payload::decode(&p.encode()), Some(p));
    }

    #[test]
    fn corrupted_crc_is_rejected() {
        let p = Payload {
            run_id: 1,
            frame_id: 2,
            gen_ts_ns: 3,
        };
        let mut s = p.encode();
        let last = s.pop().unwrap();
        let flipped = if last == '0' { '1' } else { '0' };
        s.push(flipped);
        assert_eq!(Payload::decode(&s), None);
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(Payload::decode("hello world"), None);
        assert_eq!(Payload::decode("P1.2.3"), None);
        assert_eq!(Payload::decode(""), None);
    }
}
