use crate::{normalize, Result};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

const NONE: u32 = u32::MAX;
const HEADER: usize = 32;
const DIR: usize = 56;
const RECORD: usize = 24;
const MAX_PACKAGE: u64 = 500_000_000;
const MAX_WORD_BYTES: usize = 16_384;
const MAX_METADATA_BYTES: usize = 40_000_000;
const POLICY: &[u8] = br#"{"normalizer":"nfc-lower-v1","letter_cost":1.5,"space_cost":1.0,"continuations":128,"max_operations":12000}"#;

/// Admission of an already-normalized external wordform; never a train cutoff.
pub fn is_supported_external_word(word: &str) -> bool {
    !word.is_empty() && word.len() <= MAX_WORD_BYTES && word.chars().all(char::is_alphabetic)
}

// All validation readers share one inode, but never a mutable OS file cursor.
struct PositionedFile {
    file: File,
    position: u64,
}
impl PositionedFile {
    fn new(file: File) -> Self {
        Self { file, position: 0 }
    }
}
impl Read for PositionedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        #[cfg(unix)]
        let n = {
            use std::os::unix::fs::FileExt;
            self.file.read_at(buffer, self.position)?
        };
        #[cfg(windows)]
        let n = {
            use std::os::windows::fs::FileExt;
            self.file.seek_read(buffer, self.position)?
        };
        self.position += n as u64;
        Ok(n)
    }
}
impl Seek for PositionedFile {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let position = match from {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(p) => self.position as i128 + p as i128,
            SeekFrom::End(p) => self.file.metadata()?.len() as i128 + p as i128,
        };
        self.position = u64::try_from(position)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid seek"))?;
        Ok(self.position)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageMetadata {
    pub provenance: serde_json::Value,
    pub attribution: serde_json::Value,
    #[serde(default)]
    pub external_words: Vec<String>,
    #[serde(default)]
    pub word_counts: BTreeMap<String, u32>,
}

#[derive(Clone, Copy)]
struct Section {
    kind: u32,
    offset: usize,
    length: usize,
}

pub struct Model {
    mmap: Mmap,
    sections: Vec<Section>,
    order: usize,
    words: usize,
    tree_base: usize,
    count_total: u64,
}

// A request owns the cache: no retained query history or cross-profile state.
pub(crate) struct Scorer<'a> {
    model: &'a Model,
    ids: BTreeMap<String, u32>,
}
impl Scorer<'_> {
    fn id(&mut self, word: &str) -> u32 {
        if let Some(id) = self.ids.get(word) {
            return *id;
        }
        let id = self.model.lm_id(word);
        self.ids.insert(word.to_owned(), id);
        id
    }
    pub(crate) fn score(&mut self, context: &[&str], word: &str) -> f64 {
        let id = self.id(word);
        let context: Vec<_> = context
            .iter()
            .rev()
            .take(self.model.order - 1)
            .map(|w| self.id(w))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.model.score_ids(&context, id)
    }
    pub(crate) fn frequency_score(&self, word: &str) -> f64 {
        self.model.frequency_score(word)
    }
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}
fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_bits(u32_at(b, i))
}
fn put32(b: &mut Vec<u8>, v: u32) {
    b.extend(v.to_le_bytes());
}
fn put64(b: &mut Vec<u8>, v: u64) {
    b.extend(v.to_le_bytes());
}
fn key(record: &[u8]) -> [u32; 4] {
    std::array::from_fn(|i| u32_at(record, i * 4))
}

fn read_at<const N: usize>(
    reader: &mut BufReader<PositionedFile>,
    offset: usize,
) -> Result<[u8; N]> {
    let current = reader.stream_position()?;
    reader.seek_relative(offset as i64 - current as i64)?;
    let mut buffer = [0; N];
    reader.read_exact(&mut buffer)?;
    Ok(buffer)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or("missing output filename")?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = options.open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

impl Model {
    pub fn compile_arpa(
        input: impl AsRef<Path>,
        output: impl AsRef<Path>,
        metadata: &PackageMetadata,
    ) -> Result<()> {
        let package_metadata = serde_json::to_vec(&BTreeMap::from([
            ("provenance", &metadata.provenance),
            ("attribution", &metadata.attribution),
        ]))?;
        if package_metadata.len() > MAX_METADATA_BYTES {
            return Err("metadata exceeds bound".into());
        }
        let mut rows: Vec<(usize, Vec<String>, f32, f32)> = Vec::new();
        let mut order = 0;
        let mut current = 0;
        let mut ended = false;
        let mut declared = BTreeMap::new();
        for line in BufReader::new(File::open(input)?).lines() {
            let line = line?;
            let line = line.trim();
            if line == "\\end\\" {
                ended = true;
                break;
            }
            if let Some(rest) = line.strip_prefix("ngram ") {
                let (n, c) = rest.split_once('=').ok_or("invalid ARPA count")?;
                declared.insert(n.parse::<usize>()?, c.parse::<usize>()?);
                continue;
            }
            if line.starts_with('\\') {
                current = if let Some(n) = line
                    .strip_prefix('\\')
                    .and_then(|s| s.strip_suffix("-grams:"))
                {
                    n.parse()?
                } else {
                    0
                };
                if current > 4 {
                    return Err("supported model order is 1..4".into());
                }
                order = order.max(current);
                continue;
            }
            if current == 0 || line.is_empty() {
                continue;
            }
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() != current + 1 && fields.len() != current + 2 {
                return Err("invalid ARPA row".into());
            }
            let probability: f32 = fields[0].parse()?;
            let backoff: f32 = fields
                .get(current + 1)
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(0.0);
            if !probability.is_finite() || probability > 0.0 || !backoff.is_finite() {
                return Err("invalid ARPA probability/backoff".into());
            }
            let words: Vec<_> = fields[1..=current].iter().map(|s| normalize(s)).collect();
            if words
                .iter()
                .any(|w| w.len() > MAX_WORD_BYTES || w.is_empty())
            {
                return Err("invalid ARPA word".into());
            }
            rows.push((current, words, probability, backoff));
        }
        if !ended || order == 0 {
            return Err("incomplete ARPA".into());
        }
        for n in 1..=order {
            if declared.get(&n).copied() != Some(rows.iter().filter(|r| r.0 == n).count()) {
                return Err("ARPA count mismatch".into());
            }
        }
        let mut vocabulary: BTreeMap<String, (f32, u32)> = BTreeMap::new();
        for row in &rows {
            if row.0 == 1 && vocabulary.insert(row.1[0].clone(), (row.2, 0)).is_some() {
                return Err("duplicate normalized unigram".into());
            }
        }
        for special in ["<s>", "</s>", "<unk>"] {
            if !vocabulary.contains_key(special) {
                return Err("ARPA missing special token".into());
            }
        }
        let unknown_probability = vocabulary["<unk>"].0;
        for word in &metadata.external_words {
            let word = normalize(word);
            if !is_supported_external_word(&word) {
                return Err("invalid external word".into());
            }
            vocabulary.entry(word).or_insert((unknown_probability, 1));
        }
        let ids: BTreeMap<_, _> = vocabulary
            .keys()
            .enumerate()
            .map(|(i, w)| (w.clone(), i as u32))
            .collect();
        let mut lex = Vec::new();
        let mut strings = Vec::new();
        for (w, (p, flags)) in &vocabulary {
            put64(&mut lex, strings.len() as u64);
            put32(&mut lex, w.len() as u32);
            put32(&mut lex, p.to_bits());
            put32(&mut lex, *flags);
            put32(&mut lex, metadata.word_counts.get(w).copied().unwrap_or(0));
            strings.extend(w.as_bytes());
        }
        let mut sections = vec![(1, lex), (2, strings)];
        let mut continuations: BTreeMap<[u32; 3], Vec<(u32, f32)>> = BTreeMap::new();
        for n in 1..=order {
            let mut table = BTreeMap::new();
            for (_, ws, p, b) in rows.iter().filter(|r| r.0 == n) {
                let mut k = [NONE; 4];
                for (i, w) in ws.iter().enumerate() {
                    k[i] = *ids.get(w).ok_or("ngram word lacks unigram")?;
                }
                if table.insert(k, (*p, *b)).is_some() {
                    return Err("duplicate ngram".into());
                }
                if n > 1 {
                    let mut context = [NONE; 3];
                    context[..n - 1].copy_from_slice(&k[..n - 1]);
                    continuations
                        .entry(context)
                        .or_default()
                        .push((k[n - 1], *p));
                }
            }
            let mut data = Vec::new();
            for (k, (p, b)) in table {
                for id in k {
                    put32(&mut data, id);
                }
                put32(&mut data, p.to_bits());
                put32(&mut data, b.to_bits());
            }
            sections.push((10 + n as u32, data));
        }
        let mut continuation_data = Vec::new();
        for (context, mut words) in continuations {
            words.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            words.truncate(128);
            words.sort_by_key(|w| w.0);
            for (word, p) in words {
                for id in context {
                    put32(&mut continuation_data, id);
                }
                put32(&mut continuation_data, word);
                put32(&mut continuation_data, p.to_bits());
                put32(&mut continuation_data, 0);
            }
        }
        sections.push((3, continuation_data));
        let base = vocabulary.len().next_power_of_two();
        let vocab: Vec<_> = vocabulary.iter().collect();
        let mut tree = vec![NONE; 2 * base];
        for (i, (w, _)) in vocab.iter().enumerate() {
            if !w.starts_with('<') {
                tree[base + i] = i as u32;
            }
        }
        for i in (1..base).rev() {
            tree[i] = better_id(tree[i * 2], tree[i * 2 + 1], |id| {
                let (word, (p, _)) = vocab[id as usize];
                let count = metadata.word_counts.get(word).copied().unwrap_or(0);
                if count > 0 {
                    count as f64
                } else {
                    *p as f64
                }
            });
        }
        let mut tree_data = Vec::new();
        for id in tree {
            put32(&mut tree_data, id);
        }
        sections.push((4, tree_data));
        sections.push((5, package_metadata));
        sections.push((6, POLICY.to_vec()));
        let mut bytes = Vec::new();
        bytes.extend(b"LINKALPK");
        put32(&mut bytes, 1);
        put32(&mut bytes, order as u32);
        put32(&mut bytes, sections.len() as u32);
        put32(&mut bytes, 1);
        bytes.extend(b"ru\0\0");
        put32(&mut bytes, 0);
        let mut offset = HEADER + DIR * sections.len();
        for (kind, data) in &sections {
            put32(&mut bytes, *kind);
            put32(&mut bytes, 0);
            put64(&mut bytes, offset as u64);
            put64(&mut bytes, data.len() as u64);
            bytes.extend(Sha256::digest(data));
            offset += data.len();
        }
        for (_, data) in sections {
            bytes.extend(data);
        }
        if bytes.len() as u64 > MAX_PACKAGE {
            return Err("package exceeds 500 MB".into());
        }
        atomic_write(output.as_ref(), &bytes, false)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path.as_ref())?;
        let length = file.metadata()?.len();
        if !(HEADER as u64..=MAX_PACKAGE).contains(&length) {
            return Err("invalid package size".into());
        }
        let mut reader =
            BufReader::with_capacity(64 * 1024, PositionedFile::new(file.try_clone()?));
        let mut header = [0; HEADER];
        reader.read_exact(&mut header)?;
        if &header[..8] != b"LINKALPK"
            || u32_at(&header, 8) != 1
            || u32_at(&header, 20) != 1
            || &header[24..28] != b"ru\0\0"
            || u32_at(&header, 28) != 0
        {
            return Err("incompatible package header".into());
        }
        let order = u32_at(&header, 12) as usize;
        let count = u32_at(&header, 16) as usize;
        if !(1..=4).contains(&order) || count != order + 6 {
            return Err("invalid directory count/order".into());
        }
        let mut directory = vec![0; count * DIR];
        reader.read_exact(&mut directory)?;
        let mut sections = Vec::new();
        let mut expected = HEADER + directory.len();
        for entry in directory.as_chunks::<DIR>().0 {
            let offset = usize::try_from(u64_at(entry, 8))?;
            let len = usize::try_from(u64_at(entry, 16))?;
            let end = offset.checked_add(len).ok_or("section overflow")?;
            let kind = u32_at(entry, 0);
            if u32_at(entry, 4) != 0
                || offset != expected
                || end as u64 > length
                || sections.iter().any(|s: &Section| s.kind == kind)
            {
                return Err("invalid section range/type".into());
            }
            let mut hash = Sha256::new();
            let mut remaining = len;
            let mut buffer = [0; 65536];
            while remaining > 0 {
                let take = remaining.min(buffer.len());
                reader.read_exact(&mut buffer[..take])?;
                hash.update(&buffer[..take]);
                remaining -= take;
            }
            if hash.finalize().as_slice() != &entry[24..56] {
                return Err("section checksum mismatch".into());
            }
            sections.push(Section {
                kind,
                offset,
                length: len,
            });
            expected = end;
        }
        if expected as u64 != length {
            return Err("trailing package data".into());
        }
        for kind in [1, 2, 3, 4, 5, 6]
            .into_iter()
            .chain((1..=order).map(|n| 10 + n as u32))
        {
            if !sections.iter().any(|s| s.kind == kind) {
                return Err("missing required section".into());
            }
        }
        let lex = *sections.iter().find(|s| s.kind == 1).unwrap();
        if lex.length % RECORD != 0 || lex.length == 0 {
            return Err("invalid lexicon size".into());
        }
        let words = lex.length / RECORD;
        let strings = *sections.iter().find(|s| s.kind == 2).unwrap();
        let mut string_reader = BufReader::new(PositionedFile::new(file.try_clone()?));
        string_reader.seek(SeekFrom::Start(strings.offset as u64))?;
        reader.seek(SeekFrom::Start(lex.offset as u64))?;
        let mut previous = String::new();
        let mut string_offset = 0;
        let mut count_total = 0u64;
        for _ in 0..words {
            let mut record = [0; RECORD];
            reader.read_exact(&mut record)?;
            count_total += u32_at(&record, 20) as u64;
            let len = u32_at(&record, 8) as usize;
            if u64_at(&record, 0) != string_offset as u64
                || len == 0
                || len > MAX_WORD_BYTES
                || string_offset + len > strings.length
                || !f32_at(&record, 12).is_finite()
                || f32_at(&record, 12) > 0.0
                || u32_at(&record, 16) > 1
            {
                return Err("invalid lexicon record".into());
            }
            let mut word = vec![0; len];
            string_reader.read_exact(&mut word)?;
            let word = String::from_utf8(word)?;
            if word <= previous || normalize(&word) != word {
                return Err("unsorted/unnormalized lexicon".into());
            }
            previous = word;
            string_offset += len;
        }
        if string_offset != strings.length {
            return Err("invalid string pool length".into());
        }
        let mut lex_reader = BufReader::with_capacity(4096, PositionedFile::new(file.try_clone()?));
        for section in sections
            .iter()
            .filter(|s| s.kind == 3 || (11..=14).contains(&s.kind))
        {
            if section.length % RECORD != 0 {
                return Err("invalid table size".into());
            }
            reader.seek(SeekFrom::Start(section.offset as u64))?;
            let mut previous = None;
            let mut context_count = 0;
            for _ in 0..section.length / RECORD {
                let mut record = [0; RECORD];
                reader.read_exact(&mut record)?;
                let k = key(&record);
                if previous.is_some_and(|p| p >= k)
                    || !f32_at(&record, 16).is_finite()
                    || f32_at(&record, 16) > 0.0
                    || !f32_at(&record, 20).is_finite()
                {
                    return Err("invalid sorted probability table".into());
                }
                if section.kind == 3 {
                    if k[0] == NONE
                        || k[3] == NONE
                        || k.iter().any(|id| *id != NONE && *id as usize >= words)
                    {
                        return Err("invalid continuation ids".into());
                    }
                    let used = k[..3].iter().take_while(|id| **id != NONE).count();
                    if used > order - 1
                        || k[used..3].iter().any(|id| *id != NONE)
                        || u32_at(&record, 20) != 0
                    {
                        return Err("invalid continuation context/reserved".into());
                    }
                    context_count = if previous.is_some_and(|p: [u32; 4]| p[..3] == k[..3]) {
                        context_count + 1
                    } else {
                        1
                    };
                    if context_count > 128 {
                        return Err("continuation shortlist exceeds 128".into());
                    }
                } else {
                    let n = section.kind as usize - 10;
                    if k[..n].iter().any(|id| *id as usize >= words)
                        || k[n..].iter().any(|id| *id != NONE)
                    {
                        return Err("invalid ngram ids".into());
                    }
                    if n == 1
                        && u32_at(
                            &read_at::<4>(
                                &mut lex_reader,
                                lex.offset + k[0] as usize * RECORD + 16,
                            )?,
                            0,
                        ) != 0
                    {
                        return Err("LM unigram marked external-only".into());
                    }
                }
                previous = Some(k);
            }
        }
        let tree = sections.iter().find(|s| s.kind == 4).unwrap();
        let tree_base = words.next_power_of_two();
        if tree.length != tree_base * 8 {
            return Err("invalid prefix index size".into());
        }
        reader.seek(SeekFrom::Start(tree.offset as u64))?;
        for _ in 0..tree.length / 4 {
            let mut b = [0; 4];
            reader.read_exact(&mut b)?;
            let id = u32_at(&b, 0);
            if id != NONE && id as usize >= words {
                return Err("invalid prefix id".into());
            }
        }
        // Verify the derived index with bounded read buffers, never copying the tree.
        let mut index_reader =
            BufReader::with_capacity(4096, PositionedFile::new(file.try_clone()?));
        for i in 0..tree_base * 2 {
            let id = u32_at(&read_at::<4>(&mut index_reader, tree.offset + i * 4)?, 0);
            if i == 0 {
                if id != NONE {
                    return Err("invalid prefix root sentinel".into());
                }
                continue;
            }
            if i >= tree_base {
                let index = i - tree_base;
                let expected = if index >= words {
                    NONE
                } else {
                    let record = read_at::<RECORD>(&mut lex_reader, lex.offset + index * RECORD)?;
                    let first = read_at::<1>(
                        &mut string_reader,
                        strings.offset + u64_at(&record, 0) as usize,
                    )?;
                    if first[0] == b'<' {
                        NONE
                    } else {
                        index as u32
                    }
                };
                if id != expected {
                    return Err("invalid prefix leaf".into());
                }
            } else {
                let children = read_at::<8>(&mut index_reader, tree.offset + i * 8)?;
                let a = u32_at(&children, 0);
                let b = u32_at(&children, 4);
                let mut priority = |word: u32| -> Result<f64> {
                    if word == NONE {
                        return Ok(f64::NEG_INFINITY);
                    }
                    let record =
                        read_at::<RECORD>(&mut lex_reader, lex.offset + word as usize * RECORD)?;
                    let count = u32_at(&record, 20);
                    Ok(if count > 0 {
                        count as f64
                    } else {
                        f32_at(&record, 12) as f64
                    })
                };
                let pa = priority(a)?;
                let pb = priority(b)?;
                let expected = if pa > pb || (pa == pb && a < b) { a } else { b };
                if id != expected {
                    return Err("invalid prefix maximum".into());
                }
            }
        }
        for kind in [5, 6] {
            let s = sections.iter().find(|s| s.kind == kind).unwrap();
            if s.length > MAX_METADATA_BYTES {
                return Err("metadata exceeds bound".into());
            }
            // Streaming JSON verification does not retain attribution on the runtime heap.
            reader.seek(SeekFrom::Start(s.offset as u64))?;
            let mut de =
                serde_json::Deserializer::from_reader(reader.by_ref().take(s.length as u64));
            serde::de::IgnoredAny::deserialize(&mut de)?;
            de.end()?;
        }
        let policy = sections.iter().find(|s| s.kind == 6).unwrap();
        if policy.length != POLICY.len()
            || read_at::<{ POLICY.len() }>(&mut reader, policy.offset)? != POLICY
        {
            return Err("unsupported package policy".into());
        }
        // The file must remain immutable for the lifetime of every reader (documented API contract).
        let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };
        let model = Self {
            mmap,
            sections,
            order,
            words,
            tree_base,
            count_total,
        };
        for word in ["<s>", "</s>", "<unk>"] {
            if model
                .word_id(word)
                .and_then(|id| model.row(&[id]))
                .is_none()
            {
                return Err("missing special word".into());
            }
        }
        Ok(model)
    }

    fn section(&self, kind: u32) -> &[u8] {
        let s = self.sections.iter().find(|s| s.kind == kind).unwrap();
        &self.mmap[s.offset..s.offset + s.length]
    }
    fn word(&self, id: u32) -> &str {
        let record = &self.section(1)[id as usize * RECORD..][..RECORD];
        let start = u64_at(record, 0) as usize;
        let len = u32_at(record, 8) as usize;
        std::str::from_utf8(&self.section(2)[start..start + len]).unwrap()
    }
    fn probability(&self, id: u32) -> f64 {
        let count = u32_at(self.section(1), id as usize * RECORD + 20);
        if count > 0 {
            count as f64
        } else {
            f32_at(self.section(1), id as usize * RECORD + 12) as f64
        }
    }
    pub(crate) fn external(&self, word: &str) -> bool {
        self.word_id(word)
            .is_some_and(|id| u32_at(self.section(1), id as usize * RECORD + 16) == 1)
    }
    pub fn word_id(&self, word: &str) -> Option<u32> {
        let i = self.lower_bound(word);
        (i < self.words && self.word(i as u32) == word).then_some(i as u32)
    }
    fn lower_bound(&self, word: &str) -> usize {
        let (mut lo, mut hi) = (0, self.words);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.word(mid as u32) < word {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
    fn row(&self, ids: &[u32]) -> Option<(f64, f64)> {
        if ids.is_empty() || ids.len() > self.order {
            return None;
        }
        let mut k = [NONE; 4];
        k[..ids.len()].copy_from_slice(ids);
        let table = self.section(10 + ids.len() as u32);
        let (mut lo, mut hi) = (0, table.len() / RECORD);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let r = &table[mid * RECORD..][..RECORD];
            match key(r).cmp(&k) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Some((f32_at(r, 16) as f64, f32_at(r, 20) as f64)),
            }
        }
        None
    }
    fn lm_id(&self, word: &str) -> u32 {
        self.word_id(&normalize(word))
            .filter(|id| u32_at(self.section(1), *id as usize * RECORD + 16) == 0)
            .unwrap_or_else(|| self.word_id("<unk>").unwrap())
    }
    pub fn score(&self, context: &[&str], word: &str) -> f64 {
        let id = self.lm_id(word);
        let context: Vec<_> = context
            .iter()
            .rev()
            .take(self.order - 1)
            .map(|w| self.lm_id(w))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.score_ids(&context, id)
    }
    pub(crate) fn scorer(&self) -> Scorer<'_> {
        Scorer {
            model: self,
            ids: BTreeMap::new(),
        }
    }
    fn score_ids(&self, context: &[u32], id: u32) -> f64 {
        let unknown = self.word_id("<unk>").unwrap();
        let mut backoff = 0.0;
        for start in 0..=context.len() {
            let h = &context[start..];
            let mut n = h.to_vec();
            n.push(id);
            if let Some((p, _)) = self.row(&n) {
                return backoff + p;
            }
            if let Some((_, b)) = self.row(h) {
                backoff += b;
            }
        }
        backoff + self.row(&[unknown]).unwrap().0
    }
    pub fn score_sentence(&self, words: &[&str]) -> f64 {
        let mut context = vec!["<s>"];
        let mut score = 0.0;
        for word in words.iter().copied().chain(std::iter::once("</s>")) {
            score += self.score(&context, word);
            context.push(word);
        }
        score
    }
    pub fn frequency_score(&self, word: &str) -> f64 {
        if self.count_total == 0 {
            return self.score(&[], word);
        }
        let count = self
            .word_id(&normalize(word))
            .map(|id| u32_at(self.section(1), id as usize * RECORD + 20))
            .unwrap_or(0);
        ((count as f64 + 1.0) / (self.count_total as f64 + self.words as f64)).log10()
    }
    /// Sorted mmap-backed (normalized key, observed count, external-only) records.
    pub fn vocabulary(&self) -> impl ExactSizeIterator<Item = (&str, u32, bool)> {
        (0..self.words as u32).map(|id| {
            let record = &self.section(1)[id as usize * RECORD..][..RECORD];
            (self.word(id), u32_at(record, 20), u32_at(record, 16) == 1)
        })
    }
    fn range(&self, prefix: &str) -> (usize, usize) {
        let lo = self.lower_bound(prefix);
        let mut upper = prefix.to_owned();
        upper.push(char::MAX);
        (lo, self.lower_bound(&upper))
    }
    fn range_best(&self, mut l: usize, mut r: usize) -> u32 {
        l += self.tree_base;
        r += self.tree_base;
        let mut best = NONE;
        while l < r {
            if l % 2 == 1 {
                best = better_id(best, u32_at(self.section(4), l * 4), |id| {
                    self.probability(id)
                });
                l += 1;
            }
            if r % 2 == 1 {
                r -= 1;
                best = better_id(best, u32_at(self.section(4), r * 4), |id| {
                    self.probability(id)
                });
            }
            l /= 2;
            r /= 2;
        }
        best
    }
    pub fn prefix_top(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.prefix_top_filtered(prefix, limit, true)
    }
    pub(crate) fn prefix_top_filtered(
        &self,
        prefix: &str,
        limit: usize,
        external: bool,
    ) -> Vec<String> {
        let (l, r) = self.range(prefix);
        let mut heap = BinaryHeap::new();
        let mut result = Vec::new();
        let push = |heap: &mut BinaryHeap<HeapRange>, l: usize, r: usize| {
            if l < r {
                let id = self.range_best(l, r);
                if id != NONE {
                    heap.push(HeapRange {
                        p: self.probability(id),
                        id,
                        l,
                        r,
                    });
                }
            }
        };
        push(&mut heap, l, r);
        while result.len() < limit {
            let Some(top) = heap.pop() else { break };
            if external || !self.external(self.word(top.id)) {
                result.push(self.word(top.id).to_owned());
            }
            push(&mut heap, top.l, top.id as usize);
            push(&mut heap, top.id as usize + 1, top.r);
        }
        result
    }
    pub fn prefix_top_full_scan(&self, prefix: &str, limit: usize) -> Vec<String> {
        let mut words: Vec<_> = (0..self.words as u32)
            .filter(|id| !self.word(*id).starts_with('<') && self.word(*id).starts_with(prefix))
            .collect();
        words.sort_by(|a, b| {
            self.probability(*b)
                .total_cmp(&self.probability(*a))
                .then(a.cmp(b))
        });
        words.truncate(limit);
        words
            .into_iter()
            .map(|id| self.word(id).to_owned())
            .collect()
    }
    pub(crate) fn continuations(&self, context: &[String]) -> Vec<String> {
        let mut result = Vec::new();
        let table = self.section(3);
        for n in (1..=context.len().min(self.order - 1)).rev() {
            let mut h = [NONE; 3];
            for (i, w) in context[context.len() - n..].iter().enumerate() {
                h[i] = self.lm_id(w);
            }
            let (mut lo, mut hi) = (0, table.len() / RECORD);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let k = key(&table[mid * RECORD..]);
                if k[..3] < h[..] {
                    lo = mid + 1
                } else {
                    hi = mid;
                }
            }
            while lo < table.len() / RECORD {
                let k = key(&table[lo * RECORD..]);
                if k[..3] != h {
                    break;
                }
                let word = self.word(k[3]);
                if !word.starts_with('<') {
                    result.push(word.to_owned());
                }
                lo += 1;
            }
        }
        result
    }
}

fn better_id(a: u32, b: u32, p: impl Fn(u32) -> f64) -> u32 {
    if a == NONE {
        return b;
    }
    if b == NONE {
        return a;
    }
    if p(a) > p(b) || (p(a) == p(b) && a < b) {
        a
    } else {
        b
    }
}
#[derive(Clone, Copy)]
struct HeapRange {
    p: f64,
    id: u32,
    l: usize,
    r: usize,
}
impl PartialEq for HeapRange {
    fn eq(&self, o: &Self) -> bool {
        self.p == o.p && self.id == o.id
    }
}
impl Eq for HeapRange {}
impl PartialOrd for HeapRange {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapRange {
    fn cmp(&self, o: &Self) -> Ordering {
        self.p.total_cmp(&o.p).then(o.id.cmp(&self.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_scorer_reuses_ids_with_bit_identical_scores() {
        let (_dir, path) = contextual_package();
        let model = Model::load(path).unwrap();
        let mut scorer = model.scorer();
        for context in [
            vec![],
            vec!["<s>"],
            vec!["АЛЬФА", "неизвестное"],
            vec!["а", "альфа", "цель", "Е\u{0308}ж"],
        ] {
            for word in ["цель", "АЛЬФА", "Е\u{0308}ж", "нет-в-словаре"] {
                for _ in 0..3 {
                    assert_eq!(
                        scorer.score(&context, word).to_bits(),
                        model.score(&context, word).to_bits()
                    );
                    assert_eq!(
                        scorer.frequency_score(word).to_bits(),
                        model.frequency_score(word).to_bits()
                    );
                }
            }
        }
        let before = scorer.ids.len();
        scorer.score(&["АЛЬФА", "неизвестное"], "цель");
        assert_eq!(scorer.ids.len(), before);
        assert!(before <= 10);
    }

    fn contextual_package() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let arpa = dir.path().join("context.arpa");
        let package = dir.path().join("context.lpk");
        let fillers = (1..=130)
            .map(|n| format!("-0.1 {}\n", "а".repeat(n)))
            .collect::<String>();
        fs::write(
            &arpa,
            format!("\\data\\\nngram 1=135\nngram 2=2\n\\1-grams:\n-3 <unk> 0\n0 <s> 0\n-1 </s>\n-0.1 альфа\n-2 цель\n{fillers}\\2-grams:\n-0.01 <unk> цель\n-0.02 альфа цель\n\\end\\\n"),
        )
        .unwrap();
        Model::compile_arpa(
            &arpa,
            &package,
            &PackageMetadata {
                external_words: vec!["внешнее".into(), "цель".into()],
                ..Default::default()
            },
        )
        .unwrap();
        (dir, package)
    }

    #[test]
    fn test_continuations_map_oov_and_external_context_like_score() {
        let (_dir, package) = contextual_package();
        let predictor = crate::Predictor::load(&package).unwrap();
        let model = predictor.model();
        assert!(!model.prefix_top("", 128).contains(&"цель".to_owned()));
        assert!(model.external("внешнее"));
        assert!(!model.external("цель"));
        for context in ["альфа", "невиданное", "внешнее"] {
            let expected = if context == "альфа" {
                -0.02
            } else {
                -0.01
            };
            assert!((model.score(&[context], "цель") - expected).abs() < 1e-6);
            let q = format!("{context} ");
            let completion = predictor.explain(crate::Request::new(&q), None).unwrap();
            assert!(!completion.truncated);
            assert!(
                completion
                    .pool
                    .iter()
                    .any(|c| c.edit.apply(&q).unwrap() == format!("{q}цель")),
                "missing low-unigram contextual candidate for {context}"
            );
        }
        for context in ["АЛЬФА", "НЕВИДАННОЕ", "ВНЕШНЕЕ"] {
            assert!(model
                .continuations(&[context.into()])
                .contains(&"цель".into()));
        }
    }

    #[test]
    fn test_load_rejects_rehashed_external_flag_on_lm_unigram() {
        let (dir, package) = contextual_package();
        let model = Model::load(&package).unwrap();
        let original = fs::read(&package).unwrap();
        let mut bytes = original.clone();
        let entry = (0..u32_at(&bytes, 16) as usize)
            .map(|i| HEADER + i * DIR)
            .find(|i| u32_at(&bytes, *i) == 1)
            .unwrap();
        let lex = u64_at(&bytes, entry + 8) as usize;
        let length = u64_at(&bytes, entry + 16) as usize;
        let flag = lex + model.word_id("цель").unwrap() as usize * RECORD + 16;
        bytes[flag..flag + 4].copy_from_slice(&1u32.to_le_bytes());
        let digest = Sha256::digest(&bytes[lex..lex + length]);
        bytes[entry + 24..entry + DIR].copy_from_slice(&digest);
        let corrupt = dir.path().join("external-lm-conflict.lpk");
        fs::write(&corrupt, bytes).unwrap();
        assert!(
            Model::load(corrupt).is_err(),
            "LM unigram must not be external-only"
        );
        assert_eq!(fs::read(&package).unwrap(), original);
        let valid = Model::load(&package).unwrap();
        assert!(valid.external("внешнее"));
        assert!(!valid.external("цель"));
        assert!((valid.score(&["<unk>"], "цель") + 0.01).abs() < 1e-6);
        assert!((valid.score(&["альфа"], "цель") + 0.02).abs() < 1e-6);
        assert!((valid.score(&[], "внешнее") + 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_validation_readers_have_independent_positions() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"abcdefgh").unwrap();
        let mut a = PositionedFile::new(file.try_clone().unwrap());
        let mut b = PositionedFile::new(file.try_clone().unwrap());
        a.seek(SeekFrom::Start(2)).unwrap();
        b.seek(SeekFrom::Start(5)).unwrap();
        let mut x = [0; 2];
        a.read_exact(&mut x).unwrap();
        assert_eq!(&x, b"cd");
        b.read_exact(&mut x).unwrap();
        assert_eq!(&x, b"fg");
    }

    const SMALL_ARPA: &str =
        "\\data\\\nngram 1=4\n\\1-grams:\n-3 <unk>\n0 <s>\n-1 </s>\n-1 слово\n\\end\\\n";

    #[test]
    fn test_oversize_metadata_preserves_existing_package() {
        let dir = tempfile::tempdir().unwrap();
        let arpa = dir.path().join("tiny.arpa");
        let package = dir.path().join("model.lpk");
        fs::write(&arpa, SMALL_ARPA).unwrap();
        Model::compile_arpa(&arpa, &package, &PackageMetadata::default()).unwrap();
        let original = fs::read(&package).unwrap();
        let metadata = PackageMetadata {
            attribution: serde_json::Value::String("x".repeat(40_000_001)),
            ..Default::default()
        };
        assert!(Model::compile_arpa(&arpa, &package, &metadata).is_err());
        assert_eq!(fs::read(&package).unwrap(), original);
        assert!(Model::load(&package).is_ok());
    }

    #[test]
    fn test_package_metadata_omits_build_only_fields() {
        let dir = tempfile::tempdir().unwrap();
        let arpa = dir.path().join("tiny.arpa");
        let package = dir.path().join("model.lpk");
        fs::write(&arpa, SMALL_ARPA).unwrap();
        let metadata = PackageMetadata {
            provenance: serde_json::json!({"train":"hash"}),
            attribution: serde_json::json!({"license":"fixture"}),
            external_words: vec!["другое".into()],
            word_counts: BTreeMap::from([("слово".into(), 12)]),
        };
        Model::compile_arpa(&arpa, &package, &metadata).unwrap();
        let bytes = fs::read(&package).unwrap();
        let count = u32_at(&bytes, 16) as usize;
        let entry = (0..count)
            .map(|i| HEADER + i * DIR)
            .find(|i| u32_at(&bytes, *i) == 5)
            .unwrap();
        let offset = u64_at(&bytes, entry + 8) as usize;
        let length = u64_at(&bytes, entry + 16) as usize;
        let stored: serde_json::Value =
            serde_json::from_slice(&bytes[offset..offset + length]).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({"provenance":metadata.provenance,"attribution":metadata.attribution})
        );
        let model = Model::load(package).unwrap();
        assert!(model.word_id("другое").is_some());
        assert!(model.frequency_score("слово") > model.frequency_score("другое"));
    }

    #[test]
    fn test_global_long_words_and_explicit_oversize_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let arpa = dir.path().join("tiny.arpa");
        let package = dir.path().join("model.lpk");
        let word = "я".repeat(129);
        assert_eq!(word.len(), 258);
        fs::write(&arpa, SMALL_ARPA.replace("слово", &word)).unwrap();
        let external = "ю".repeat(129);
        Model::compile_arpa(
            &arpa,
            &package,
            &PackageMetadata {
                external_words: vec![external.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        let model = Model::load(&package).unwrap();
        assert!(model.word_id(&word).is_some());
        assert!(model.word_id(&external).is_some());
        let predictor = crate::Predictor::load(&package).unwrap();
        let prefix = "я".repeat(128);
        assert!(predictor
            .explain(crate::Request::new(&prefix), None)
            .unwrap()
            .pool
            .iter()
            .any(|c| c.edit.apply(&prefix).unwrap() == word));
        let original = fs::read(&package).unwrap();
        let oversize = "я".repeat(8193);
        assert!(oversize.len() > 16384);
        fs::write(&arpa, SMALL_ARPA.replace("слово", &oversize)).unwrap();
        assert!(Model::compile_arpa(&arpa, &package, &PackageMetadata::default()).is_err());
        fs::write(&arpa, SMALL_ARPA).unwrap();
        assert!(Model::compile_arpa(
            &arpa,
            &package,
            &PackageMetadata {
                external_words: vec![oversize],
                ..Default::default()
            }
        )
        .is_err());
        assert_eq!(fs::read(&package).unwrap(), original);
        assert!(Model::load(package).is_ok());
    }
}
