use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rax_core::ContentEncoding;

#[must_use]
pub(crate) fn encode_protocol_bytes(bytes: &[u8]) -> (String, ContentEncoding) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), ContentEncoding::Utf8),
        Err(_) => (BASE64.encode(bytes), ContentEncoding::Base64),
    }
}

#[must_use]
pub(crate) const fn encoding_name(encoding: ContentEncoding) -> &'static str {
    match encoding {
        ContentEncoding::Utf8 => "utf8",
        ContentEncoding::Base64 => "base64",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_is_preserved() {
        let (value, encoding) = encode_protocol_bytes("rax".as_bytes());
        assert_eq!(encoding, ContentEncoding::Utf8);
        assert_eq!(value, "rax");
    }

    #[test]
    fn invalid_utf8_is_lossless_base64() {
        let bytes = [0xff, 0x00, 0x80];
        let (value, encoding) = encode_protocol_bytes(&bytes);
        assert_eq!(encoding, ContentEncoding::Base64);
        assert_eq!(BASE64.decode(value).expect("base64"), bytes);
    }
}
