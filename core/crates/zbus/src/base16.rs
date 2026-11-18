//! Base-16 text encoding helpers for the D-Bus authentication handshake.
//!
//! The protocol transports opaque byte strings labeled in base-16 (lowercase)
//! form; these helpers are the minimal encode/decode surface used here.

use std::fmt;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Failure modes of [`decode`].
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum InvalidBase16 {
    /// Odd-length input.
    OddLength,
    /// Invalid base-16 character.
    InvalidCharacter { c: char, index: usize },
}

impl fmt::Display for InvalidBase16 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OddLength => write!(f, "Invalid base-16 length"),
            Self::InvalidCharacter { c, index } => {
                write!(f, "Invalid base-16 character `{c}` at index {index}")
            }
        }
    }
}

impl std::error::Error for InvalidBase16 {}

/// Encodes the input bytes as lowercase base-16 text.
pub fn encode<T: AsRef<[u8]>>(data: T) -> String {
    let bytes = data.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xF) as usize] as char);
    }
    out
}

fn value(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(c as u8 - b'0'),
        'a'..='f' => Some(c as u8 - b'a' + 10),
        'A'..='F' => Some(c as u8 - b'A' + 10),
        _ => None,
    }
}

/// Decodes lowercase or uppercase base-16 text into bytes.
pub fn decode(text: &str) -> Result<Vec<u8>, InvalidBase16> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() % 2 == 1 {
        return Err(InvalidBase16::OddLength);
    }
    let mut out = Vec::with_capacity(chars.len() / 2);
    for (pair, chunk) in chars.chunks(2).enumerate() {
        let hi = value(chunk[0]).ok_or(InvalidBase16::InvalidCharacter {
            c: chunk[0],
            index: pair * 2,
        })?;
        let lo = value(chunk[1]).ok_or(InvalidBase16::InvalidCharacter {
            c: chunk[1],
            index: pair * 2 + 1,
        })?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}
