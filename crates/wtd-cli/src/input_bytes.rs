const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEncoding {
    Utf8,
    Escaped,
    Hex,
    Base64,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum InputBytesError {
    #[error("hex input must contain an even number of digits")]
    OddHexLength,
    #[error("invalid hex digit: {0}")]
    InvalidHexDigit(char),
    #[error("unsupported escape sequence: \\{0}")]
    InvalidEscape(char),
    #[error("unterminated escape sequence")]
    UnterminatedEscape,
    #[error("invalid unicode escape")]
    InvalidUnicodeEscape,
    #[error("invalid base64 input")]
    InvalidBase64,
}

pub fn encode_input_payload(
    data: &str,
    encoding: InputEncoding,
) -> Result<String, InputBytesError> {
    let bytes = match encoding {
        InputEncoding::Utf8 => data.as_bytes().to_vec(),
        InputEncoding::Escaped => parse_escaped_bytes(data)?,
        InputEncoding::Hex => decode_hex(data)?,
        InputEncoding::Base64 => decode_base64(data)?,
    };
    Ok(encode_base64(&bytes))
}

fn decode_hex(input: &str) -> Result<Vec<u8>, InputBytesError> {
    let digits: Vec<char> = input
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != '_')
        .collect();

    if digits.len() % 2 != 0 {
        return Err(InputBytesError::OddHexLength);
    }

    let mut bytes = Vec::with_capacity(digits.len() / 2);
    let mut idx = 0;
    while idx < digits.len() {
        let hi = hex_value(digits[idx])?;
        let lo = hex_value(digits[idx + 1])?;
        bytes.push((hi << 4) | lo);
        idx += 2;
    }
    Ok(bytes)
}

fn hex_value(ch: char) -> Result<u8, InputBytesError> {
    match ch {
        '0'..='9' => Ok(ch as u8 - b'0'),
        'a'..='f' => Ok(ch as u8 - b'a' + 10),
        'A'..='F' => Ok(ch as u8 - b'A' + 10),
        _ => Err(InputBytesError::InvalidHexDigit(ch)),
    }
}

fn parse_escaped_bytes(input: &str) -> Result<Vec<u8>, InputBytesError> {
    let mut out = Vec::new();
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            continue;
        }

        let escaped = chars.next().ok_or(InputBytesError::UnterminatedEscape)?;
        match escaped {
            '\\' => out.push(b'\\'),
            '\'' => out.push(b'\''),
            '"' => out.push(b'"'),
            '0' => out.push(0),
            'n' => out.push(b'\n'),
            'r' => out.push(b'\r'),
            't' => out.push(b'\t'),
            'e' | 'E' => out.push(0x1B),
            'x' => {
                let hi = chars.next().ok_or(InputBytesError::UnterminatedEscape)?;
                let lo = chars.next().ok_or(InputBytesError::UnterminatedEscape)?;
                out.push((hex_value(hi)? << 4) | hex_value(lo)?);
            }
            'u' => {
                if chars.next() != Some('{') {
                    return Err(InputBytesError::InvalidUnicodeEscape);
                }
                let mut hex = String::new();
                loop {
                    let next = chars.next().ok_or(InputBytesError::InvalidUnicodeEscape)?;
                    if next == '}' {
                        break;
                    }
                    hex.push(next);
                }
                if hex.is_empty() || hex.len() > 6 {
                    return Err(InputBytesError::InvalidUnicodeEscape);
                }
                let scalar = u32::from_str_radix(&hex, 16)
                    .map_err(|_| InputBytesError::InvalidUnicodeEscape)?;
                let value = char::from_u32(scalar).ok_or(InputBytesError::InvalidUnicodeEscape)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(value.encode_utf8(&mut buf).as_bytes());
            }
            other => return Err(InputBytesError::InvalidEscape(other)),
        }
    }
    Ok(out)
}

fn decode_base64(input: &str) -> Result<Vec<u8>, InputBytesError> {
    const DECODE: [u8; 256] = {
        let mut table = [0xFFu8; 256];
        let mut i = 0u8;
        while i < 26 {
            table[(b'A' + i) as usize] = i;
            table[(b'a' + i) as usize] = i + 26;
            i += 1;
        }
        let mut d = 0u8;
        while d < 10 {
            table[(b'0' + d) as usize] = d + 52;
            d += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };

    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| !matches!(b, b'=' | b'\n' | b'\r'))
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);

    for chunk in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            let value = DECODE[b as usize];
            if value == 0xFF {
                return Err(InputBytesError::InvalidBase64);
            }
            buf[i] = value;
        }
        let n = chunk.len();
        if n >= 2 {
            out.push((buf[0] << 2) | (buf[1] >> 4));
        }
        if n >= 3 {
            out.push((buf[1] << 4) | (buf[2] >> 2));
        }
        if n >= 4 {
            out.push((buf[2] << 6) | buf[3]);
        }
    }

    Ok(out)
}

fn encode_base64(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_sequences_support_escape_and_cr() {
        let encoded = encode_input_payload(r"\e[<35;40;12M\r", InputEncoding::Escaped).unwrap();
        assert_eq!(decode_base64(&encoded).unwrap(), b"\x1B[<35;40;12M\r");
    }

    #[test]
    fn hex_sequences_decode() {
        let encoded = encode_input_payload("1b5b41", InputEncoding::Hex).unwrap();
        assert_eq!(decode_base64(&encoded).unwrap(), b"\x1B[A");
    }

    #[test]
    fn invalid_escape_fails() {
        assert_eq!(
            encode_input_payload(r"\q", InputEncoding::Escaped).unwrap_err(),
            InputBytesError::InvalidEscape('q')
        );
    }
}
