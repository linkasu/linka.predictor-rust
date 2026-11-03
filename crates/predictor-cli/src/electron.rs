use predictor_core::{Predictor, Request};
use serde::Deserialize;
use std::io::{Read, Write};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    q: String,
    lang: String,
    limit: usize,
}

fn parse_request(bytes: &[u8]) -> Result<Input, ()> {
    let input: Input = serde_json::from_slice(bytes).map_err(|_| ())?;
    if input.lang != "ru" || input.limit != 5 || input.q.len() > 16384 {
        return Err(());
    }
    Ok(input)
}

fn run() -> Result<(), i32> {
    let mut args = std::env::args_os().skip(1);
    let model = args.next().ok_or(12)?;
    if args.next().is_some() {
        return Err(12);
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| 12)?;
    if bytes.len() > 128 * 1024 {
        return Err(12);
    }
    let input = parse_request(&bytes).map_err(|_| 12)?;
    match std::fs::metadata(&model) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(10),
        Err(_) => return Err(11),
        Ok(m) if !m.is_file() => return Err(11),
        Ok(_) => {}
    }
    let predictor = Predictor::load(model).map_err(|_| 11)?;
    let response = predictor
        .complete(
            Request {
                q: &input.q,
                lang: "ru",
                limit: 5,
            },
            None,
        )
        .map_err(|_| 13)?;
    let output = serde_json::to_vec(&response).map_err(|_| 13)?;
    std::io::stdout().write_all(&output).map_err(|_| 13)
}

fn main() {
    if let Err(code) = run() {
        // Never echo user text, paths, model contents or underlying loader errors.
        eprintln!("predictor sidecar failed ({code})");
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_utf8_byte_bounds() {
        for (character, count) in [("x", 16384), ("я", 8192), ("😀", 4096)] {
            let q = character.repeat(count);
            let request = serde_json::json!({ "q": q, "lang": "ru", "limit": 5 });
            assert!(parse_request(&serde_json::to_vec(&request).unwrap()).is_ok());
            let request = serde_json::json!({ "q": q + character, "lang": "ru", "limit": 5 });
            assert!(parse_request(&serde_json::to_vec(&request).unwrap()).is_err());
        }
    }

    #[test]
    fn test_request_bounds_and_schema() {
        assert!(parse_request(r#"{"q":"я хочу ","lang":"ru","limit":5}"#.as_bytes()).is_ok());
        for bad in [
            r#"{"q":"hello","lang":"en","limit":5}"#.to_owned(),
            r#"{"q":"x","lang":"ru","limit":999}"#.to_owned(),
            r#"{"q":"x","lang":"ru","limit":5,"profile":"x"}"#.to_owned(),
            format!(r#"{{"q":"{}","lang":"ru","limit":5}}"#, "x".repeat(16385)),
        ] {
            assert!(parse_request(bad.as_bytes()).is_err());
        }
    }
}
