//! Issue 1387: a minimal PNG writer for the receive probe's decoded-frame dumps (RGB8, stored
//! deflate blocks: no compression, no dependency). A 1080p frame is ~6.2 MB; the probe writes a
//! handful of them for a pixel comparison with the NDI picture, never a stream.

fn crc32_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (n, e) in t.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *e = c;
    }
    t
}

/// CRC-32 (ISO-HDLC, the PNG chunk CRC).
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let t = crc32_table();
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &b in *p {
            c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Encode packed RGB8 (`3 x width x height` bytes) as a PNG file.
pub fn encode_rgb8(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row = width as usize * 3;
    assert_eq!(rgb.len(), row * height as usize, "rgb buffer size");
    // scanlines, each with filter byte 0
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for r in rgb.chunks(row) {
        raw.push(0);
        raw.extend_from_slice(r);
    }
    // zlib: header, stored blocks of at most 65535 bytes, adler32
    let mut z = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 16);
    z.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = raw.chunks(65535).peekable();
    if raw.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(b) = blocks.next() {
        z.push(if blocks.peek().is_none() { 1 } else { 0 });
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour, deflate, no filter, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_and_adler32_known_values() {
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn a_png_has_the_signature_valid_chunks_and_the_pixels() {
        let rgb: Vec<u8> = (0..2 * 2 * 3).map(|i| i as u8 * 10).collect();
        let png = encode_rgb8(&rgb, 2, 2);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // walk the chunks and check each CRC
        let mut p = 8;
        let mut kinds = Vec::new();
        let mut idat = Vec::new();
        while p < png.len() {
            let len = u32::from_be_bytes(png[p..p + 4].try_into().unwrap()) as usize;
            let kind = &png[p + 4..p + 8];
            let data = &png[p + 8..p + 8 + len];
            let crc = u32::from_be_bytes(png[p + 8 + len..p + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&[kind, data]));
            kinds.push(String::from_utf8(kind.to_vec()).unwrap());
            if kind == b"IDAT" {
                idat = data.to_vec();
            }
            p += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        // one stored final block: header(2) + 5 + 2 rows x (1 + 6) + adler(4)
        assert_eq!(idat.len(), 2 + 5 + 14 + 4);
        assert_eq!(idat[2], 1);
        assert_eq!(&idat[7..14], &[0, 0, 10, 20, 30, 40, 50]);
        assert_eq!(&idat[14..21], &[0, 60, 70, 80, 90, 100, 110]);
    }

    #[test]
    fn a_big_picture_splits_into_stored_blocks() {
        let rgb = vec![7u8; 200 * 200 * 3];
        let png = encode_rgb8(&rgb, 200, 200);
        // 200 rows x 601 = 120200 raw bytes = 2 stored blocks
        assert!(png.len() > 120_200 + 2 * 5);
    }
}
