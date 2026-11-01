use crate::{normalize, project, tokenize, Model, PersonalModel, Response, Result, TailEdit};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

fn horizontal_gap(gap: &str) -> bool {
    gap.chars().all(|c| {
        c.is_whitespace()
            && !matches!(
                c,
                '\n' | '\r' | '\u{000b}' | '\u{000c}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
            )
    })
}

#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub q: &'a str,
    pub lang: &'a str,
    pub limit: usize,
}
impl<'a> Request<'a> {
    pub fn new(q: &'a str) -> Self {
        Self {
            q,
            lang: "ru",
            limit: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    pub frequency_only: bool,
    pub letters: bool,
    pub spaces: bool,
    pub external_lexicon: bool,
    pub full_scan_oracle: bool,
    pub candidate_limit: usize,
    pub max_operations: usize,
    pub letter_cost: f64,
    pub space_cost: f64,
}
impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            frequency_only: false,
            letters: true,
            spaces: true,
            external_lexicon: true,
            full_scan_oracle: false,
            candidate_limit: 128,
            max_operations: 12000,
            letter_cost: 1.5,
            space_cost: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub edit: TailEdit,
    pub score: f64,
    pub letter_edits: u8,
    pub space_edits: u8,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Completion {
    pub response: Response,
    pub candidates: Vec<Candidate>,
    pub pool: Vec<Candidate>,
    pub unchanged_score: f64,
    pub truncated: bool,
}

pub struct Predictor {
    model: Model,
    config: SearchConfig,
}
impl Predictor {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            model: Model::load(path)?,
            config: SearchConfig::default(),
        })
    }
    pub fn with_config(mut self, config: SearchConfig) -> Result<Self> {
        if !(1..=100_000).contains(&config.candidate_limit)
            || !(1..=1_000_000).contains(&config.max_operations)
            || !config.letter_cost.is_finite()
            || config.letter_cost < 0.0
            || !config.space_cost.is_finite()
            || config.space_cost < 0.0
        {
            return Err("invalid search configuration".into());
        }
        self.config = config;
        Ok(self)
    }
    pub fn model(&self) -> &Model {
        &self.model
    }
    pub fn complete(
        &self,
        request: Request<'_>,
        personal: Option<&PersonalModel>,
    ) -> Result<Response> {
        Ok(self.explain(request, personal)?.response)
    }
    pub fn explain(
        &self,
        request: Request<'_>,
        personal: Option<&PersonalModel>,
    ) -> Result<Completion> {
        let q = request.q;
        if request.lang != "ru" {
            return Err("only ru is supported".into());
        }
        if !(1..=100).contains(&request.limit) {
            return Err("limit must be 1..100".into());
        }
        if q.len() > 16384 {
            return Err("query exceeds 16 KiB".into());
        }
        if q.is_empty() {
            return Ok(Completion {
                response: project(q, &[])?,
                candidates: Vec::new(),
                pool: Vec::new(),
                unchanged_score: 0.0,
                truncated: false,
            });
        }
        let normalized_q = normalize(q);
        let tokens = tokenize(q);
        let Some(last) = tokens.last() else {
            return Ok(Completion {
                response: project(q, &[])?,
                candidates: Vec::new(),
                pool: Vec::new(),
                unchanged_score: 0.0,
                truncated: false,
            });
        };
        // Do not cross punctuation, numbers, URL components or sentence boundaries.
        if !horizontal_gap(&q[last.end_byte..]) {
            return Ok(Completion {
                response: project(q, &[])?,
                candidates: Vec::new(),
                pool: Vec::new(),
                unchanged_score: 0.0,
                truncated: false,
            });
        }
        let active = last.end_byte == q.len();
        let last_i = tokens.len() - 1;
        let can_join = active
            && last_i > 0
            && horizontal_gap(&q[tokens[last_i - 1].end_byte..last.start_byte]);
        let origin_i = if can_join { last_i - 1 } else { last_i };
        let origin = tokens[origin_i].start_byte;
        let mut context = Vec::new();
        for (i, token) in tokens[..origin_i].iter().enumerate() {
            if i > 0 && !horizontal_gap(&q[tokens[i - 1].end_byte..token.start_byte]) {
                context.clear();
            }
            context.push(token.normalized.clone());
        }
        if origin_i > 0 && !horizontal_gap(&q[tokens[origin_i - 1].end_byte..origin]) {
            context.clear();
        }
        if context.is_empty() {
            context.push("<s>".into());
        }
        if context.len() > 3 {
            context.drain(..context.len() - 3);
        }
        let mut search = Search {
            predictor: self,
            personal,
            operations: 0,
            truncated: false,
            found: BTreeMap::new(),
            continuations: BTreeMap::new(),
            #[cfg(test)]
            continuation_calls: BTreeMap::new(),
        };
        if active {
            let prefix = &last.normalized;
            let mut h = context.clone();
            if can_join {
                h.push(tokens[origin_i].normalized.clone());
            }
            search.words(prefix, &h, 0, 0, &mut |s, w, l, sp| {
                s.add(last.start_byte, w, l, sp)
            });
            if self.model.word_id(prefix).is_some()
                || personal.is_some_and(|p| p.words().any(|(w, _)| w == prefix))
            {
                let mut h = h.clone();
                h.push(prefix.clone());
                search.words("", &h, 0, 0, &mut |s, w, l, sp| {
                    s.add(q.len(), format!(" {w}"), l, sp)
                });
            }
            if self.config.spaces {
                let sources = vec![(prefix.clone(), 0)];
                for (source, letters) in sources {
                    if !search.tick() {
                        break;
                    }
                    let boundaries: Vec<_> =
                        source.char_indices().skip(1).map(|(i, _)| i).collect();
                    for split in boundaries {
                        if !search.tick() {
                            break;
                        }
                        let left = &source[..split];
                        let right = &source[split..];
                        if !search.allowed_word(left) {
                            continue;
                        }
                        let mut split_context = h.clone();
                        split_context.push(left.to_owned());
                        search.words(right, &split_context, letters, 1, &mut |s, w, l, sp| {
                            s.add(last.start_byte, format!("{left} {w}"), l, sp)
                        });
                    }
                }
                if can_join {
                    let joined = format!("{}{}", tokens[origin_i].normalized, last.normalized);
                    search.words(&joined, &context, 0, 1, &mut |s, w, l, sp| {
                        s.add(origin, w, l, sp)
                    });
                    if self.config.letters && (3..=32).contains(&joined.chars().count()) {
                        for variant in letter_variants(&joined) {
                            if !search.tick() {
                                break;
                            }
                            search.words(&variant, &context, 1, 1, &mut |s, w, l, sp| {
                                s.add(origin, w, l, sp)
                            });
                        }
                    }
                }
            }
            if self.config.letters && (3..=32).contains(&prefix.chars().count()) {
                for variant in letter_variants(prefix) {
                    if !search.tick() {
                        break;
                    }
                    search.words(&variant, &h, 1, 0, &mut |s, w, l, sp| {
                        s.add(last.start_byte, w, l, sp)
                    });
                    if self.config.spaces {
                        for (split, _) in variant.char_indices().skip(1) {
                            if !search.tick() {
                                break;
                            }
                            let left = &variant[..split];
                            if !search.allowed_word(left) {
                                continue;
                            }
                            let mut h = h.clone();
                            h.push(left.to_owned());
                            search.words(&variant[split..], &h, 1, 1, &mut |s, w, l, sp| {
                                s.add(last.start_byte, format!("{left} {w}"), l, sp)
                            });
                        }
                    }
                }
            }
        } else {
            let mut h = context.clone();
            h.push(last.normalized.clone());
            search.words("", &h, 0, 0, &mut |s, w, l, sp| s.add(q.len(), w, l, sp));
        }
        search.add(q.len(), String::new(), 0, 0);
        let truncated = search.truncated;
        let mut candidates = Vec::new();
        let mut unchanged_score = 0.0;
        // Every hypothesis starts at the same original boundary, including preserved words.
        let mut scorer = self.model.scorer();
        for ((start, replacement), (letters, spaces)) in search.found {
            let edit = TailEdit {
                start_byte: start,
                replacement,
            };
            let output = edit.apply(q)?;
            let tail = &output[origin..];
            let mut h = context.clone();
            let mut score = 0.0;
            for word in tokenize(tail) {
                let word = word.normalized;
                let refs: Vec<_> = h.iter().map(String::as_str).collect();
                score += if self.config.frequency_only {
                    scorer.frequency_score(&word)
                } else {
                    scorer.score(&refs, &word)
                };
                score += personal.map(|p| p.boost(&word)).unwrap_or(0.0);
                h.push(word);
            }
            score -=
                letters as f64 * self.config.letter_cost + spaces as f64 * self.config.space_cost;
            if normalize(&output) == normalized_q {
                unchanged_score = score;
                continue;
            }
            candidates.push(Candidate {
                edit,
                score,
                letter_edits: letters,
                space_edits: spaces,
            });
        }
        candidates.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.letter_edits.cmp(&b.letter_edits))
                .then(a.space_edits.cmp(&b.space_edits))
                .then(a.edit.replacement.cmp(&b.edit.replacement))
                .then(a.edit.start_byte.cmp(&b.edit.start_byte))
        });
        let mut seen = BTreeSet::new();
        candidates.retain(|c| seen.insert(c.edit.apply(q).unwrap()));
        let pool = candidates.clone();
        candidates.truncate(request.limit);
        let edits: Vec<_> = candidates.iter().map(|c| c.edit.clone()).collect();
        Ok(Completion {
            response: project(q, &edits)?,
            candidates,
            pool,
            unchanged_score,
            truncated,
        })
    }
}

struct Search<'a> {
    predictor: &'a Predictor,
    personal: Option<&'a PersonalModel>,
    operations: usize,
    truncated: bool,
    found: BTreeMap<(usize, String), (u8, u8)>,
    continuations: BTreeMap<Vec<String>, Vec<String>>,
    #[cfg(test)]
    continuation_calls: BTreeMap<Vec<String>, usize>,
}
impl Search<'_> {
    fn tick(&mut self) -> bool {
        if self.operations >= self.predictor.config.max_operations {
            self.truncated = true;
            false
        } else {
            self.operations += 1;
            true
        }
    }
    fn allowed_word(&self, word: &str) -> bool {
        (!word.starts_with('<'))
            && ((self.predictor.model.word_id(word).is_some()
                && (self.predictor.config.external_lexicon
                    || !self.predictor.model.external(word)))
                || self
                    .personal
                    .is_some_and(|p| p.words().any(|(w, _)| w == word)))
    }
    fn add(&mut self, start: usize, mut word: String, letters: u8, spaces: u8) {
        if let Some(personal) = self.personal {
            for token in tokenize(&word).into_iter().rev() {
                if let Some(surface) = personal.surface(&token.normalized) {
                    word.replace_range(token.start_byte..token.end_byte, surface);
                }
            }
        }
        let cost = (letters, spaces);
        let penalty = |cost: (u8, u8)| {
            cost.0 as f64 * self.predictor.config.letter_cost
                + cost.1 as f64 * self.predictor.config.space_cost
        };
        self.found
            .entry((start, word))
            .and_modify(|old| {
                if penalty(cost) < penalty(*old) || (penalty(cost) == penalty(*old) && cost < *old)
                {
                    *old = cost;
                }
            })
            .or_insert(cost);
    }
    fn words(
        &mut self,
        prefix: &str,
        context: &[String],
        letters: u8,
        spaces: u8,
        consume: &mut impl FnMut(&mut Self, String, u8, u8),
    ) {
        if !self.tick() {
            return;
        }
        let config = &self.predictor.config;
        let model = &self.predictor.model;
        let limit = if config.full_scan_oracle {
            usize::MAX
        } else {
            config.candidate_limit
        };
        let mut words = if config.full_scan_oracle {
            model.prefix_top_full_scan(prefix, limit)
        } else {
            model.prefix_top_filtered(prefix, limit, config.external_lexicon)
        };
        if !config.frequency_only {
            let continuations = self
                .continuations
                .entry(context.to_vec())
                .or_insert_with(|| {
                    #[cfg(test)]
                    {
                        *self.continuation_calls.entry(context.to_vec()).or_default() += 1;
                    }
                    model.continuations(context)
                });
            words.extend(
                continuations
                    .iter()
                    .filter(|w| w.starts_with(prefix))
                    .cloned(),
            );
        }
        if let Some(personal) = self.personal {
            words.extend(
                personal
                    .words()
                    .filter(|(w, _)| w.starts_with(prefix))
                    .map(|(w, _)| w.to_owned()),
            );
        }
        let mut seen = BTreeSet::new();
        for word in words {
            if !seen.insert(word.clone()) || !self.allowed_word(&word) {
                continue;
            }
            if !self.tick() {
                break;
            }
            consume(self, word, letters, spaces);
        }
    }
}

fn letter_variants(word: &str) -> BTreeSet<String> {
    const ALPHABET: &str = "абвгдеёжзийклмнопрстуфхцчшщъыьэюя";
    let chars: Vec<_> = normalize(word).chars().collect();
    let mut variants = BTreeSet::new();
    for i in 0..=chars.len() {
        for c in ALPHABET.chars() {
            let mut v = chars.clone();
            v.insert(i, c);
            variants.insert(v.into_iter().collect());
        }
        if i < chars.len() {
            let mut v = chars.clone();
            v.remove(i);
            variants.insert(v.into_iter().collect());
            for c in ALPHABET.chars() {
                let mut v = chars.clone();
                v[i] = c;
                variants.insert(v.into_iter().collect());
            }
            if i + 1 < chars.len() {
                let mut v = chars.clone();
                v.swap(i, i + 1);
                variants.insert(v.into_iter().collect());
            }
        }
    }
    variants.remove(word);
    variants.remove("");
    variants
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(predictor: &Predictor) -> Search<'_> {
        Search {
            predictor,
            personal: None,
            operations: 0,
            truncated: false,
            found: BTreeMap::new(),
            continuations: BTreeMap::new(),
            continuation_calls: BTreeMap::new(),
        }
    }

    // Frozen uncached words path: independently checks ordering and budget accounting.
    fn uncached_words(search: &mut Search<'_>, prefix: &str, context: &[String]) -> Vec<String> {
        if !search.tick() {
            return Vec::new();
        }
        let config = &search.predictor.config;
        let model = &search.predictor.model;
        let limit = if config.full_scan_oracle {
            usize::MAX
        } else {
            config.candidate_limit
        };
        let mut words = if config.full_scan_oracle {
            model.prefix_top_full_scan(prefix, limit)
        } else {
            model.prefix_top_filtered(prefix, limit, config.external_lexicon)
        };
        if !config.frequency_only {
            words.extend(
                model
                    .continuations(context)
                    .into_iter()
                    .filter(|w| w.starts_with(prefix)),
            );
        }
        let mut seen = BTreeSet::new();
        let mut consumed = Vec::new();
        for word in words {
            if !seen.insert(word.clone()) || !search.allowed_word(&word) {
                continue;
            }
            if !search.tick() {
                break;
            }
            search.add(0, word.clone(), 1, 0);
            consumed.push(word);
        }
        consumed
    }

    #[test]
    fn test_continuation_cache_bounds_work_and_matches_uncached_budgets() {
        let (dir, mut predictor) = fixture();
        let arpa = std::fs::read_to_string(dir.path().join("boundary.arpa"))
            .unwrap()
            .replace("ngram 2=4", "ngram 2=4\nngram 3=1")
            .replace("\\end\\", "\\3-grams:\n-0.2 да я хочу\n\\end\\");
        let input = dir.path().join("continuations.arpa");
        let output = dir.path().join("continuations.lpk");
        std::fs::write(&input, arpa).unwrap();
        Model::compile_arpa(input, &output, &crate::PackageMetadata::default()).unwrap();
        predictor.model = Model::load(output).unwrap();
        let contexts: Vec<Vec<String>> = [
            vec!["<s>"],
            vec!["да", "я"],
            vec!["<s>", "да", "я"],
            vec!["нет"],
        ]
        .into_iter()
        .map(|h| h.into_iter().map(str::to_owned).collect())
        .collect();
        assert_eq!(
            predictor.model.continuations(&contexts[1]),
            ["хочу", "хочу"]
        );
        let mut prefixes = vec![String::new(), "я".into(), "х".into(), "вет".into()];
        prefixes.extend(letter_variants("вет"));
        for frequency_only in [false, true] {
            for full_scan_oracle in [false, true] {
                for external_lexicon in [false, true] {
                    for candidate_limit in [1, 128] {
                        for max_operations in [1, 2, 3, 7, 32, 12000] {
                            predictor.config = SearchConfig {
                                frequency_only,
                                full_scan_oracle,
                                external_lexicon,
                                candidate_limit,
                                max_operations,
                                ..Default::default()
                            };
                            let mut cached = search(&predictor);
                            let mut oracle = search(&predictor);
                            for context in &contexts {
                                for prefix in &prefixes {
                                    let mut actual = Vec::new();
                                    cached.words(prefix, context, 1, 0, &mut |s, w, l, sp| {
                                        s.add(0, w.clone(), l, sp);
                                        actual.push(w);
                                    });
                                    assert_eq!(
                                        actual,
                                        uncached_words(&mut oracle, prefix, context)
                                    );
                                    assert_eq!(cached.operations, oracle.operations);
                                    assert_eq!(cached.truncated, oracle.truncated);
                                    assert_eq!(cached.found, oracle.found);
                                }
                            }
                            assert!(
                                cached.continuation_calls.values().all(|&count| count == 1),
                                "repeated model.continuations work: {:?}, budget {max_operations}",
                                cached.continuation_calls
                            );
                            for (context, words) in &cached.continuations {
                                assert_eq!(*words, predictor.model.continuations(context));
                            }
                            if frequency_only {
                                assert!(cached.continuation_calls.is_empty());
                            } else if max_operations == 12000 {
                                assert_eq!(cached.continuation_calls.len(), contexts.len());
                                assert!(!cached.truncated);
                            } else {
                                assert!(cached.truncated);
                            }
                        }
                    }
                }
            }
        }
    }
    fn fixture() -> (tempfile::TempDir, Predictor) {
        // Hand-authored boundary arithmetic, not trainer output.
        let arpa="\\data\\\nngram 1=11\nngram 2=4\n\\1-grams:\n-3 <unk>\n0 <s> 0\n-1 </s>\n-0.7 привет\n-0.8 ёж\n-1.2 при\n-0.8 ветер\n-0.3 да 0\n-1 я 0\n-1.1 хочу\n-1 другое\n\\2-grams:\n-0.1 <s> я 0\n-0.9 да я 0\n-0.2 я хочу\n-0.15 <s> ветер\n\\end\\\n";
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("boundary.arpa");
        let output = dir.path().join("model.lpk");
        std::fs::write(&input, arpa).unwrap();
        Model::compile_arpa(input, &output, &crate::PackageMetadata::default()).unwrap();
        let predictor = Predictor::load(output)
            .unwrap()
            .with_config(SearchConfig {
                letters: false,
                ..Default::default()
            })
            .unwrap();
        (dir, predictor)
    }
    #[test]
    fn test_hard_boundaries_preserved_and_reset_bos() {
        let (_dir, predictor) = fixture();
        for separator in [
            "\n", "\r\n", "\r", "\u{000b}", "\u{000c}", "\u{0085}", "\u{2028}", "\u{2029}",
        ] {
            let q = format!("при{separator}вет");
            let result = predictor.explain(Request::new(&q), None).unwrap();
            let expected = format!("при{separator}ветер");
            assert!(
                result
                    .pool
                    .iter()
                    .any(|c| c.edit.apply(&q).unwrap() == expected),
                "{separator:?}"
            );
            assert!(
                result
                    .pool
                    .iter()
                    .all(|c| c.edit.start_byte >= q.rfind("вет").unwrap()),
                "crossed {separator:?}"
            );
            let q = format!("да{separator}я ");
            let expected = format!("да{separator}я хочу");
            let result = predictor.explain(Request::new(&q), None).unwrap();
            let candidate = result
                .pool
                .iter()
                .find(|c| c.edit.apply(&q).unwrap() == expected)
                .unwrap();
            assert!(
                (candidate.score + 0.3).abs() < 1e-6,
                "{separator:?}: {}",
                candidate.score
            );
        }
        for separator in [" ", "\t", "\u{00a0}"] {
            let q = format!("при{separator}вет");
            let result = predictor.explain(Request::new(&q), None).unwrap();
            assert!(
                result
                    .pool
                    .iter()
                    .any(|c| c.edit.apply(&q).unwrap() == "привет"),
                "horizontal {separator:?}"
            );
        }
    }
    #[test]
    fn test_case_and_nfc_equivalent_noops_omitted_but_next_word_preserves_input() {
        let (_dir, predictor) = fixture();
        for q in ["Привет", "ПРИВЕТ", "Е\u{0308}ж"] {
            let result = predictor.explain(Request::new(q), None).unwrap();
            assert!(
                result
                    .pool
                    .iter()
                    .all(|c| normalize(&c.edit.apply(q).unwrap()) != normalize(q)),
                "no-op for {q}"
            );
            assert!(
                result.pool.iter().any(|c| c.edit.start_byte == q.len()
                    && c.edit.apply(q).unwrap().starts_with(&format!("{q} "))),
                "missing original-preserving next word for {q}"
            );
        }
    }
}
