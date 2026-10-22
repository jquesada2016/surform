//! Base64 ENCODE only (RFC 4648, standard alphabet, `=` padding) —
//! the one transformation `migrate push`'s `Authorization: Basic`
//! header needs. In-repo like the SHA-256 module ([`super::sha256`]):
//! the pipeline's one dependency addition is the HTTP client alone,
//! and encode-only base64 is thirty lines with published test vectors.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `bytes` as base64 with padding.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let word = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(word >> 18) as usize & 63] as char);
        out.push(ALPHABET[(word >> 12) as usize & 63] as char);
        out.push(match chunk.len() {
            1 => '=',
            _ => ALPHABET[(word >> 6) as usize & 63] as char,
        });
        out.push(match chunk.len() {
            3 => ALPHABET[word as usize & 63] as char,
            _ => '=',
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_test_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn credentials_round_trip_through_known_spellings() {
        // The RFC 7617 textbook example, plus a shape push encodes.
        assert_eq!(
            encode(b"Aladdin:open sesame"),
            "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(encode(b"user:p@ss w0rd"), "dXNlcjpwQHNzIHcwcmQ=");
    }
}
