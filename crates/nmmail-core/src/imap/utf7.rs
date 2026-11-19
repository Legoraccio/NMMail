//! IMAP modified UTF-7 (RFC 3501 §5.1.3) for mailbox names.

use base64::Engine as _;
use base64::alphabet::IMAP_MUTF7;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig, general_purpose::NO_PAD};

const ENGINE: GeneralPurpose = GeneralPurpose::new(
    &IMAP_MUTF7,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Decodes a modified UTF-7 mailbox name; returns the input on malformed data.
pub fn decode(s: &str) -> String {
    let _ = NO_PAD;
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(dash) = after.find('-') else {
            return s.to_string();
        };
        let chunk = &after[..dash];
        if chunk.is_empty() {
            out.push('&');
        } else {
            let Ok(bytes) = ENGINE.decode(chunk) else {
                return s.to_string();
            };
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            match String::from_utf16(&units) {
                Ok(t) => out.push_str(&t),
                Err(_) => return s.to_string(),
            }
        }
        rest = &after[dash + 1..];
    }
    out.push_str(rest);
    out
}

/// Encodes a mailbox name into modified UTF-7.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&ENGINE.encode(bytes));
        out.push('-');
        pending.clear();
    };
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
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
        for s in [
            "INBOX",
            "Posta in arrivo",
            "Élément envoyés",
            "日本語",
            "A&B",
            "Café & 🎉",
        ] {
            assert_eq!(decode(&encode(s)), s);
        }
        assert_eq!(decode("&AMk-l&AOk-ments envoy&AOk-s"), "Éléments envoyés");
        assert_eq!(encode("A&B"), "A&-B");
    }
}
