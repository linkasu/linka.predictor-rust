use super::family;
use predictor_core::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub const INDEX_LIMIT: u64 = 16 * 1024 * 1024;
const FAMILY_LIMIT: usize = 20000;
const TOKEN_LIMIT: usize = 4096;
const SHINGLE_LIMIT: usize = 1000000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuratorRecord {
    pub id: String,
    pub group_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearDuplicate {
    pub width: usize,
    pub numerator: usize,
    pub denominator: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalFamily {
    pub id: String,
    pub group_id: String,
    pub sha256: String,
    pub token_count: usize,
    pub shingles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalExclusions {
    pub schema: String,
    pub normalizer: String,
    pub source_seal_sha256: String,
    pub families: Vec<EvalFamily>,
    pub near_duplicate: NearDuplicate,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:#".contains(&b))
}

pub(super) fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn text_hash(normalized: &str) -> String {
    format!("{:x}", Sha256::digest(normalized.as_bytes()))
}

fn shingle_hash(text: &str, width: usize) -> String {
    let mut hash = Sha256::new();
    hash.update(b"linka-eval-shingle-v1\0");
    hash.update((width as u32).to_le_bytes());
    hash.update(text.as_bytes());
    format!("{:x}", hash.finalize())
}

fn shingles(normalized: &str) -> BTreeSet<String> {
    let tokens: Vec<_> = normalized.split_whitespace().collect();
    let width = tokens.len().min(3);
    if width == 0 {
        return BTreeSet::new();
    }
    tokens
        .windows(width)
        .map(|window| shingle_hash(&window.join(" "), width))
        .collect()
}

pub fn make_index(records: &[CuratorRecord], seal: &str) -> Result<EvalExclusions> {
    if !valid_hash(seal) || records.len() > FAMILY_LIMIT {
        return Err("invalid curator seal or record limit".into());
    }
    let mut index = EvalExclusions {
        schema: "linka-eval-exclusions-v1".into(),
        normalizer: "nfc-lower-v1".into(),
        source_seal_sha256: seal.into(),
        families: Vec::new(),
        near_duplicate: NearDuplicate {
            width: 3,
            numerator: 4,
            denominator: 5,
        },
    };
    let mut ids = BTreeSet::new();
    let mut seen = BTreeMap::new();
    for record in records {
        if !valid_id(&record.id)
            || !valid_id(&record.group_id)
            || !ids.insert(&record.id)
            || record.text.len() > 65536
            || family(&record.text).is_empty()
        {
            return Err("invalid or duplicate curator ID/record".into());
        }
        for (n, text) in std::iter::once(record.text.as_str())
            .chain(record.text.split(['.', '!', '?']))
            .enumerate()
        {
            let normalized = family(text);
            if normalized.is_empty() {
                continue;
            }
            let sha256 = text_hash(&normalized);
            if let Some(group) = seen.get(&sha256) {
                if group != &record.group_id {
                    return Err("duplicate evaluation family crosses independent groups".into());
                }
                continue;
            }
            seen.insert(sha256.clone(), record.group_id.clone());
            let token_count = normalized.split_whitespace().count();
            if index.families.len() == FAMILY_LIMIT || token_count > TOKEN_LIMIT {
                return Err("curator family/token limit exceeded".into());
            }
            index.families.push(EvalFamily {
                id: if n == 0 {
                    record.id.clone()
                } else {
                    format!("{}#segment-{}", record.id, n - 1)
                },
                group_id: record.group_id.clone(),
                sha256,
                token_count,
                shingles: shingles(&normalized).into_iter().collect(),
            });
        }
    }
    index.families.sort_by(|a, b| a.id.cmp(&b.id));
    Matcher::new(index.clone())?;
    if serde_json::to_vec(&index)?.len() as u64 > INDEX_LIMIT {
        return Err("hash-only index exceeds 16 MiB".into());
    }
    Ok(index)
}

pub struct Matcher {
    pub source_seal_sha256: String,
    exact: BTreeSet<String>,
    sizes: Vec<usize>,
    inverted: BTreeMap<String, Vec<usize>>,
}

impl Matcher {
    pub fn read(path: &Path) -> Result<Self> {
        if fs::metadata(path)?.len() > INDEX_LIMIT {
            return Err("evaluation exclusion index exceeds 16 MiB".into());
        }
        let index = serde_json::from_slice(&fs::read(path)?)
            .map_err(|_| "invalid hash-only evaluation index JSON")?;
        Self::new(index)
    }

    pub fn new(index: EvalExclusions) -> Result<Self> {
        if index.schema != "linka-eval-exclusions-v1"
            || index.normalizer != "nfc-lower-v1"
            || !valid_hash(&index.source_seal_sha256)
            || index.families.len() > FAMILY_LIMIT
            || (
                index.near_duplicate.width,
                index.near_duplicate.numerator,
                index.near_duplicate.denominator,
            ) != (3, 4, 5)
        {
            return Err("invalid evaluation exclusion contract".into());
        }
        let mut matcher = Self {
            source_seal_sha256: index.source_seal_sha256,
            exact: BTreeSet::new(),
            sizes: Vec::new(),
            inverted: BTreeMap::new(),
        };
        let mut ids = BTreeSet::new();
        let mut total = 0;
        for (n, entry) in index.families.into_iter().enumerate() {
            let unique: BTreeSet<_> = entry.shingles.iter().collect();
            total += entry.shingles.len();
            if !valid_id(&entry.id)
                || !valid_id(&entry.group_id)
                || !ids.insert(entry.id)
                || !valid_hash(&entry.sha256)
                || !matcher.exact.insert(entry.sha256)
                || !(1..=TOKEN_LIMIT).contains(&entry.token_count)
                || unique.is_empty()
                || unique.len() != entry.shingles.len()
                || unique.len() > entry.token_count.saturating_sub(3).saturating_add(1).max(1)
                || unique.iter().any(|s| !valid_hash(s))
                || total > SHINGLE_LIMIT
            {
                return Err("invalid/duplicate evaluation family, hash or shingle limit".into());
            }
            matcher.sizes.push(unique.len());
            for shingle in unique {
                matcher.inverted.entry(shingle.clone()).or_default().push(n);
            }
        }
        Ok(matcher)
    }

    pub fn matches(&self, normalized: &str) -> Option<&'static str> {
        if self.exact.contains(&text_hash(normalized)) {
            return Some("eval-exact");
        }
        let query = shingles(normalized);
        let mut intersection = BTreeMap::<usize, usize>::new();
        for shingle in &query {
            if let Some(entries) = self.inverted.get(shingle) {
                for entry in entries {
                    *intersection.entry(*entry).or_default() += 1;
                }
            }
        }
        intersection
            .into_iter()
            .any(|(n, common)| 5 * common >= 4 * (query.len() + self.sizes[n] - common))
            .then_some("eval-near")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curator_and_consumer_reject_empty_records_invalid_bounds_and_raw_fields() {
        let record = CuratorRecord {
            id: "r1".into(),
            group_id: "g1".into(),
            text: "!?".into(),
        };
        assert!(make_index(&[record], &"a".repeat(64)).is_err());
        let index = make_index(
            &[CuratorRecord {
                id: "r1".into(),
                group_id: "g1".into(),
                text: "мне нужна помощь. хочу пить".into(),
            }],
            &"a".repeat(64),
        )
        .unwrap();
        assert_eq!(index.families.len(), 3, "whole message and both segments");
        let mut invalid = index.clone();
        invalid.families[0].token_count = TOKEN_LIMIT + 1;
        assert!(Matcher::new(invalid).is_err());
        let mut invalid = index.clone();
        invalid.families[0].sha256 = "X".repeat(64);
        assert!(Matcher::new(invalid).is_err());
        let mut value = serde_json::to_value(&index).unwrap();
        value["families"][0]["text"] = "SYNTHETIC_TARGET_NEVER_ECHO".into();
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("bad.json");
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = Matcher::read(&file).err().unwrap().to_string();
        assert!(!error.contains("SYNTHETIC_TARGET_NEVER_ECHO"));
    }

    #[test]
    fn curator_index_is_hash_only_and_matches_fixed_integer_jaccard() {
        let index = make_index(
            &[CuratorRecord {
                id: "message-1".into(),
                group_id: "group-1".into(),
                text: "Аа бб вв гг дд ее!".into(),
            }],
            &"a".repeat(64),
        )
        .unwrap();
        let serialized = serde_json::to_string(&index).unwrap();
        assert!(!serialized.contains("Аа") && !serialized.contains("бб"));
        assert_eq!(
            index.families.len(),
            1,
            "whole/single-segment duplicate is deduplicated"
        );
        let matcher = Matcher::new(index.clone()).unwrap();
        assert_eq!(matcher.matches("аа бб вв гг дд ее"), Some("eval-exact"));
        assert_eq!(matcher.matches("аа бб вв гг дд ее жж"), Some("eval-near"));
        assert_eq!(matcher.matches("аа бб вв гг дд ее жж зз"), None);
        assert_ne!(shingle_hash("аа бб", 2), shingle_hash("аа бб", 3));
        assert_ne!(super::super::family("ёлка"), super::super::family("елка"));

        let mut invalid = index.clone();
        invalid.normalizer = "lower".into();
        assert!(Matcher::new(invalid).is_err());
        let mut invalid = index.clone();
        invalid.families.push(invalid.families[0].clone());
        assert!(Matcher::new(invalid).is_err());
        let mut invalid = index.clone();
        let duplicate = invalid.families[0].shingles[0].clone();
        invalid.families[0].shingles.push(duplicate);
        assert!(Matcher::new(invalid).is_err());
        let mut invalid = index;
        invalid.near_duplicate.numerator = 3;
        assert!(Matcher::new(invalid).is_err());
    }
}
