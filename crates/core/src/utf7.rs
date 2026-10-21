//! Modified UTF-7 (RFC 3501 §5.1.3), the encoding of IMAP folder names.

use base64::Engine as _;
use base64::alphabet::IMAP_MUTF7;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig, DecodePaddingMode};

const B64: GeneralPurpose = GeneralPurpose::new(
    &IMAP_MUTF7,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

pub fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            // Unterminated sequence: keep the text as is.
            out.push_str(&rest[start..]);
            return out;
        };
        let chunk = &after[..end];
        if chunk.is_empty() {
            out.push('&');
        } else if let Ok(bytes) = B64.decode(chunk) {
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            out.push_str(&String::from_utf16_lossy(&units));
        } else {
            out.push('&');
            out.push_str(chunk);
            out.push('-');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if !pending.is_empty() {
            let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
            out.push('&');
            out.push_str(&B64.encode(bytes));
            out.push('-');
            pending.clear();
        }
    };
    for c in s.chars() {
        if c == '&' {
            flush(&mut pending, &mut out);
            out.push_str("&-");
        } else if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for s in ["INBOX", "Envoyés", "Éléments supprimés", "R&D", "日本語/メール"] {
            assert_eq!(decode(&encode(s)), s);
        }
        assert_eq!(decode("Envoy&AOk-s"), "Envoyés");
        assert_eq!(encode("R&D"), "R&-D");
    }
}
