//! Base64 encoding (RFC 4648 §4 and §5). Encoding only: ahi never decodes.

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Standard alphabet with `=` padding, as OSC 52 expects.
pub(crate) fn encode_standard(input: &[u8]) -> String {
    encode(input, STANDARD, true)
}

/// URL-safe alphabet without padding, for file names and tokens.
pub(crate) fn encode_url_safe_no_pad(input: &[u8]) -> String {
    encode(input, URL_SAFE, false)
}

fn encode(input: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let (chunks, rest) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            out.push(alphabet[((n >> shift) & 63) as usize] as char);
        }
    }
    match rest.len() {
        1 => {
            let n = u32::from(rest[0]) << 16;
            out.push(alphabet[((n >> 18) & 63) as usize] as char);
            out.push(alphabet[((n >> 12) & 63) as usize] as char);
            if pad {
                out.push_str("==");
            }
        }
        2 => {
            let n = (u32::from(rest[0]) << 16) | (u32::from(rest[1]) << 8);
            out.push(alphabet[((n >> 18) & 63) as usize] as char);
            out.push(alphabet[((n >> 12) & 63) as usize] as char);
            out.push(alphabet[((n >> 6) & 63) as usize] as char);
            if pad {
                out.push('=');
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_4648_test_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode_standard(input.as_bytes()), expected, "{input:?}");
        }
    }

    #[test]
    fn url_safe_uses_dash_and_underscore_without_padding() {
        // 0xfb 0xff 0xbf encodes to the last alphabet positions: `+/` vs `-_`.
        assert_eq!(encode_standard(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(encode_url_safe_no_pad(&[0xfb, 0xff, 0xbf]), "-_-_");
        assert_eq!(encode_url_safe_no_pad(b"f"), "Zg");
        assert_eq!(encode_url_safe_no_pad(b"fo"), "Zm8");
        assert_eq!(encode_url_safe_no_pad(&[0; 16]).len(), 22);
    }

    #[test]
    fn every_byte_value_round_trips_through_a_reference_decoder() {
        let all: Vec<u8> = (0..=255).collect();
        let encoded = encode_standard(&all);
        assert_eq!(encoded.len(), 344);
        let decoded = reference_decode(&encoded, STANDARD);
        assert_eq!(decoded, all);
    }

    /// Minimal decoder for the test only, so the encoder is checked against
    /// something other than itself.
    fn reference_decode(text: &str, alphabet: &[u8; 64]) -> Vec<u8> {
        let value = |c: u8| alphabet.iter().position(|a| *a == c).unwrap() as u32;
        let bytes: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let mut n = 0u32;
            for (i, c) in chunk.iter().enumerate() {
                n |= value(*c) << (18 - 6 * i);
            }
            let count = chunk.len() * 6 / 8;
            for i in 0..count {
                out.push(((n >> (16 - 8 * i)) & 0xff) as u8);
            }
        }
        out
    }
}
