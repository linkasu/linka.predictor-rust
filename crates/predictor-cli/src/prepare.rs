use bzip2::read::BzDecoder;
use predictor_core::{tokenize, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::Path,
};

pub const GROUPING_VERSION: &str = "duplicate-original-components-v1";
const DSU_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrepareReport {
    pub input_rows: u64,
    pub malformed: u64,
    pub non_russian: u64,
    pub missing_author: u64,
    pub reserved_markers: u64,
    pub holdout_excluded: u64,
    pub holdout_matches: u64,
    pub holdout_related_excluded: u64,
    pub holdout_original_records: u64,
    pub duplicate_records: u64,
    pub unique_families: u64,
    pub train: u64,
    pub dev: u64,
    pub test: u64,
    pub split_overlap: u64,
    pub original_overlap: u64,
    pub component_overlap: u64,
    pub grouping_version: String,
    pub independent_groups: BTreeMap<String, u64>,
    pub family_counts: BTreeMap<String, u64>,
    pub dsu_reserved_bytes: usize,
    pub max_token_bytes: usize,
    pub oversize_tokens: u64,
    pub process_peak_rss_bytes: u64,
    pub split_sha256: BTreeMap<String, String>,
}

pub fn family(text: &str) -> String {
    tokenize(text)
        .into_iter()
        .map(|token| token.normalized)
        .collect::<Vec<_>>()
        .join(" ")
}

fn hex(bytes: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

fn split(component: &str) -> &'static str {
    let mut digest = Sha256::new();
    digest.update(b"linka-components-v1\0");
    digest.update(component.as_bytes());
    let hash = digest.finalize();
    match u64::from_le_bytes(hash[..8].try_into().unwrap()) % 100 {
        0..=89 => "train",
        90..=94 => "dev",
        _ => "test",
    }
}

// Negative roots store set size; minima hold the minimum member SHA-256.
struct Components {
    parents: Vec<i32>,
    minima: Vec<[u8; 32]>,
    maximum: usize,
}
impl Components {
    fn new(limit: usize) -> Result<Self> {
        let maximum = limit.saturating_sub(std::mem::size_of::<Self>()) / 36;
        if maximum == 0 || maximum > i32::MAX as usize {
            return Err("invalid component DSU budget".into());
        }
        let mut result = Self {
            parents: Vec::new(),
            minima: Vec::new(),
            maximum,
        };
        result.parents.try_reserve_exact(maximum)?;
        result.minima.try_reserve_exact(maximum)?;
        if result.reserved_bytes() > limit {
            return Err("component DSU allocation exceeds budget".into());
        }
        Ok(result)
    }
    fn reserved_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.parents.capacity() * 4 + self.minima.capacity() * 32
    }
    fn push(&mut self, hash: [u8; 32]) -> Result<()> {
        if self.parents.len() == self.maximum {
            return Err("component DSU budget exhausted; no families dropped".into());
        }
        self.parents.push(-1);
        self.minima.push(hash);
        Ok(())
    }
    fn root(&mut self, mut id: usize) -> usize {
        let start = id;
        while self.parents[id] >= 0 {
            id = self.parents[id] as usize;
        }
        let root = id;
        id = start;
        while self.parents[id] >= 0 {
            let next = self.parents[id] as usize;
            self.parents[id] = root as i32;
            id = next;
        }
        root
    }
    fn union(&mut self, left: usize, right: usize) {
        let (mut a, mut b) = (self.root(left), self.root(right));
        if a == b {
            return;
        }
        if self.parents[a] > self.parents[b] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parents[a] += self.parents[b];
        self.parents[b] = a as i32;
        self.minima[a] = self.minima[a].min(self.minima[b]);
    }
}

pub(crate) fn bounded_line(reader: &mut dyn BufRead, line: &mut String) -> Result<bool> {
    line.clear();
    let n = reader.take(65537).read_line(line)?;
    if n > 65536 {
        return Err("source line exceeds 64 KiB".into());
    }
    Ok(n != 0)
}

/// Disk-backed records/credits plus a fixed-cap integer DSU; never merges LM sentences.
pub fn prepare(raw: &Path, output: &Path, holdout: &Path) -> Result<PrepareReport> {
    fs::create_dir(output)?;
    let conn = Connection::open(output.join("families.sqlite"))?;
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;
        CREATE TABLE holdout(family TEXT PRIMARY KEY);
        CREATE TABLE blocked(sentence_id TEXT PRIMARY KEY);
        CREATE TABLE input_segments(family TEXT NOT NULL,sentence_id TEXT NOT NULL,segment INTEGER NOT NULL,author TEXT NOT NULL);
        CREATE TABLE families(id INTEGER PRIMARY KEY,family TEXT UNIQUE NOT NULL,component TEXT,split TEXT);
        CREATE TABLE attribution(family TEXT NOT NULL,sentence_id TEXT NOT NULL,segment INTEGER NOT NULL,author TEXT NOT NULL);
        CREATE TABLE frequencies(word TEXT PRIMARY KEY,count INTEGER NOT NULL); BEGIN;")?;
    let mut line = String::new();
    let mut hold = BufReader::new(File::open(holdout)?);
    while bounded_line(&mut hold, &mut line)? {
        for segment in line.split(['.', '!', '?']) {
            let key = family(segment);
            if !key.is_empty() {
                conn.execute("INSERT OR IGNORE INTO holdout VALUES (?)", [key])?;
            }
        }
    }
    let file = File::open(raw)?;
    let input: Box<dyn Read> = if raw.extension().is_some_and(|e| e == "bz2") {
        Box::new(BzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::new(input);
    let mut report = PrepareReport {
        grouping_version: GROUPING_VERSION.into(),
        ..Default::default()
    };
    {
        let mut insert = conn.prepare("INSERT INTO input_segments VALUES (?,?,?,?)")?;
        let mut matched = conn.prepare("SELECT EXISTS(SELECT 1 FROM holdout WHERE family=?)")?;
        while bounded_line(&mut reader, &mut line)? {
            report.input_rows += 1;
            let columns: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
            if columns.len() != 6 {
                report.malformed += 1;
                continue;
            }
            if columns[1] != "rus" {
                report.non_russian += 1;
                continue;
            }
            if columns[3].trim().is_empty() || columns[3] == "\\N" {
                report.missing_author += 1;
                continue;
            }
            if ["<s>", "</s>", "<unk>"]
                .iter()
                .any(|marker| columns[2].contains(marker))
            {
                report.reserved_markers += 1;
                continue;
            }
            for (segment, text) in columns[2].split(['.', '!', '?']).enumerate() {
                let key = family(text);
                if key.is_empty() {
                    continue;
                }
                for word in key.split_whitespace() {
                    report.max_token_bytes = report.max_token_bytes.max(word.len());
                    report.oversize_tokens += u64::from(word.len() > 16384);
                }
                let held: bool = matched.query_row([&key], |row| row.get(0))?;
                if held {
                    report.holdout_matches += 1;
                    conn.execute("INSERT OR IGNORE INTO blocked VALUES (?)", [columns[0]])?;
                }
                insert.execute(params![
                    key,
                    columns[0],
                    i64::try_from(segment)?,
                    columns[3]
                ])?;
            }
        }
    }
    conn.execute_batch("CREATE INDEX input_original ON input_segments(sentence_id);
        INSERT INTO attribution SELECT s.family,s.sentence_id,s.segment,s.author FROM input_segments s WHERE NOT EXISTS(SELECT 1 FROM blocked b WHERE b.sentence_id=s.sentence_id);
        CREATE INDEX attribution_family ON attribution(family);
        INSERT INTO families(family) SELECT DISTINCT family FROM attribution ORDER BY family COLLATE BINARY;")?;
    report.holdout_excluded = conn.query_row(
        "SELECT COUNT(*) FROM input_segments s JOIN blocked b USING(sentence_id)",
        [],
        |r| r.get::<_, i64>(0),
    )? as u64;
    report.holdout_related_excluded = report
        .holdout_excluded
        .checked_sub(report.holdout_matches)
        .ok_or("invalid holdout counters")?;
    report.holdout_original_records =
        conn.query_row("SELECT COUNT(*) FROM blocked", [], |r| r.get::<_, i64>(0))? as u64;
    let mut components = Components::new(DSU_LIMIT)?;
    report.dsu_reserved_bytes = components.reserved_bytes();
    {
        let mut statement = conn.prepare("SELECT id,family FROM families ORDER BY id")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            if usize::try_from(id)? != components.parents.len() + 1 {
                return Err("non-contiguous family IDs".into());
            }
            let text: String = row.get(1)?;
            components.push(Sha256::digest(text.as_bytes()).into())?;
        }
    }
    {
        let mut statement=conn.prepare("SELECT DISTINCT a.sentence_id,f.id FROM attribution a JOIN families f USING(family) ORDER BY a.sentence_id,f.id")?;
        let mut rows = statement.query([])?;
        let mut previous = None;
        let mut anchor = 0;
        while let Some(row) = rows.next()? {
            let original: String = row.get(0)?;
            let id = usize::try_from(row.get::<_, i64>(1)?)? - 1;
            if previous.as_ref() == Some(&original) {
                components.union(anchor, id);
            } else {
                anchor = id;
                previous = Some(original);
            }
        }
    }
    {
        let mut update = conn.prepare("UPDATE families SET component=?,split=? WHERE id=?")?;
        for id in 0..components.parents.len() {
            let root = components.root(id);
            let component = hex(&components.minima[root]);
            update.execute(params![
                component,
                split(&component),
                i64::try_from(id + 1)?
            ])?;
        }
    }
    report.unique_families = components.parents.len() as u64;
    drop(components);
    let admitted = conn.query_row("SELECT COUNT(*) FROM attribution", [], |r| {
        r.get::<_, i64>(0)
    })? as u64;
    report.duplicate_records = admitted
        .checked_sub(report.unique_families)
        .ok_or("invalid family counters")?;
    conn.execute_batch("COMMIT; BEGIN;")?;
    for name in ["train", "dev", "test"] {
        let mut text_out = BufWriter::new(File::create(output.join(format!("{name}.txt")))?);
        let mut group_out =
            BufWriter::new(File::create(output.join(format!("{name}.groups.tsv")))?);
        let mut statement = conn.prepare(
            "SELECT family,component FROM families WHERE split=? ORDER BY family COLLATE BINARY",
        )?;
        let mut rows = statement.query([name])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let text: String = row.get(0)?;
            let component: String = row.get(1)?;
            writeln!(text_out, "{text}")?;
            writeln!(
                group_out,
                "{:x}\t{component}",
                Sha256::digest(text.as_bytes())
            )?;
            count += 1;
            if name == "train" {
                for word in text.split_whitespace() {
                    conn.execute("INSERT INTO frequencies VALUES (?,1) ON CONFLICT(word) DO UPDATE SET count=count+1",[word])?;
                }
            }
        }
        text_out.flush()?;
        group_out.flush()?;
        report.family_counts.insert(name.into(), count);
        let groups = conn.query_row(
            "SELECT COUNT(DISTINCT component) FROM families WHERE split=?",
            [name],
            |r| r.get::<_, i64>(0),
        )? as u64;
        report.independent_groups.insert(name.into(), groups);
    }
    conn.execute_batch("COMMIT;")?;
    report.train = report.family_counts["train"];
    report.dev = report.family_counts["dev"];
    report.test = report.family_counts["test"];
    report.original_overlap=conn.query_row("SELECT COUNT(*) FROM (SELECT a.sentence_id FROM attribution a JOIN families f USING(family) GROUP BY a.sentence_id HAVING COUNT(DISTINCT f.split)>1)",[],|r|r.get::<_,i64>(0))? as u64;
    report.component_overlap=conn.query_row("SELECT COUNT(*) FROM (SELECT component FROM families GROUP BY component HAVING COUNT(DISTINCT split)>1)",[],|r|r.get::<_,i64>(0))? as u64;
    report.split_overlap=conn.query_row("SELECT COUNT(*) FROM (SELECT family FROM families GROUP BY family HAVING COUNT(DISTINCT split)>1)",[],|r|r.get::<_,i64>(0))? as u64;
    if report.original_overlap + report.component_overlap + report.split_overlap != 0 {
        return Err("split isolation invariant violated".into());
    }
    let mut out = BufWriter::new(File::create(output.join("attribution.jsonl"))?);
    let mut statement=conn.prepare("SELECT a.family,a.sentence_id,a.segment,a.author,f.split,f.component FROM attribution a JOIN families f USING(family) ORDER BY a.family,a.sentence_id,a.segment,a.author")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(1)?;
        serde_json::to_writer(
            &mut out,
            &serde_json::json!({"source":"tatoeba","family":row.get::<_,String>(0)?,"sentence_id":id,"segment":row.get::<_,i64>(2)?,"author":row.get::<_,String>(3)?,"split":row.get::<_,String>(4)?,"component":row.get::<_,String>(5)?,"source_url":format!("https://tatoeba.org/en/sentences/show/{id}"),"license":"CC-BY-2.0-FR","license_url":"https://creativecommons.org/licenses/by/2.0/fr/"}),
        )?;
        writeln!(out)?;
    }
    out.flush()?;
    let mut counts = BufWriter::new(File::create(output.join("frequencies.tsv"))?);
    let mut statement =
        conn.prepare("SELECT word,count FROM frequencies ORDER BY word COLLATE BINARY")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        writeln!(
            counts,
            "{}\t{}",
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?
        )?;
    }
    counts.flush()?;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    report.process_peak_rss_bytes = u64::try_from(usage.ru_maxrss)?;
    #[cfg(not(target_os = "macos"))]
    {
        report.process_peak_rss_bytes = report
            .process_peak_rss_bytes
            .checked_mul(1024)
            .ok_or("RSS overflow")?;
    }
    for name in [
        "train.txt",
        "dev.txt",
        "test.txt",
        "train.groups.tsv",
        "dev.groups.tsv",
        "test.groups.tsv",
    ] {
        report
            .split_sha256
            .insert(name.into(), crate::pipeline::hash_file(&output.join(name))?);
    }
    serde_json::to_writer_pretty(File::create(output.join("report.json"))?, &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dsu_budget_is_hard_and_never_silently_drops_nodes() {
        let cap = std::mem::size_of::<Components>() + 2 * 36;
        let mut dsu = Components::new(cap).unwrap();
        assert!(dsu.reserved_bytes() <= cap);
        dsu.push([2; 32]).unwrap();
        dsu.push([1; 32]).unwrap();
        assert!(dsu
            .push([0; 32])
            .unwrap_err()
            .to_string()
            .contains("budget exhausted"));
        dsu.union(0, 1);
        let root = dsu.root(0);
        assert_eq!(dsu.minima[root], [1; 32]);
        assert_eq!(dsu.root(1), root);
    }
}
