//! Unicode 16 names with Java 25 `Character.codePointOf` matching semantics.
//!
//! Data is generated from licensed Unicode Consortium UCD inputs, not JDK
//! tables. See `docs/unicode-data.md` and `third_party/unicode/LICENSE.txt`.
//! Lookup borrows read-only binary tables and uses a bounded stack buffer.

use std::cmp::Ordering;

static NAMES: &[u8] = include_bytes!("data/names.bin");
static NAME_RECORDS: &[u8] = include_bytes!("data/name_records.bin");
static RANGE_PREFIXES: &[u8] = include_bytes!("data/range_prefixes.bin");
static RANGE_RECORDS: &[u8] = include_bytes!("data/range_records.bin");
static UPPERCASE_ASCII: &[u8] = include_bytes!("data/uppercase_ascii.bin");
static DECIMAL_STARTS: &[u8] = include_bytes!("data/decimal_starts.bin");

// Generation rejects a Unicode version whose names exceed this bound. The
// longest Unicode 16 explicit name is 88 bytes; generated block names are less.
const MAX_NAME_BYTES: usize = 128;

/// Resolves a UTF-16 name as Java 25 `Character.codePointOf` does.
///
/// Leading/trailing units <= U+0020 are trimmed; Unicode ROOT uppercase is
/// applied, including non-ASCII forms that map to ASCII names. Names and spaces
/// within names otherwise require an exact match. Standard Unicode algorithmic
/// Hangul/CJK names are not Java names; Java uses the block name plus hex value.
///
/// Some successful results are surrogate code points (for example the name
/// `HIGH SURROGATES D800`). Callers writing Java strings must preserve them as
/// UTF-16 units, rather than requiring a Rust Unicode scalar value.
pub fn lookup_utf16(mut name: &[u16]) -> Option<u32> {
    while name.first().is_some_and(|&unit| unit <= 0x20) {
        name = &name[1..];
    }
    while name.last().is_some_and(|&unit| unit <= 0x20) {
        name = &name[..name.len() - 1];
    }
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return None;
    }

    let mut normalized = [0; MAX_NAME_BYTES];
    let mut length = 0;
    for &unit in name {
        if unit < 128 {
            *normalized.get_mut(length)? = (unit as u8).to_ascii_uppercase();
            length += 1;
        } else {
            let replacement = uppercase_ascii(unit)?;
            normalized
                .get_mut(length..length + replacement.len())?
                .copy_from_slice(replacement);
            length += replacement.len();
        }
    }
    let name = &normalized[..length];
    lookup_explicit(name).or_else(|| lookup_generated(name))
}

/// Java 25 `Character.digit(char, 16)` for one UTF-16 code unit.
///
/// Decimal digits include every Unicode 16 BMP decimal range. Hexadecimal A-F
/// additionally accepts ASCII and fullwidth Latin forms. Supplementary-plane
/// digits require a code-point API and are not accepted as isolated surrogates.
pub fn hex_digit_utf16(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x39 => return Some((unit - 0x30) as u8),
        0x41..=0x46 => return Some((unit - 0x41 + 10) as u8),
        0x61..=0x66 => return Some((unit - 0x61 + 10) as u8),
        0xff21..=0xff26 => return Some((unit - 0xff21 + 10) as u8),
        0xff41..=0xff46 => return Some((unit - 0xff41 + 10) as u8),
        _ => {}
    }
    let mut low = 0;
    let mut high = DECIMAL_STARTS.len() / 2;
    while low < high {
        let middle = low + (high - low) / 2;
        let start = read_u16(DECIMAL_STARTS, middle * 2);
        if unit < start {
            high = middle;
        } else if unit - start < 10 {
            return Some((unit - start) as u8);
        } else {
            low = middle + 1;
        }
    }
    None
}

fn lookup_explicit(name: &[u8]) -> Option<u32> {
    let mut low = 0;
    let mut high = NAME_RECORDS.len() / 8 - 1;
    while low < high {
        let middle = low + (high - low) / 2;
        let record = middle * 8;
        let start = read_u32(NAME_RECORDS, record + 4) as usize;
        let end = read_u32(NAME_RECORDS, record + 12) as usize;
        match name.cmp(&NAMES[start..end]) {
            Ordering::Less => high = middle,
            Ordering::Greater => low = middle + 1,
            Ordering::Equal => return Some(read_u32(NAME_RECORDS, record)),
        }
    }
    None
}

fn lookup_generated(name: &[u8]) -> Option<u32> {
    let separator = name.iter().rposition(|&byte| byte == b' ')?;
    let hex = &name[separator + 1..];
    if hex.is_empty() || hex.len() > 6 || (hex.len() > 1 && hex[0] == b'0') {
        return None;
    }
    let mut code_point = 0_u32;
    for &byte in hex {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        code_point = (code_point << 4) | u32::from(digit);
    }

    let mut low = 0;
    let mut high = RANGE_RECORDS.len() / 14;
    while low < high {
        let middle = low + (high - low) / 2;
        let record = middle * 14;
        let start = read_u32(RANGE_RECORDS, record);
        let end = read_u32(RANGE_RECORDS, record + 4);
        if code_point < start {
            high = middle;
        } else if code_point > end {
            low = middle + 1;
        } else {
            let offset = read_u32(RANGE_RECORDS, record + 8) as usize;
            let length = read_u16(RANGE_RECORDS, record + 12) as usize;
            return (name[..separator] == RANGE_PREFIXES[offset..offset + length])
                .then_some(code_point);
        }
    }
    None
}

fn uppercase_ascii(unit: u16) -> Option<&'static [u8]> {
    let mut low = 0;
    let mut high = UPPERCASE_ASCII.len() / 6;
    while low < high {
        let middle = low + (high - low) / 2;
        let record = middle * 6;
        match unit.cmp(&read_u16(UPPERCASE_ASCII, record)) {
            Ordering::Less => high = middle,
            Ordering::Greater => low = middle + 1,
            Ordering::Equal => {
                let length = UPPERCASE_ASCII[record + 2] as usize;
                return Some(&UPPERCASE_ASCII[record + 3..record + 3 + length]);
            }
        }
    }
    None
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::{hex_digit_utf16, lookup_utf16};

    fn lookup(name: &str) -> Option<u32> {
        lookup_utf16(&name.encode_utf16().collect::<Vec<_>>())
    }

    #[test]
    fn canonical_unicode16_names_and_java_controls() {
        assert_eq!(lookup("LATIN CAPITAL LETTER A"), Some(0x41));
        assert_eq!(lookup("HANGUL SYLLABLES AC00"), Some(0xac00));
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPHS 4E00"), Some(0x4e00));
        assert_eq!(lookup("CJK COMPATIBILITY IDEOGRAPH-F900"), Some(0xf900));
        assert_eq!(lookup("OL ONAL LETTER O"), Some(0x1e5d0));
        assert_eq!(lookup("NULL"), Some(0));
        assert_eq!(lookup("BEL"), Some(7));
        assert_eq!(lookup("BELL"), Some(0x1f514));
        assert_eq!(lookup("PADDING CHARACTER"), Some(0x80));
        assert_eq!(lookup("LINE FEED (LF)"), Some(10));
    }

    #[test]
    fn non_names_and_unassigned_points_are_rejected() {
        for name in [
            "",
            "HANGUL SYLLABLE GA",
            "CJK UNIFIED IDEOGRAPH-4E00",
            "CJK UNIFIED IDEOGRAPHS 04E00",
            "CJK UNIFIED IDEOGRAPHS +4E00",
            "CJK UNIFIED IDEOGRAPHS 9FFD0",
            "BASIC LATIN 41",
            "LINE FEED",
            "NUL",
            "TAB",
            "LATIN  CAPITAL LETTER A",
            "LATIN_CAPITAL_LETTER_A",
            "GREEK 378",
            "SUPPLEMENTARY PRIVATE USE AREA B 10FFFF",
        ] {
            assert_eq!(lookup(name), None, "{name}");
        }
    }

    #[test]
    fn root_uppercase_and_java_trim() {
        assert_eq!(lookup("\0\t latin small letter a \r\n"), Some(0x61));
        assert_eq!(lookup("latın capıtal letter a"), Some(0x41));
        assert_eq!(lookup("latın ſmall letter ſharp s"), Some(0xdf));
        assert_eq!(lookup("white cheß pawn"), Some(0x2659));
        assert_eq!(lookup("LATIN SMALL LIGATURE ﬃ"), Some(0xfb03));
        assert_eq!(lookup("\u{a0}LATIN CAPITAL LETTER A"), None);
        assert_eq!(lookup("LATIN CAPITAL LETTER A\u{2000}"), None);
        assert_eq!(lookup_utf16(&[0xd800]), None);
        let name = format!("{}NULL{}", " ".repeat(1_000), " ".repeat(1_000));
        assert_eq!(lookup(&name), Some(0));
        assert_eq!(lookup(&"a".repeat(129)), None);
    }

    #[test]
    fn java_names_can_resolve_surrogates_and_private_use() {
        assert_eq!(lookup("HIGH SURROGATES D800"), Some(0xd800));
        assert_eq!(lookup("HIGH PRIVATE USE SURROGATES DB80"), Some(0xdb80));
        assert_eq!(lookup("LOW SURROGATES DC00"), Some(0xdc00));
        assert_eq!(lookup("PRIVATE USE AREA E000"), Some(0xe000));
        assert_eq!(
            lookup("SUPPLEMENTARY PRIVATE USE AREA B 10FFFD"),
            Some(0x10fffd)
        );
    }

    #[test]
    fn bmp_hex_digits_match_java_categories() {
        for (unit, expected) in [
            (b'7' as u16, 7),
            (b'f' as u16, 15),
            (0x0664, 4),
            (0xff12, 2),
            (0xff26, 15),
        ] {
            assert_eq!(hex_digit_utf16(unit), Some(expected));
        }
        for unit in [b'g' as u16, 0x00b2, 0x2165, 0xd835, 0xdfe0, 0xffff] {
            assert_eq!(hex_digit_utf16(unit), None);
        }
    }
}
