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

pub mod exclusions;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SourceFilterReport {
    pub exclusion_index_sha256: String,
    pub source_seal_sha256: String,
    pub review_only: bool,
    pub exact_originals: u64,
    pub near_originals: u64,
    pub privacy_originals: u64,
    pub excluded_ids: BTreeMap<String, Vec<String>>,
}

fn privacy_reasons(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut reasons = Vec::new();
    if lower.split_whitespace().any(|token| {
        token.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && domain.chars().any(char::is_alphanumeric)
        })
    }) {
        reasons.push("direct-email".into());
    }
    if lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("www.")
        || lower
            .split_whitespace()
            .filter(|t| !t.contains('@'))
            .any(|token| {
                let host = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
                host.rsplit_once('.').is_some_and(|(name, tld)| {
                    !name.is_empty()
                        && name.bytes().any(|b| b.is_ascii_alphanumeric())
                        && (2..=24).contains(&tld.len())
                        && tld.bytes().all(|b| b.is_ascii_alphabetic())
                })
            })
    {
        reasons.push("direct-url".into());
    }
    let mut digits = 0;
    for c in lower.chars().chain(std::iter::once('x')) {
        if c.is_numeric() {
            digits += 1;
        } else if !c.is_whitespace() && !"+()-./‐‑‒–—＋（）．".contains(c) {
            if digits >= 7 {
                reasons.push("phone-like-identifier".into());
                break;
            }
            digits = 0;
        }
    }
    reasons
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceProfile {
    TatoebaDetailedCcBy2Fr,
    TatoebaCc0,
}

impl SourceProfile {
    pub(crate) fn terms(self) -> (&'static str, &'static str, &'static str, usize) {
        match self {
            Self::TatoebaDetailedCcBy2Fr => (
                "CC-BY-2.0-FR",
                "https://creativecommons.org/licenses/by/2.0/fr/",
                "https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_detailed.tsv.bz2",
                6,
            ),
            Self::TatoebaCc0 => (
                "CC0-1.0",
                "https://creativecommons.org/publicdomain/zero/1.0/",
                "https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_CC0.tsv.bz2",
                4,
            ),
        }
    }
}

/// A checked input identity, not legal approval or an assertion of content safety.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    pub profile: SourceProfile,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub license: String,
    pub license_url: String,
    pub evidence_url: String,
    pub evidence_sha256: String,
}

impl SourceEvidence {
    pub fn verify(&self, raw: &Path) -> Result<()> {
        let (license, license_url, url, _) = self.profile.terms();
        let is_hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.license != license
            || self.license_url != license_url
            || self.url != url
            || self.evidence_url != "https://tatoeba.org/en/downloads"
            || !is_hash(&self.sha256)
            || !is_hash(&self.evidence_sha256)
            || self.bytes == 0
        {
            return Err("source profile/license/evidence mismatch".into());
        }
        if fs::metadata(raw)?.len() != self.bytes || crate::pipeline::hash_file(raw)? != self.sha256
        {
            return Err("source bytes/hash mismatch before decoding".into());
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrepareReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_filter: Option<SourceFilterReport>,
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
    prepare_impl(raw, output, holdout, None, false)
}

pub fn prepare_with_source(
    raw: &Path,
    output: &Path,
    holdout: &Path,
    source: &SourceEvidence,
) -> Result<PrepareReport> {
    source.verify(raw)?;
    prepare_impl(raw, output, holdout, Some(source), false)
}

pub fn review_with_source(
    raw: &Path,
    output: &Path,
    exclusions: &Path,
    source: &SourceEvidence,
) -> Result<PrepareReport> {
    source.verify(raw)?;
    prepare_impl(raw, output, exclusions, Some(source), true)
}

fn prepare_impl(
    raw: &Path,
    output: &Path,
    holdout: &Path,
    source: Option<&SourceEvidence>,
    review_only: bool,
) -> Result<PrepareReport> {
    let matcher = source
        .map(|_| exclusions::Matcher::read(holdout))
        .transpose()?;
    let source_filter = matcher
        .as_ref()
        .map(|matcher| -> Result<_> {
            Ok(SourceFilterReport {
                exclusion_index_sha256: crate::pipeline::hash_file(holdout)?,
                source_seal_sha256: matcher.source_seal_sha256.clone(),
                review_only,
                ..Default::default()
            })
        })
        .transpose()?;
    let profile = source.map_or(SourceProfile::TatoebaDetailedCcBy2Fr, |s| s.profile);
    let (license, license_url, _, columns_expected) = profile.terms();
    fs::create_dir(output)?;
    let conn = Connection::open(output.join("families.sqlite"))?;
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;
        CREATE TABLE holdout(family TEXT PRIMARY KEY);
        CREATE TABLE blocked(sentence_id TEXT PRIMARY KEY);
        CREATE TABLE input_segments(family TEXT NOT NULL,sentence_id TEXT NOT NULL,segment INTEGER NOT NULL,author TEXT NOT NULL,modified TEXT NOT NULL);
        CREATE TABLE families(id INTEGER PRIMARY KEY,family TEXT UNIQUE NOT NULL,component TEXT,split TEXT);
        CREATE TABLE attribution(family TEXT NOT NULL,sentence_id TEXT NOT NULL,segment INTEGER NOT NULL,author TEXT NOT NULL,modified TEXT NOT NULL);
        CREATE TABLE frequencies(word TEXT PRIMARY KEY,count INTEGER NOT NULL); BEGIN;")?;
    let mut line = String::new();
    if source.is_none() {
        let mut hold = BufReader::new(File::open(holdout)?);
        while bounded_line(&mut hold, &mut line)? {
            for segment in line.split(['.', '!', '?']) {
                let key = family(segment);
                if !key.is_empty() {
                    conn.execute("INSERT OR IGNORE INTO holdout VALUES (?)", [key])?;
                }
            }
        }
    }
    let file = File::open(raw)?;
    let input: Box<dyn Read> = if raw.extension().is_some_and(|e| e == "bz2") {
        Box::new(BzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::new(input.take(if source.is_some() {
        64 * 1024 * 1024 + 1
    } else {
        u64::MAX
    }));
    let mut report = PrepareReport {
        grouping_version: GROUPING_VERSION.into(),
        source: source.cloned(),
        source_filter,
        ..Default::default()
    };
    {
        let mut insert = conn.prepare("INSERT INTO input_segments VALUES (?,?,?,?,?)")?;
        let mut matched = conn.prepare("SELECT EXISTS(SELECT 1 FROM holdout WHERE family=?)")?;
        let mut decoded = 0;
        while bounded_line(&mut reader, &mut line)? {
            report.input_rows += 1;
            decoded += line.len();
            if source.is_some() && (decoded > 64 * 1024 * 1024 || report.input_rows > 100000) {
                return Err("bounded source review exceeds 64 MiB or 100000 rows".into());
            }
            let columns: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
            if columns.len() != columns_expected {
                if source.is_some() {
                    return Err("source profile/schema mismatch".into());
                }
                report.malformed += 1;
                continue;
            }
            if source.is_some()
                && (columns[0].is_empty()
                    || columns[0].len() > 20
                    || !columns[0].bytes().all(|b| b.is_ascii_digit())
                    || columns[0].parse::<u64>().is_err())
            {
                return Err("invalid Tatoeba sentence ID".into());
            }
            if columns[1] != "rus" {
                report.non_russian += 1;
                continue;
            }
            let author = if profile == SourceProfile::TatoebaCc0 {
                ""
            } else {
                columns[3]
            };
            let modified = columns[columns_expected - 1];
            if profile == SourceProfile::TatoebaDetailedCcBy2Fr
                && (author.trim().is_empty() || author == "\\N")
            {
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
            if let (Some(matcher), Some(filter)) = (&matcher, &mut report.source_filter) {
                let mut reasons = privacy_reasons(columns[2]);
                for text in std::iter::once(columns[2]).chain(columns[2].split(['.', '!', '?'])) {
                    if let Some(reason) = matcher.matches(&family(text)) {
                        reasons.push(reason.into());
                    }
                }
                reasons.sort();
                reasons.dedup();
                if !reasons.is_empty() {
                    let entry = filter.excluded_ids.entry(columns[0].into()).or_default();
                    entry.extend(reasons);
                    entry.sort();
                    entry.dedup();
                    conn.execute("INSERT OR IGNORE INTO blocked VALUES (?)", [columns[0]])?;
                }
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
                let held: bool = source.is_none() && matched.query_row([&key], |row| row.get(0))?;
                if held {
                    report.holdout_matches += 1;
                    conn.execute("INSERT OR IGNORE INTO blocked VALUES (?)", [columns[0]])?;
                }
                insert.execute(params![
                    key,
                    columns[0],
                    i64::try_from(segment)?,
                    author,
                    modified
                ])?;
            }
        }
    }
    conn.execute_batch("CREATE INDEX input_original ON input_segments(sentence_id);")?;
    conn.execute("INSERT INTO attribution SELECT s.family,s.sentence_id,s.segment,s.author,s.modified FROM input_segments s WHERE ? OR NOT EXISTS(SELECT 1 FROM blocked b WHERE b.sentence_id=s.sentence_id)", [source.is_some()])?;
    conn.execute_batch("CREATE INDEX attribution_family ON attribution(family);
        INSERT INTO families(family) SELECT DISTINCT family FROM attribution ORDER BY family COLLATE BINARY;")?;
    if source.is_none() {
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
    }
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
    drop(components);
    if let Some(filter) = &mut report.source_filter {
        for reasons in filter.excluded_ids.values_mut() {
            if reasons.iter().any(|r| r == "eval-exact") {
                reasons.retain(|r| r != "eval-near");
            }
        }
        filter.exact_originals = filter
            .excluded_ids
            .values()
            .filter(|r| r.iter().any(|s| s == "eval-exact"))
            .count() as u64;
        filter.near_originals = filter
            .excluded_ids
            .values()
            .filter(|r| r.iter().any(|s| s == "eval-near"))
            .count() as u64;
        filter.privacy_originals = filter
            .excluded_ids
            .values()
            .filter(|r| {
                r.iter()
                    .any(|s| s.starts_with("direct-") || s == "phone-like-identifier")
            })
            .count() as u64;
        conn.execute_batch("CREATE TABLE rejected_components(component TEXT PRIMARY KEY);
            INSERT INTO rejected_components SELECT DISTINCT f.component FROM attribution a JOIN families f USING(family) JOIN blocked b USING(sentence_id);
            INSERT OR IGNORE INTO blocked SELECT DISTINCT a.sentence_id FROM attribution a JOIN families f USING(family) JOIN rejected_components USING(component);")?;
        {
            let mut statement =
                conn.prepare("SELECT sentence_id FROM blocked ORDER BY sentence_id")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                filter
                    .excluded_ids
                    .entry(row.get(0)?)
                    .or_insert_with(|| vec!["component-connected".into()]);
            }
        }
        conn.execute_batch("DELETE FROM attribution WHERE family IN (SELECT family FROM families JOIN rejected_components USING(component));
            CREATE TABLE retained AS SELECT family,component,split FROM families WHERE component NOT IN (SELECT component FROM rejected_components);
            DELETE FROM families;
            INSERT INTO families(family,component,split) SELECT family,component,split FROM retained ORDER BY family COLLATE BINARY;
            DROP TABLE retained;")?;
    }
    report.unique_families =
        conn.query_row("SELECT COUNT(*) FROM families", [], |r| r.get::<_, i64>(0))? as u64;
    let admitted = conn.query_row("SELECT COUNT(*) FROM attribution", [], |r| {
        r.get::<_, i64>(0)
    })? as u64;
    report.duplicate_records = admitted
        .checked_sub(report.unique_families)
        .ok_or("invalid family counters")?;
    conn.execute_batch("COMMIT; BEGIN;")?;
    for name in ["train", "dev", "test"] {
        let count = conn.query_row("SELECT COUNT(*) FROM families WHERE split=?", [name], |r| {
            r.get::<_, i64>(0)
        })? as u64;
        let groups = conn.query_row(
            "SELECT COUNT(DISTINCT component) FROM families WHERE split=?",
            [name],
            |r| r.get::<_, i64>(0),
        )? as u64;
        report.family_counts.insert(name.into(), count);
        report.independent_groups.insert(name.into(), groups);
    }
    report.train = report.family_counts["train"];
    report.dev = report.family_counts["dev"];
    report.test = report.family_counts["test"];
    report.original_overlap=conn.query_row("SELECT COUNT(*) FROM (SELECT a.sentence_id FROM attribution a JOIN families f USING(family) GROUP BY a.sentence_id HAVING COUNT(DISTINCT f.split)>1)",[],|r|r.get::<_,i64>(0))? as u64;
    report.component_overlap=conn.query_row("SELECT COUNT(*) FROM (SELECT component FROM families GROUP BY component HAVING COUNT(DISTINCT split)>1)",[],|r|r.get::<_,i64>(0))? as u64;
    report.split_overlap=conn.query_row("SELECT COUNT(*) FROM (SELECT family FROM families GROUP BY family HAVING COUNT(DISTINCT split)>1)",[],|r|r.get::<_,i64>(0))? as u64;
    if report.original_overlap + report.component_overlap + report.split_overlap != 0 {
        return Err("split isolation invariant violated".into());
    }
    if review_only {
        conn.execute_batch("COMMIT;")?;
        return finish_report(output, report);
    }
    for name in ["train", "dev", "test"] {
        let mut text_out = BufWriter::new(File::create(output.join(format!("{name}.txt")))?);
        let mut group_out =
            BufWriter::new(File::create(output.join(format!("{name}.groups.tsv")))?);
        let mut statement = conn.prepare(
            "SELECT family,component FROM families WHERE split=? ORDER BY family COLLATE BINARY",
        )?;
        let mut rows = statement.query([name])?;
        while let Some(row) = rows.next()? {
            let text: String = row.get(0)?;
            let component: String = row.get(1)?;
            writeln!(text_out, "{text}")?;
            writeln!(
                group_out,
                "{:x}\t{component}",
                Sha256::digest(text.as_bytes())
            )?;
            if name == "train" {
                for word in text.split_whitespace() {
                    conn.execute("INSERT INTO frequencies VALUES (?,1) ON CONFLICT(word) DO UPDATE SET count=count+1",[word])?;
                }
            }
        }
        text_out.flush()?;
        group_out.flush()?;
    }
    conn.execute_batch("COMMIT;")?;
    let mut out = BufWriter::new(File::create(output.join("attribution.jsonl"))?);
    let mut statement=conn.prepare("SELECT a.family,a.sentence_id,a.segment,a.author,f.split,f.component,a.modified FROM attribution a JOIN families f USING(family) ORDER BY a.family,a.sentence_id,a.segment,a.author,a.modified")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(1)?;
        let mut credit = serde_json::json!({"source":"tatoeba","family":row.get::<_,String>(0)?,"sentence_id":id,"segment":row.get::<_,i64>(2)?,"author":row.get::<_,String>(3)?,"split":row.get::<_,String>(4)?,"component":row.get::<_,String>(5)?,"source_url":format!("https://tatoeba.org/en/sentences/show/{id}"),"license":license,"license_url":license_url});
        if let Some(source) = source {
            credit["source_profile"] = serde_json::to_value(source.profile)?;
            credit["source_sha256"] = source.sha256.clone().into();
            credit["modified"] = row.get::<_, String>(6)?.into();
            if profile == SourceProfile::TatoebaCc0 {
                credit["author"] = serde_json::Value::Null;
            }
        }
        serde_json::to_writer(&mut out, &credit)?;
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
    finish_report(output, report)
}

fn finish_report(output: &Path, mut report: PrepareReport) -> Result<PrepareReport> {
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
    serde_json::to_writer_pretty(File::create(output.join("report.json"))?, &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifier_screen_handles_unicode_without_banning_need_or_pain_words() {
        assert!(privacy_reasons("почта тест@пример.испытание").contains(&"direct-email".into()));
        assert!(
            privacy_reasons("+7\u{202f}000\u{202f}000\u{2011}00\u{2011}00")
                .contains(&"phone-like-identifier".into())
        );
        assert!(privacy_reasons("мне больно, нужна помощь и вода").is_empty());
    }
    #[test]
    fn review_excludes_hash_overlap_identifiers_and_entire_transitive_components_without_text_output(
    ) {
        use exclusions::{make_index, CuratorRecord};
        let temp = tempfile::tempdir().unwrap();
        let raw = temp.path().join("cc0.tsv");
        let index = temp.path().join("exclusions.json");
        let data = [
            "аа бб вв гг дд ее. связка.",
            "связка. хвост.",
            "хвост. уцелевшее.",
            "Напиши test@example.com. мост.",
            "мост. ещё хвост.",
            "аа бб вв гг дд ее жж.",
            "мне больно и нужна помощь.",
            "хочу пить.",
            "+7 (999) 123-45-67",
            "https://example.org",
        ];
        fs::write(
            &raw,
            data.iter()
                .enumerate()
                .map(|(n, text)| format!("{}\trus\t{text}\t2026\n", n + 1))
                .collect::<String>(),
        )
        .unwrap();
        let manifest = make_index(
            &[CuratorRecord {
                id: "eval-1".into(),
                group_id: "g-1".into(),
                text: "аа бб вв гг дд ее".into(),
            }],
            &"a".repeat(64),
        )
        .unwrap();
        fs::write(&index, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../provenance/tatoeba-cc0-candidate.json"
        ))
        .unwrap();
        let mut source: SourceEvidence = serde_json::from_value(value["source"].clone()).unwrap();
        source.bytes = fs::metadata(&raw).unwrap().len();
        source.sha256 = crate::pipeline::hash_file(&raw).unwrap();
        let review = temp.path().join("review");
        let report = review_with_source(&raw, &review, &index, &source).unwrap();
        let filter = report.source_filter.as_ref().unwrap();
        assert_eq!(filter.exact_originals, 1);
        assert_eq!(filter.near_originals, 1);
        assert_eq!(filter.privacy_originals, 3);
        assert_eq!(filter.excluded_ids.len(), 8);
        assert_eq!(filter.excluded_ids["2"], vec!["component-connected"]);
        assert_eq!(filter.excluded_ids["5"], vec!["component-connected"]);
        assert_eq!(report.unique_families, 2);
        assert!(!review.join("train.txt").exists());
        assert!(!review.join("attribution.jsonl").exists());
        let serialized = fs::read_to_string(review.join("report.json")).unwrap();
        assert!(!serialized.contains("example") && !serialized.contains("больно"));
        let output = temp.path().join("prepared");
        prepare_with_source(&raw, &output, &index, &source).unwrap();
        let retained = ["train", "dev", "test"]
            .map(|n| fs::read_to_string(output.join(format!("{n}.txt"))).unwrap())
            .join("");
        assert!(retained.contains("мне больно"));
        assert!(!retained.contains("уцелевшее"));
        let mut invalid = manifest;
        invalid.normalizer = "unknown".into();
        fs::write(&index, serde_json::to_vec(&invalid).unwrap()).unwrap();
        let bad = temp.path().join("bad-index");
        assert!(prepare_with_source(&raw, &bad, &index, &source).is_err());
        assert!(!bad.exists());
    }

    #[test]
    fn explicit_source_profiles_bind_schema_license_and_bytes_before_preparing() {
        let temp = tempfile::tempdir().unwrap();
        let raw = temp.path().join("cc0.tsv");
        let hold = temp.path().join("hold");
        fs::write(&raw, "1\trus\tЕщё чай.\t2026-10-31 00:00:00\n").unwrap();
        fs::write(
            &hold,
            serde_json::to_vec(&exclusions::make_index(&[], &"a".repeat(64)).unwrap()).unwrap(),
        )
        .unwrap();
        let mut source = SourceEvidence {
            profile: SourceProfile::TatoebaCc0,
            url: "https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_CC0.tsv.bz2"
                .into(),
            sha256: crate::pipeline::hash_file(&raw).unwrap(),
            bytes: fs::metadata(&raw).unwrap().len(),
            license: "CC0-1.0".into(),
            license_url: "https://creativecommons.org/publicdomain/zero/1.0/".into(),
            evidence_url: "https://tatoeba.org/en/downloads".into(),
            evidence_sha256: "a".repeat(64),
        };
        let output = temp.path().join("cc0");
        let report = prepare_with_source(&raw, &output, &hold, &source).unwrap();
        assert_eq!(report.unique_families, 1);
        assert_eq!(report.missing_author, 0);
        let attribution = fs::read_to_string(output.join("attribution.jsonl")).unwrap();
        let row: serde_json::Value = serde_json::from_str(attribution.trim()).unwrap();
        assert_eq!(row["license"], "CC0-1.0");
        assert!(row["author"].is_null());
        assert_eq!(row["modified"], "2026-10-31 00:00:00");
        assert_eq!(row["family"], "ещё чай");
        assert_eq!(report.source.unwrap().sha256, source.sha256);
        let compact = temp.path().join("credits.bz2");
        crate::pipeline::compact_attribution(&output, &compact).unwrap();
        let mut credits = String::new();
        BzDecoder::new(File::open(compact).unwrap())
            .read_to_string(&mut credits)
            .unwrap();
        let mut lines = credits.lines();
        let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["sources"][0]["license"], "CC0-1.0");
        let record: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert!(record[1].is_null());

        let size = source.bytes;
        source.bytes += 1;
        let bad = temp.path().join("wrong-size");
        assert!(prepare_with_source(&raw, &bad, &hold, &source).is_err());
        assert!(!bad.exists());
        source.bytes = size;

        source.license = "CC-BY-2.0-FR".into();
        let bad = temp.path().join("wrong-license");
        assert!(prepare_with_source(&raw, &bad, &hold, &source).is_err());
        assert!(!bad.exists());
        source.license = "CC0-1.0".into();
        source.sha256 = "0".repeat(64);
        let bad = temp.path().join("wrong-hash");
        assert!(prepare_with_source(&raw, &bad, &hold, &source).is_err());
        assert!(!bad.exists());
        source.sha256 = crate::pipeline::hash_file(&raw).unwrap();
        fs::write(&raw, "1\trus\tчай\tanna\t2026\t2026\n").unwrap();
        source.bytes = fs::metadata(&raw).unwrap().len();
        source.sha256 = crate::pipeline::hash_file(&raw).unwrap();
        assert!(
            prepare_with_source(&raw, &temp.path().join("wrong-schema"), &hold, &source)
                .unwrap_err()
                .to_string()
                .contains("source profile/schema mismatch")
        );
        assert!(serde_json::from_str::<SourceEvidence>("{}").is_err());
    }

    #[test]
    fn explicit_detailed_profile_preserves_authors_and_train_only_counts() {
        let temp = tempfile::tempdir().unwrap();
        let raw = temp.path().join("detailed.tsv");
        let hold = temp.path().join("hold");
        fs::write(
            &hold,
            serde_json::to_vec(&exclusions::make_index(&[], &"a".repeat(64)).unwrap()).unwrap(),
        )
        .unwrap();
        let rows: String = (0..200)
            .map(|i| {
                format!(
                    "{i}\trus\tслово{}{}\tfixture\t2026\t2026\n",
                    char::from_u32(0x430 + i / 20).unwrap(),
                    char::from_u32(0x430 + i % 20).unwrap()
                )
            })
            .collect();
        fs::write(&raw, rows).unwrap();
        let source = SourceEvidence {
            profile: SourceProfile::TatoebaDetailedCcBy2Fr,
            url: "https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_detailed.tsv.bz2".into(),
            sha256: crate::pipeline::hash_file(&raw).unwrap(),
            bytes: fs::metadata(&raw).unwrap().len(),
            license: "CC-BY-2.0-FR".into(),
            license_url: "https://creativecommons.org/licenses/by/2.0/fr/".into(),
            evidence_url: "https://tatoeba.org/en/downloads".into(),
            evidence_sha256: "a".repeat(64),
        };
        let output = temp.path().join("prepared");
        let report = prepare_with_source(&raw, &output, &hold, &source).unwrap();
        assert!(report.independent_groups["dev"] > 0);
        assert!(report.independent_groups["test"] > 0);
        assert_eq!(
            report.original_overlap + report.component_overlap + report.split_overlap,
            0
        );
        let train = fs::read_to_string(output.join("train.txt")).unwrap();
        let expected: BTreeMap<String, u32> = train.lines().map(|word| (word.into(), 1)).collect();
        assert_eq!(
            crate::pipeline::word_counts(&output.join("frequencies.tsv")).unwrap(),
            expected
        );
        for line in fs::read_to_string(output.join("attribution.jsonl"))
            .unwrap()
            .lines()
        {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(row["author"], "fixture");
            assert_eq!(row["license"], "CC-BY-2.0-FR");
        }
    }

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
