//! Independent client-side application; never calls the runtime's edit helpers.

/// Apply the serialized public response's `pos` and one `text` entry.
/// Invalid Unicode or an unknown positive position is a protocol failure.
pub fn apply_suggestion(query: &str, pos: i32, text: &str) -> Result<String, &'static str> {
    let mut units: Vec<u16> = query.encode_utf16().collect();
    match pos {
        i32::MIN..=-1 => units.truncate(units.len().saturating_sub(pos.unsigned_abs() as usize)),
        0 => {}
        1 => units.push(u16::from(b' ')),
        _ => return Err("unsupported positive legacy position"),
    }
    units.extend(text.encode_utf16());
    String::from_utf16(&units).map_err(|_| "legacy response splits a UTF-16 surrogate pair")
}

#[cfg(test)]
mod tests {
    use super::apply_suggestion;

    #[test]
    fn applies_public_response_using_original_utf16_units() {
        assert_eq!(
            apply_suggestion("😀 при вет", -7, "привет").unwrap(),
            "😀 привет"
        );
        assert_eq!(
            apply_suggestion("😀 при вет", -7, "при ветер").unwrap(),
            "😀 при ветер"
        );
        assert_eq!(apply_suggestion("чай", 1, "готов").unwrap(), "чай готов");
        assert_eq!(apply_suggestion("чай ", 0, "готов").unwrap(), "чай готов");
        assert_eq!(
            apply_suggestion("ё е\u{301}", -2, "есть").unwrap(),
            "ё есть"
        );
        assert_eq!(apply_suggestion("чай", -100, "вода").unwrap(), "вода");
        assert!(apply_suggestion("😀", -1, "чай").is_err());
        assert!(apply_suggestion("чай", 2, "готов").is_err());
    }
}
