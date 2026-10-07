use markup5ever::data::{C1_REPLACEMENTS, NAMED_ENTITIES};

/// Decode HTML attribute character references, retaining byte boundaries for
/// surgical srcset rewrites. Markdown destinations have different entity rules.
pub(crate) fn decode_attribute(source: &str) -> (String, Vec<usize>) {
    let mut value = String::with_capacity(source.len());
    let mut offsets = vec![0];
    let mut cursor = 0;
    while cursor < source.len() {
        if source.as_bytes()[cursor] == b'&'
            && let Some((consumed, decoded)) = character_reference(&source[cursor + 1..])
        {
            cursor += consumed + 1;
            value.push_str(&decoded);
            offsets.extend(std::iter::repeat_n(cursor, decoded.len()));
        } else {
            let character = source[cursor..].chars().next().unwrap();
            value.push(character);
            offsets.extend(cursor + 1..=cursor + character.len_utf8());
            cursor += character.len_utf8();
        }
    }
    (value, offsets)
}

fn character_reference(source: &str) -> Option<(usize, String)> {
    let bytes = source.as_bytes();
    if bytes.first() == Some(&b'#') {
        let hex = matches!(bytes.get(1), Some(b'x' | b'X'));
        let start = if hex { 2 } else { 1 };
        let radix = if hex { 16 } else { 10 };
        let mut end = start;
        let mut number = 0u32;
        while let Some(digit) = bytes.get(end).and_then(|b| (*b as char).to_digit(radix)) {
            number = number.saturating_mul(radix).saturating_add(digit);
            end += 1;
        }
        if end == start {
            return None;
        }
        if bytes.get(end) == Some(&b';') {
            end += 1;
        }
        let character = match number {
            0x80..=0x9f => C1_REPLACEMENTS[(number - 0x80) as usize]
                .unwrap_or_else(|| char::from_u32(number).unwrap()),
            0 => '\u{fffd}',
            _ => char::from_u32(number).unwrap_or('\u{fffd}'),
        };
        return Some((end, character.to_string()));
    }
    let mut matched = None;
    // The HTML5 table includes prefixes and legacy names without semicolons.
    for (index, byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_alphanumeric() && *byte != b';' {
            break;
        }
        let end = index + 1;
        let Some(&(first, second)) = NAMED_ENTITIES.get(&source[..end]) else {
            break;
        };
        if first != 0 {
            matched = Some((end, first, second));
        }
    }
    let (end, first, second) = matched?;
    if bytes[end - 1] != b';'
        && bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'=')
    {
        return None;
    }
    let mut decoded = char::from_u32(first)?.to_string();
    if second != 0 {
        decoded.push(char::from_u32(second)?);
    }
    Some((end, decoded))
}
