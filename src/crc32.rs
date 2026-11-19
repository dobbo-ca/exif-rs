//! The CRC-32 of PNG chunks (ISO 3309, polynomial 0xEDB88320).

const TABLE: [u32; 256] = build();

const fn build() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// The CRC-32 of the concatenation of `parts`.
pub(crate) fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for part in parts {
        for &b in *part {
            c = TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn check_value() {
        assert_eq!(crc32(&[&b"1234"[..], &b"56789"[..]]), 0xCBF4_3926);
    }
}
