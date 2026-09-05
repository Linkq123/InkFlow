use std::borrow::Cow;

use chardetng::EncodingDetector;
use encoding_rs::Encoding;

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone)]
pub struct DecodedText {
    pub content: String,
    pub encoding: String,
    pub eol: String,
    pub had_bom: bool,
    pub had_final_newline: bool,
}

pub fn decode(bytes: &[u8]) -> ApiResult<DecodedText> {
    let (text, encoding, had_bom) = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (
            String::from_utf8(rest.to_vec())
                .map_err(|error| ApiError::new("invalid_encoding", error.to_string()))?,
            "utf-8".to_string(),
            true,
        )
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        (decode_utf16(rest, true)?, "utf-16le".to_string(), true)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        (decode_utf16(rest, false)?, "utf-16be".to_string(), true)
    } else if let Ok(value) = std::str::from_utf8(bytes) {
        (value.to_string(), "utf-8".to_string(), false)
    } else {
        let mut detector = EncodingDetector::new();
        detector.feed(bytes, true);
        let guessed = detector.guess(None, true);
        let (decoded, _, had_errors) = guessed.decode(bytes);
        if had_errors {
            return Err(ApiError::new(
                "invalid_encoding",
                "The file contains bytes that cannot be decoded safely.",
            ));
        }
        (
            decoded.into_owned(),
            guessed.name().to_ascii_lowercase(),
            false,
        )
    };

    let eol = detect_eol(&text).to_string();
    let had_final_newline = text.ends_with('\n') || text.ends_with('\r');
    let content = normalize_eol(&text);

    Ok(DecodedText {
        content,
        encoding,
        eol,
        had_bom,
        had_final_newline,
    })
}

pub fn encode(content: &str, encoding: &str, eol: &str, had_bom: bool) -> ApiResult<Vec<u8>> {
    let normalized = normalize_eol(content);
    let with_eol = if eol.eq_ignore_ascii_case("crlf") {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    };
    let encoding = canonical_name(encoding)?;

    match encoding.as_str() {
        "utf-8" => {
            let mut output = Vec::with_capacity(with_eol.len() + 3);
            if had_bom {
                output.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            output.extend_from_slice(with_eol.as_bytes());
            Ok(output)
        }
        "utf-16le" => encode_utf16(&with_eol, true, had_bom),
        "utf-16be" => encode_utf16(&with_eol, false, had_bom),
        label => {
            let codec = Encoding::for_label(label.as_bytes()).ok_or_else(|| {
                ApiError::new(
                    "unsupported_encoding",
                    format!("Unsupported encoding: {label}"),
                )
            })?;
            let (encoded, _, had_errors) = codec.encode(&with_eol);
            if had_errors {
                return Err(ApiError::new(
                    "encoding_loss",
                    format!("Some characters cannot be represented as {label}."),
                ));
            }
            Ok(match encoded {
                Cow::Borrowed(value) => value.to_vec(),
                Cow::Owned(value) => value,
            })
        }
    }
}

pub fn canonical_name(encoding: &str) -> ApiResult<String> {
    match encoding.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => Ok("utf-8".into()),
        "utf-16le" => Ok("utf-16le".into()),
        "utf-16be" => Ok("utf-16be".into()),
        label => Encoding::for_label(label.as_bytes())
            .map(|codec| codec.name().to_ascii_lowercase())
            .ok_or_else(|| {
                ApiError::new(
                    "unsupported_encoding",
                    format!("Unsupported encoding: {label}"),
                )
            }),
    }
}

#[cfg(feature = "cli")]
pub fn supports_bom(encoding: &str) -> bool {
    matches!(encoding, "utf-8" | "utf-16le" | "utf-16be")
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> ApiResult<String> {
    if bytes.len() % 2 != 0 {
        return Err(ApiError::new(
            "invalid_encoding",
            "UTF-16 file has an incomplete code unit.",
        ));
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect();
    String::from_utf16(&units).map_err(|error| ApiError::new("invalid_encoding", error.to_string()))
}

fn encode_utf16(text: &str, little_endian: bool, had_bom: bool) -> ApiResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
    if had_bom {
        bytes.extend_from_slice(if little_endian {
            &[0xFF, 0xFE]
        } else {
            &[0xFE, 0xFF]
        });
    }
    for unit in text.encode_utf16() {
        let encoded = if little_endian {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        };
        bytes.extend_from_slice(&encoded);
    }
    Ok(bytes)
}

pub(crate) fn normalize_eol(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn detect_eol(text: &str) -> &'static str {
    let crlf = text.match_indices("\r\n").count();
    let lf = text.matches('\n').count().saturating_sub(crlf);
    if crlf > lf { "crlf" } else { "lf" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_utf8_bom_and_crlf() {
        let source = b"\xEF\xBB\xBF# InkFlow\r\n\r\n\xE4\xB8\xAD\xE6\x96\x87\r\n";
        let decoded = decode(source).unwrap();
        assert_eq!(decoded.encoding, "utf-8");
        assert_eq!(decoded.eol, "crlf");
        assert!(decoded.had_bom);
        assert_eq!(
            encode(
                &decoded.content,
                &decoded.encoding,
                &decoded.eol,
                decoded.had_bom
            )
            .unwrap(),
            source
        );
    }

    #[test]
    fn round_trips_utf16le() {
        let source = encode_utf16("你好\r\n", true, true).unwrap();
        let decoded = decode(&source).unwrap();
        assert_eq!(decoded.encoding, "utf-16le");
        assert_eq!(
            encode(
                &decoded.content,
                &decoded.encoding,
                &decoded.eol,
                decoded.had_bom
            )
            .unwrap(),
            source
        );
    }
}
