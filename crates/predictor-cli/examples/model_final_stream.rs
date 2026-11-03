// Private evaluation transport, not a production worker or a quality verdict.
use predictor_core::{Predictor, Request, Response, SearchConfig};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read, Write};

const MAX_LINE: usize = 128 * 1024;
const MAX_OUTPUT: usize = 256 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    id: u64,
    q: String,
    lang: String,
    limit: usize,
}

#[derive(Serialize)]
struct Output {
    id: u64,
    #[serde(flatten)]
    response: Response,
}

fn parse_request(bytes: &[u8], previous: u64) -> Result<Input, ()> {
    if bytes.len() > MAX_LINE {
        return Err(());
    }
    let input: Input = serde_json::from_slice(bytes).map_err(|_| ())?;
    if input.id <= previous
        || input.id > 9_007_199_254_740_991
        || input.lang != "ru"
        || input.limit != 5
        || input.q.len() > 16384
    {
        return Err(());
    }
    Ok(input)
}

fn parse_config(bytes: &[u8]) -> Result<SearchConfig, ()> {
    if bytes.len() > 65536 {
        return Err(());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let object = value.as_object().ok_or(())?;
    let expected = [
        "frequency_only",
        "letters",
        "spaces",
        "external_lexicon",
        "full_scan_oracle",
        "candidate_limit",
        "max_operations",
        "letter_cost",
        "space_cost",
    ];
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(());
    }
    serde_json::from_value(value).map_err(|_| ())
}

fn stream(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    mut complete: impl FnMut(&Input) -> Result<Response, ()>,
) -> Result<(), ()> {
    let mut previous = 0;
    let mut count = 0usize;
    loop {
        let mut line = Vec::new();
        let size = reader
            .take((MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| ())?;
        if size == 0 {
            return Ok(());
        }
        count += 1;
        if count > 1_000_000 || size > MAX_LINE || line.last() != Some(&b'\n') {
            return Err(());
        }
        let input = parse_request(&line, previous)?;
        previous = input.id;
        let output = serde_json::to_vec(&Output {
            id: input.id,
            response: complete(&input)?,
        })
        .map_err(|_| ())?;
        if output.len() > MAX_OUTPUT {
            return Err(());
        }
        writer.write_all(&output).map_err(|_| ())?;
        writer.write_all(b"\n").map_err(|_| ())?;
        writer.flush().map_err(|_| ())?;
    }
}

fn run() -> Result<(), ()> {
    let mut args = std::env::args_os().skip(1);
    let model = args.next().ok_or(())?;
    let config = args.next().ok_or(())?;
    if args.next().is_some() {
        return Err(());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(config)
        .map_err(|_| ())?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    let config = parse_config(&bytes)?;
    // Once per process, with no personal model, telemetry, profiles or learning.
    let predictor = Predictor::load(model)
        .map_err(|_| ())?
        .with_config(config)
        .map_err(|_| ())?;
    stream(
        &mut std::io::stdin().lock(),
        &mut std::io::BufWriter::new(std::io::stdout().lock()),
        |input| {
            predictor
                .complete(Request::new(&input.q), None)
                .map_err(|_| ())
        },
    )
}

fn main() {
    if run().is_err() {
        // stdout is a PRIVATE pipe, never workflow logs/artifacts. Errors reveal no content.
        eprintln!("evaluation stream blocked");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request_schema_sequence_and_utf8_bounds() {
        let input = br#"{"id":1,"q":"hello","lang":"ru","limit":5}"#;
        assert_eq!(parse_request(input, 0).unwrap().q, "hello");
        assert!(parse_request(input, 1).is_err());
        for bad in [
            r#"{"id":0,"q":"x","lang":"ru","limit":5}"#.to_string(),
            r#"{"id":9007199254740992,"q":"x","lang":"ru","limit":5}"#.to_string(),
            r#"{"id":1,"q":"x","lang":"en","limit":5}"#.to_string(),
            r#"{"id":1,"q":"x","lang":"ru","limit":3}"#.to_string(),
            r#"{"id":1,"q":"x","lang":"ru","limit":5,"profile":"no"}"#.to_string(),
            format!(
                r#"{{"id":1,"q":"{}","lang":"ru","limit":5}}"#,
                "я".repeat(8193)
            ),
            format!(
                r#"{{"id":1,"q":"{}","lang":"ru","limit":5}}"#,
                "x".repeat(MAX_LINE)
            ),
        ] {
            assert!(parse_request(bad.as_bytes(), 0).is_err());
        }
        let boundary = format!(
            r#"{{"id":1,"q":"{}","lang":"ru","limit":5}}"#,
            "я".repeat(8192)
        );
        assert!(parse_request(boundary.as_bytes(), 0).is_ok());
    }

    #[test]
    fn test_config_requires_complete_exact_schema() {
        let config = serde_json::to_vec(&SearchConfig::default()).unwrap();
        assert!(parse_config(&config).is_ok());
        assert!(parse_config(b"{}").is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&config).unwrap();
        value["unexpected"] = true.into();
        assert!(parse_config(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn test_stream_bounded_sequence_private_jsonl_and_flush() {
        let input = b"{\"id\":1,\"q\":\"a\",\"lang\":\"ru\",\"limit\":5}\n{\"id\":2,\"q\":\"b\",\"lang\":\"ru\",\"limit\":5}\n";
        let mut output = Vec::new();
        let mut calls = 0;
        let response = || Response {
            text: Vec::new(),
            pos: 0,
            end_of_word: false,
        };
        stream(&mut input.as_slice(), &mut output, |_| {
            calls += 1;
            Ok(response())
        })
        .unwrap();
        assert_eq!(calls, 2);
        let rows: Vec<serde_json::Value> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["id"], 2);
        assert_eq!(rows[0]["text"], serde_json::json!([]));
        assert!(
            stream(&mut &input[..input.len() - 1], &mut Vec::new(), |_| Ok(
                response()
            ))
            .is_err()
        );
        assert!(stream(
            &mut vec![b'x'; MAX_LINE + 1].as_slice(),
            &mut Vec::new(),
            |_| Ok(response())
        )
        .is_err());
        assert!(stream(&mut input.as_slice(), &mut Vec::new(), |_| Err(())).is_err());
    }
}
