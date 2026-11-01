use predictor_core::{is_supported_external_word, normalize, Result};
use quick_xml::{events::Event, Reader};
use rusqlite::Connection;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// External dictionary only: stream XML into disk-backed dedup; never add counts.
pub fn extract(input: &Path, output: &Path) -> Result<u64> {
    Ok(extract_report(input, output)?.wordforms)
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ExtractionReport {
    pub wordforms: u64,
    pub unsupported_forms: u64,
    pub duplicate_forms: u64,
}

pub fn extract_report(input: &Path, output: &Path) -> Result<ExtractionReport> {
    let mut report = ExtractionReport::default();
    let file = File::open(input)?;
    let input: Box<dyn Read> = if input.extension().is_some_and(|e| e == "bz2") {
        Box::new(bzip2::read::BzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut reader = Reader::from_reader(BufReader::new(input));
    let mut buffer = Vec::new();
    let db = output.with_extension("sqlite");
    if db.exists() || output.exists() {
        return Err("lexicon output exists".into());
    }
    let connection = Connection::open(db)?;
    connection.execute_batch("PRAGMA cache_size=-8192; PRAGMA temp_store=FILE; CREATE TABLE words(word TEXT PRIMARY KEY); BEGIN;")?;
    let mut depth = 0usize;
    let mut root_seen = false;
    loop {
        let event = reader.read_event_into(&mut buffer)?;
        match &event {
            Event::Start(element) | Event::Empty(element) if depth == 0 => {
                if root_seen || element.name().as_ref() != b"dictionary" {
                    return Err("expected one dictionary XML root".into());
                }
                root_seen = true;
            }
            _ => {}
        }
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.checked_sub(1).ok_or("unbalanced dictionary XML")?,
            Event::Eof => {
                if !root_seen || depth != 0 {
                    return Err("incomplete dictionary XML".into());
                }
                break;
            }
            _ => {}
        }
        match event {
            Event::Start(element) | Event::Empty(element)
                if element.name().as_ref() == b"f" || element.name().as_ref() == b"l" =>
            {
                for attribute in element.attributes() {
                    let attribute = attribute?;
                    if attribute.key.as_ref() == b"t" {
                        let value = attribute.decode_and_unescape_value(reader.decoder())?;
                        if value.len() > 65536 {
                            return Err("dictionary word exceeds bound".into());
                        }
                        let word = normalize(&value);
                        if is_supported_external_word(&word) {
                            if connection
                                .execute("INSERT OR IGNORE INTO words VALUES (?)", [word])?
                                == 0
                            {
                                report.duplicate_forms += 1;
                            }
                        } else {
                            report.unsupported_forms += 1;
                        }
                    }
                }
            }
            Event::DocType(_) => return Err("dictionary DTDs are not permitted".into()),
            _ => {}
        }
        buffer.clear();
    }
    connection.execute_batch("COMMIT;")?;
    let mut writer = BufWriter::new(File::create(output)?);
    let mut statement =
        connection.prepare("SELECT word FROM words ORDER BY word COLLATE BINARY")?;
    for row in statement.query_map([], |row| row.get::<_, String>(0))? {
        writeln!(writer, "{}", row?)?;
        report.wordforms += 1;
    }
    writer.flush()?;
    Ok(report)
}
