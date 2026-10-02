use crate::source::Error;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use percent_encoding::percent_decode_str;

pub(super) fn decode(uri: &str) -> Result<String, Error> {
    let (header, data) = uri
        .get(5..)
        .and_then(|uri| uri.split_once(','))
        .ok_or_else(|| Error("inline source map data URI is missing its comma".into()))?;
    // Validate the decoded JSON independently of the generator's MIME label.
    let mut metadata = header.split(';').skip(1);
    // Strict percent validation; '+' is literal in data URIs, never a space.
    let bytes = data.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !(bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return Err(Error(
                "invalid percent encoding in inline source map".into(),
            ));
        }
    }
    let bytes = percent_decode_str(data).collect::<Vec<_>>();
    let bytes = if metadata.any(|item| item.eq_ignore_ascii_case("base64")) {
        STANDARD
            .decode(&bytes)
            .or_else(|_| STANDARD_NO_PAD.decode(&bytes))
            .map_err(|error| Error(format!("invalid inline source map base64: {error}")))?
    } else {
        bytes
    };
    String::from_utf8(bytes)
        .map_err(|error| Error(format!("inline source map is not UTF-8: {error}")))
}
