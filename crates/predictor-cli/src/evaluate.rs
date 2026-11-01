use crate::client::apply_suggestion;
use predictor_core::{
    normalize, tokenize, Candidate, Model, Predictor, Request, Response, Result, SearchConfig,
    TailEdit,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::BufReader,
    path::Path,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Serialize, Deserialize)]
pub struct Actions {
    pub typed: usize,
    pub paid: usize,
    pub selections: usize,
    pub ksr: f64,
}

pub fn simulate(
    target: &str,
    cost: usize,
    mut suggest: impl FnMut(&str) -> Result<Response>,
) -> Result<Actions> {
    if !(1..=3).contains(&cost) {
        return Err("selection cost must be 1, 2 or 3".into());
    }
    let typed = target.graphemes(true).count();
    let mut query = String::new();
    let (mut paid, mut selections) = (0, 0);
    while query != target {
        let before = query.graphemes(true).count();
        let mut best = query.clone();
        if !query.is_empty() {
            // Actual wire round-trip; the independent client sees no TailEdit.
            let response: Response =
                serde_json::from_slice(&serde_json::to_vec(&suggest(&query)?)?)?;
            for text in response.text {
                let applied = apply_suggestion(&query, response.pos, &text)?;
                if target.starts_with(&applied)
                    && target.is_char_boundary(applied.len())
                    && (applied == target
                        || target
                            .grapheme_indices(true)
                            .any(|(i, _)| i == applied.len()))
                    && applied.graphemes(true).count() > before + cost
                    && applied.len() > best.len()
                {
                    best = applied;
                }
            }
        }
        if best != query {
            query = best;
            paid += cost;
            selections += 1;
        } else {
            let next = target[query.len()..]
                .graphemes(true)
                .next()
                .ok_or("invalid evaluator prefix")?;
            query.push_str(next);
            paid += 1;
        }
    }
    Ok(Actions {
        typed,
        paid,
        selections,
        ksr: if typed == 0 {
            0.0
        } else {
            1.0 - paid as f64 / typed as f64
        },
    })
}

pub fn paired_interval(differences: &[f64], resamples: usize) -> Result<[f64; 2]> {
    let groups: Vec<_> = differences.iter().map(|d| (*d, 1)).collect();
    paired_cluster_interval(&groups, resamples)
}

/// Each component stores (sum of paired phrase deltas, number of phrases).
pub fn resampled_phrase_mean(groups: &[(f64, usize)], indices: &[usize]) -> Result<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for index in indices {
        let (delta, n) = *groups.get(*index).ok_or("invalid resampled group index")?;
        if n == 0 || !delta.is_finite() {
            return Err("invalid paired group aggregate".into());
        }
        sum += delta;
        count = count
            .checked_add(n)
            .ok_or("bootstrap phrase count overflow")?;
    }
    if count == 0 {
        return Err("bootstrap requires phrases".into());
    }
    Ok(sum / count as f64)
}

pub fn paired_cluster_interval(groups: &[(f64, usize)], resamples: usize) -> Result<[f64; 2]> {
    if groups.is_empty()
        || resamples < 100
        || groups.iter().any(|(sum, n)| !sum.is_finite() || *n == 0)
    {
        return Err("bootstrap requires groups and >=100 resamples".into());
    }
    let mut state = 906u64;
    let mut means = Vec::with_capacity(resamples);
    let mut indices = vec![0; groups.len()];
    for _ in 0..resamples {
        for index in &mut indices {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *index = state as usize % groups.len();
        }
        means.push(resampled_phrase_mean(groups, &indices)?);
    }
    means.sort_by(f64::total_cmp);
    Ok([
        means[((resamples - 1) as f64 * 0.025) as usize],
        means[((resamples - 1) as f64 * 0.975) as usize],
    ])
}

fn clean_config(frequency_only: bool) -> SearchConfig {
    SearchConfig {
        frequency_only,
        letters: false,
        spaces: false,
        external_lexicon: false,
        ..Default::default()
    }
}

pub const SELECTION_PROTOCOL: &str = "lowest-component-sha256-whole-file-v1";
pub const GROUPING_VERSION: &str = "duplicate-original-components-v1";
const FIXTURE_GROUPING: &str = "normalized-phrase-fixture-v1";

#[derive(Debug, Clone)]
pub struct GroupedPhrases {
    pub phrases: Vec<String>,
    pub groups: Vec<String>,
    pub grouping_version: &'static str,
    pub grouping_mode: &'static str,
    pub group_map_sha256: Option<String>,
    pub input_sha256: Option<String>,
    pub maximum_groups: usize,
    pub input_phrase_count: usize,
}
impl GroupedPhrases {
    pub fn group_count(&self) -> usize {
        self.groups.iter().collect::<BTreeSet<_>>().len()
    }
}

fn phrase_hash(phrase: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(crate::prepare::family(phrase).as_bytes())
    )
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn walk_phrases(path: &Path, map: Option<&Path>, mut visit: impl FnMut(&str, &str)) -> Result<()> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut map_reader = map.map(File::open).transpose()?.map(BufReader::new);
    let mut line = String::new();
    let mut mapping = String::new();
    while crate::prepare::bounded_line(&mut reader, &mut line)? {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.len() > 16384 {
            return Err("evaluation phrase exceeds runtime input bound".into());
        }
        let key = phrase_hash(line);
        if let Some(reader) = map_reader.as_mut() {
            if line.trim().is_empty() || !crate::prepare::bounded_line(reader, &mut mapping)? {
                return Err("group map/phrase count mismatch".into());
            }
            let (phrase, group) = mapping
                .trim_end_matches(['\r', '\n'])
                .split_once('\t')
                .ok_or("invalid group map record")?;
            if phrase != key || !valid_hash(phrase) || !valid_hash(group) {
                return Err("group map hash/alignment mismatch".into());
            }
            visit(line, group);
        } else if !line.trim().is_empty() {
            visit(line, &key);
        }
    }
    if let Some(reader) = map_reader.as_mut() {
        if crate::prepare::bounded_line(reader, &mut mapping)? {
            return Err("group map has trailing records".into());
        }
    }
    Ok(())
}

pub fn read_grouped_phrases(path: &Path, maximum: usize) -> Result<GroupedPhrases> {
    if maximum == 0 {
        return Err("group limit must be positive".into());
    }
    let map = path.with_extension("groups.tsv");
    let map_path = map.is_file().then_some(map.as_path());
    let input_hash = crate::pipeline::hash_file(path)?;
    let map_hash = map_path.map(crate::pipeline::hash_file).transpose()?;
    let mut selected = BTreeSet::new();
    let mut input_phrase_count = 0;
    walk_phrases(path, map_path, |_, group| {
        input_phrase_count += 1;
        selected.insert(group.to_owned());
        if selected.len() > maximum {
            selected.pop_last();
        }
    })?;
    if selected.is_empty() {
        return Err("evaluation has no phrase groups".into());
    }
    let mut rows = Vec::new();
    walk_phrases(path, map_path, |phrase, group| {
        if selected.contains(group) {
            rows.push((group.to_owned(), phrase.to_owned()));
        }
    })?;
    if crate::pipeline::hash_file(path)? != input_hash
        || map_path.map(crate::pipeline::hash_file).transpose()? != map_hash
    {
        return Err("evaluation input/group map changed during selection".into());
    }
    rows.sort();
    let (groups, phrases) = rows.into_iter().unzip();
    Ok(GroupedPhrases {
        phrases,
        groups,
        grouping_version: if map_path.is_some() {
            GROUPING_VERSION
        } else {
            FIXTURE_GROUPING
        },
        grouping_mode: if map_path.is_some() {
            "component_sidecar"
        } else {
            "normalized_phrase_fixture_unqualified"
        },
        group_map_sha256: map_hash,
        input_sha256: Some(input_hash),
        maximum_groups: maximum,
        input_phrase_count,
    })
}

pub fn read_phrases(path: &Path, maximum: usize) -> Result<Vec<String>> {
    Ok(read_grouped_phrases(path, maximum)?.phrases)
}

pub fn validate_frozen_selection(
    frozen: &serde_json::Value,
    grouped: &GroupedPhrases,
) -> Result<()> {
    for (field, actual) in [
        ("maximum_groups", serde_json::json!(grouped.maximum_groups)),
        ("selection_protocol", serde_json::json!(SELECTION_PROTOCOL)),
        (
            "grouping_version",
            serde_json::json!(grouped.grouping_version),
        ),
        ("grouping_mode", serde_json::json!(grouped.grouping_mode)),
        (
            "group_map_sha256",
            serde_json::json!(grouped.group_map_sha256),
        ),
    ] {
        if frozen.get(field) != Some(&actual) {
            return Err(format!("frozen manifest mismatch: {field}").into());
        }
    }
    Ok(())
}

fn frequency_binding(model: &Model, path: &Path) -> Result<serde_json::Value> {
    let sidecar = path
        .parent()
        .unwrap_or(Path::new("."))
        .join("frequencies.tsv");
    let sidecar_hash = if sidecar.is_file() {
        Some(crate::pipeline::hash_file(&sidecar)?)
    } else {
        None
    };
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut records = 0usize;
    let mut ordinary_lm_records = 0usize;
    let mut zero_count_ordinary_lm_records = 0usize;
    for (word, count, external) in model.vocabulary() {
        if !external && !matches!(word, "<s>" | "</s>" | "<unk>") {
            ordinary_lm_records += 1;
            zero_count_ordinary_lm_records += usize::from(count == 0);
        }
        if count > 0 {
            hash.update(format!("{word}\t{count}\n").as_bytes());
            total += count as u64;
            records += 1;
        }
    }
    let embedded_hash = format!("{:x}", hash.finalize());
    let validation = (|| -> Result<()> {
        if total == 0 {
            return Err("no positive embedded observed counts; ARPA proxy is unqualified".into());
        }
        if zero_count_ordinary_lm_records > 0 {
            return Err("incomplete observed count baseline: ordinary nonexternal LM words have zero counts".into());
        }
        if sidecar_hash.is_none() {
            return Err("missing observed count sidecar".into());
        }
        let mut reader = BufReader::new(File::open(&sidecar)?);
        let mut line = String::new();
        for (word, count, _) in model.vocabulary().filter(|(_, c, _)| *c > 0) {
            if !crate::prepare::bounded_line(&mut reader, &mut line)?
                || normalize(word) != word
                || line != format!("{word}\t{count}\n")
            {
                return Err(
                    "sidecar is not the complete canonical embedded positive-count stream".into(),
                );
            }
        }
        if crate::prepare::bounded_line(&mut reader, &mut line)? {
            return Err("sidecar contains extra count records".into());
        }
        if sidecar_hash.as_deref() != Some(&embedded_hash)
            || Some(crate::pipeline::hash_file(&sidecar)?) != sidecar_hash
        {
            return Err("frequency sidecar hash binding mismatch".into());
        }
        Ok(())
    })();
    Ok(
        serde_json::json!({"qualified":validation.is_ok(),"reason":validation.err().map(|e|e.to_string()),"ordinary_lm_records":ordinary_lm_records,"zero_count_ordinary_lm_records":zero_count_ordinary_lm_records,"positive_embedded_records":records,"embedded_count_total":total,"embedded_canonical_counts_sha256":embedded_hash,"sidecar_sha256":sidecar_hash,"model_sha256":crate::pipeline::hash_file(path)?}),
    )
}

#[derive(Debug, Default)]
pub struct OracleSnapshot {
    pub hits: u64,
    pub denominator: u64,
    pub candidate_cap_truncated: bool,
    pub operation_truncated: bool,
    pub gold_available: Option<bool>,
}
impl OracleSnapshot {
    pub fn certified_space_coverage(&self) -> Option<f64> {
        if self.candidate_cap_truncated || self.operation_truncated {
            None
        } else {
            ratio(self.hits as f64, self.denominator)
        }
    }
    /// Compatibility alias: ALL candidate-space coverage, never desired-gold recall.
    pub fn certified_recall(&self) -> Option<f64> {
        self.certified_space_coverage()
    }
    pub fn certified_gold_available(&self) -> Option<bool> {
        if self.candidate_cap_truncated || self.operation_truncated {
            None
        } else {
            self.gold_available
        }
    }
}

#[derive(Debug, Default)]
pub struct CleanGoldMetrics {
    total_queries: u64,
    pool_hits: u64,
    fullsearch_gold_available: u64,
    fullsearch_gold_unavailable: u64,
    pool_hits_given_oracle_available: u64,
    oracle_inconclusive: u64,
}
impl CleanGoldMetrics {
    pub fn observe(
        &mut self,
        q: &str,
        intended: &str,
        pool: &[Candidate],
        oracle: &OracleSnapshot,
    ) -> Result<()> {
        let mut hit = false;
        for candidate in pool {
            if candidate.edit.apply(q)? == intended {
                hit = true;
                break;
            }
        }
        self.total_queries += 1;
        self.pool_hits += u64::from(hit);
        match oracle.certified_gold_available() {
            Some(true) => {
                self.fullsearch_gold_available += 1;
                self.pool_hits_given_oracle_available += u64::from(hit);
            }
            Some(false) => self.fullsearch_gold_unavailable += 1,
            None => self.oracle_inconclusive += 1,
        }
        Ok(())
    }
    pub fn report(&self) -> serde_json::Value {
        let certified = self.oracle_inconclusive == 0;
        serde_json::json!({"total_queries":self.total_queries,"pool_hits":self.pool_hits,"pool_gold_recall":ratio(self.pool_hits as f64,self.total_queries),
            "fullsearch_gold_available":certified.then_some(self.fullsearch_gold_available),"fullsearch_gold_unavailable":certified.then_some(self.fullsearch_gold_unavailable),
            "pool_hits_given_oracle_available":certified.then_some(self.pool_hits_given_oracle_available),
            "retrieval_recall_given_fullsearch_gold_available":if certified {ratio(self.pool_hits_given_oracle_available as f64,self.fullsearch_gold_available)}else{None},
            "oracle_inconclusive":self.oracle_inconclusive,"gold_match":"TailEdit.apply(q) == exact intended completed prefix"})
    }
}

/// Independent streaming full-vocabulary exact candidate oracle, outside latency SLO.
pub fn exact_candidate_oracle(
    model: &Model,
    q: &str,
    predicted: &[Candidate],
    candidate_cap: usize,
    operation_cap: usize,
) -> Result<OracleSnapshot> {
    enumerate_candidate_oracle(model, q, predicted, candidate_cap, operation_cap, None)
}

pub fn exact_candidate_oracle_with_gold(
    model: &Model,
    q: &str,
    predicted: &[Candidate],
    candidate_cap: usize,
    operation_cap: usize,
    intended: &str,
) -> Result<OracleSnapshot> {
    enumerate_candidate_oracle(
        model,
        q,
        predicted,
        candidate_cap,
        operation_cap,
        Some(intended),
    )
}

fn enumerate_candidate_oracle(
    model: &Model,
    q: &str,
    predicted: &[Candidate],
    candidate_cap: usize,
    operation_cap: usize,
    intended: Option<&str>,
) -> Result<OracleSnapshot> {
    let mut snapshot = OracleSnapshot {
        gold_available: intended.map(|_| false),
        ..Default::default()
    };
    let tokens = tokenize(q);
    let Some(last) = tokens.last() else {
        return Ok(snapshot);
    };
    let active = last.end_byte == q.len();
    // Independently enforce the documented horizontal-gap policy, not runtime search.
    if !q[last.end_byte..].chars().all(|c| {
        c.is_whitespace()
            && !matches!(
                c,
                '\n' | '\r' | '\u{000b}' | '\u{000c}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
            )
    }) {
        return Ok(snapshot);
    }
    let normalized_q = normalize(q);
    let next_word = !active || model.word_id(&last.normalized).is_some();
    let mut operations = 0usize;
    let predicted: BTreeSet<_> = predicted
        .iter()
        .map(|c| (c.edit.start_byte, c.edit.replacement.as_str()))
        .collect();
    for (word, _, external) in model.vocabulary() {
        if operations >= operation_cap {
            snapshot.operation_truncated = true;
            break;
        }
        operations += 1;
        if external || word.starts_with('<') {
            continue;
        }
        let edits = [
            (active && word.starts_with(&last.normalized)).then(|| TailEdit {
                start_byte: last.start_byte,
                replacement: word.into(),
            }),
            next_word.then(|| TailEdit {
                start_byte: q.len(),
                replacement: if active {
                    format!(" {word}")
                } else {
                    word.to_owned()
                },
            }),
        ];
        for edit in edits.into_iter().flatten() {
            if operations >= operation_cap {
                snapshot.operation_truncated = true;
                break;
            }
            operations += 1;
            let applied = edit.apply(q)?;
            if normalize(&applied) == normalized_q {
                continue;
            }
            if snapshot.denominator as usize >= candidate_cap {
                snapshot.candidate_cap_truncated = true;
                break;
            }
            snapshot.denominator += 1;
            if intended.is_some_and(|gold| applied == gold) {
                snapshot.gold_available = Some(true);
            }
            snapshot.hits +=
                u64::from(predicted.contains(&(edit.start_byte, edit.replacement.as_str())));
        }
        if snapshot.operation_truncated || snapshot.candidate_cap_truncated {
            break;
        }
    }
    Ok(snapshot)
}

pub fn evaluate(model: &Path, phrases: &[String]) -> Result<serde_json::Value> {
    let grouped = GroupedPhrases {
        phrases: phrases.to_vec(),
        groups: phrases.iter().map(|p| phrase_hash(p)).collect(),
        grouping_version: FIXTURE_GROUPING,
        grouping_mode: "normalized_phrase_fixture_unqualified",
        group_map_sha256: None,
        input_sha256: None,
        maximum_groups: phrases.len(),
        input_phrase_count: phrases.len(),
    };
    evaluate_grouped(model, &grouped)
}

pub fn evaluate_grouped(model: &Path, grouped: &GroupedPhrases) -> Result<serde_json::Value> {
    let phrases = &grouped.phrases;
    if phrases.is_empty() || phrases.len() != grouped.groups.len() {
        return Err("empty or misaligned grouped evaluation".into());
    }
    let contextual = Predictor::load(model)?.with_config(clean_config(false))?;
    let frequency = Predictor::load(model)?.with_config(clean_config(true))?;
    let corrections = Predictor::load(model)?.with_config(SearchConfig {
        letters: true,
        spaces: true,
        ..clean_config(false)
    })?;
    let letters = Predictor::load(model)?.with_config(SearchConfig {
        letters: true,
        ..clean_config(false)
    })?;
    let spaces = Predictor::load(model)?.with_config(SearchConfig {
        spaces: true,
        ..clean_config(false)
    })?;
    let vocabulary = contextual.model().vocabulary().len();
    let oracle_candidates = vocabulary
        .checked_mul(2)
        .ok_or("oracle candidate budget overflow")?;
    let oracle_operations = vocabulary
        .checked_mul(3)
        .and_then(|n| n.checked_add(1))
        .ok_or("oracle operation budget overflow")?;
    let counts_binding = frequency_binding(contextual.model(), model)?;
    let mut per_phrase = Vec::new();
    let (mut top1, mut top5, mut rr, mut next_n, mut prefix_hits, mut prefix_n) =
        (0u64, 0u64, 0.0, 0u64, 0u64, 0u64);
    let (mut oracle_hits, mut oracle_total, mut displacement, mut unchanged_n, mut truncated) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut differences: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
    let mut corrected_differences = Vec::new();
    let mut oracle_partial = 0u64;
    let mut oracle_candidate_truncations = 0u64;
    let mut oracle_operation_truncations = 0u64;
    let mut prefix_gold = CleanGoldMetrics::default();
    let mut next_gold = CleanGoldMetrics::default();
    for (target, group) in phrases.iter().zip(&grouped.groups) {
        let mut costs = Vec::new();
        for cost in 1..=3 {
            let base = simulate(target, cost, |q| frequency.complete(Request::new(q), None))?;
            let context = simulate(target, cost, |q| contextual.complete(Request::new(q), None))?;
            let corrected = simulate(target, cost, |q| {
                corrections.complete(Request::new(q), None)
            })?;
            let letters_only = simulate(target, cost, |q| letters.complete(Request::new(q), None))?;
            let spaces_only = simulate(target, cost, |q| spaces.complete(Request::new(q), None))?;
            if cost == 1 {
                let aggregate = differences.entry(group).or_default();
                aggregate.0 += context.ksr - base.ksr;
                aggregate.1 += 1;
                corrected_differences.push(corrected.ksr - context.ksr);
            }
            costs.push(serde_json::json!({"selection_cost":cost,"frequency":base,"contextual":context,"letters_only":letters_only,"spaces_only":spaces_only,"corrections":corrected}));
        }
        per_phrase
            .push(serde_json::json!({"target":target,"component_sha256":group,"costs":costs}));
        let tokens = tokenize(target);
        for token in &tokens {
            let intended = &target[..token.end_byte];
            if token.start_byte > 0 {
                let q = &target[..token.start_byte];
                let result = contextual.explain(Request::new(q), None)?;
                next_n += 1;
                let rank = result.response.text.iter().position(|text| {
                    apply_suggestion(q, result.response.pos, text)
                        .ok()
                        .as_deref()
                        == Some(intended)
                });
                if let Some(rank) = rank {
                    if rank == 0 {
                        top1 += 1;
                    }
                    if rank < 5 {
                        top5 += 1;
                        rr += 1.0 / (rank + 1) as f64;
                    }
                }
                let oracle = exact_candidate_oracle_with_gold(
                    contextual.model(),
                    q,
                    &result.pool,
                    oracle_candidates,
                    oracle_operations,
                    intended,
                )?;
                next_gold.observe(q, intended, &result.pool, &oracle)?;
            }
            let token_text = &target[token.start_byte..token.end_byte];
            if let Some((offset, _)) = token_text.grapheme_indices(true).nth(1) {
                let q = &target[..token.start_byte + offset];
                let predicted = contextual.explain(Request::new(q), None)?;
                let response = &predicted.response;
                prefix_n += 1;
                if response.text.iter().any(|text| {
                    apply_suggestion(q, response.pos, text)
                        .ok()
                        .is_some_and(|s| s == intended)
                }) {
                    prefix_hits += 1;
                }
                let expected = exact_candidate_oracle_with_gold(
                    contextual.model(),
                    q,
                    &predicted.pool,
                    oracle_candidates,
                    oracle_operations,
                    intended,
                )?;
                prefix_gold.observe(q, intended, &predicted.pool, &expected)?;
                truncated += u64::from(predicted.truncated);
                oracle_total += expected.denominator;
                oracle_hits += expected.hits;
                oracle_candidate_truncations += u64::from(expected.candidate_cap_truncated);
                oracle_operation_truncations += u64::from(expected.operation_truncated);
                oracle_partial +=
                    u64::from(expected.candidate_cap_truncated || expected.operation_truncated);
            }
        }
        let result = corrections.explain(Request::new(target), None)?;
        unchanged_n += 1;
        if result
            .candidates
            .first()
            .is_some_and(|c| c.letter_edits + c.space_edits > 0 && c.score > result.unchanged_score)
        {
            displacement += 1;
        }
    }
    let paired_groups: Vec<_> = differences.values().copied().collect();
    let ci = paired_cluster_interval(&paired_groups, 10000)?;
    let corrected_mean = corrected_differences.iter().sum::<f64>() / phrases.len() as f64;
    let mut aggregates = Vec::new();
    for cost in 0..3 {
        let mut modes = serde_json::Map::new();
        for mode in [
            "frequency",
            "contextual",
            "letters_only",
            "spaces_only",
            "corrections",
        ] {
            let mean = per_phrase
                .iter()
                .map(|p| p["costs"][cost][mode]["ksr"].as_f64().unwrap())
                .sum::<f64>()
                / phrases.len() as f64;
            let typed = per_phrase
                .iter()
                .map(|p| p["costs"][cost][mode]["typed"].as_u64().unwrap())
                .sum::<u64>();
            let paid = per_phrase
                .iter()
                .map(|p| p["costs"][cost][mode]["paid"].as_u64().unwrap())
                .sum::<u64>();
            modes.insert(mode.into(),serde_json::json!({"mean_phrase_ksr":mean,"action_weighted_ksr":1.0-paid as f64/typed as f64,"typed":typed,"paid":paid}));
        }
        aggregates.push(serde_json::json!({"selection_cost":cost+1,"modes":modes}));
    }
    let actual_groups = differences.len();
    let group_aggregates:Vec<_>=differences.iter().map(|(group,(sum,n))|serde_json::json!({"component_sha256":group,"sum_paired_phrase_deltas":sum,"phrase_count":n})).collect();
    Ok(
        serde_json::json!({"phrase_groups":actual_groups,"phrase_count":phrases.len(),"input_phrase_count":grouped.input_phrase_count,"phrases_sha256":grouped.input_sha256,"group_map_sha256":grouped.group_map_sha256,"grouping_mode":grouped.grouping_mode,"grouping_version":grouped.grouping_version,"selection_protocol":SELECTION_PROTOCOL,"maximum_groups":grouped.maximum_groups,"per_phrase":per_phrase,"aggregates":aggregates,
        "paired_contextual_minus_frequency_95_ci":ci,"bootstrap":{"seed":906,"resamples":10000,"unit":if grouped.grouping_version==GROUPING_VERSION {"component"}else{"normalized_phrase_fixture_unqualified"},"estimand":"unweighted_mean_per_phrase","group_aggregates":group_aggregates},
        "quality_gate":{"minimum_groups":actual_groups>=200,"qualified_component_grouping":grouped.grouping_version==GROUPING_VERSION&&grouped.group_map_sha256.is_some(),"contextual_gain":ci[0]>0.0 && actual_groups>=200 && counts_binding["qualified"]==true && grouped.grouping_version==GROUPING_VERSION && grouped.group_map_sha256.is_some(),"clean_correction_regression":corrected_mean>=-0.01,
            "complete":false,"reason":"Personal and correction fixture gates and representative final evaluation are reported separately."},
        "frequency_baseline":"core frequency_score: observed train/add-one only when compiler supplies counts; otherwise ARPA fallback. Qualification requires a pipeline package with bound frequencies sidecar.",
        "frequency_counts_sidecar_sha256":counts_binding["sidecar_sha256"],"frequency_counts_binding":counts_binding,
        "next_word":{"denominator":next_n,"top1_hits":top1,"top5_hits":top5,"reciprocal_rank_sum":rr,"top1":ratio(top1 as f64,next_n),"top5":ratio(top5 as f64,next_n),"mrr":ratio(rr,next_n)},
        "prefix_completion":{"denominator":prefix_n,"hits":prefix_hits,"recall":ratio(prefix_hits as f64,prefix_n)},
        "candidate_space_coverage":{"definition":"coverage of ALL eligible exact candidate edits; not desired-gold recall or oracle-best5","query_slice":"clean_prefix_completion","denominator":if oracle_partial==0 {Some(oracle_total)}else{None},"hits":if oracle_partial==0 {Some(oracle_hits)}else{None},"coverage":if oracle_partial==0 {ratio(oracle_hits as f64,oracle_total)}else{None},"inconclusive":oracle_partial>0,"observed_partial_denominator":oracle_total,"observed_partial_hits":oracle_hits,"oracle_inconclusive_queries":oracle_partial,"candidate_cap_truncated_queries":oracle_candidate_truncations,"operation_truncated_queries":oracle_operation_truncations,"candidate_budget":oracle_candidates,"operation_budget":oracle_operations,"algorithm":"streaming_full_lexicon_exact_v1","truncated_queries":truncated},
        "clean_gold_pre_rank":{"prefix_completion":prefix_gold.report(),"next_word":next_gold.report()},
        "correct_word_displacement":{"denominator":unchanged_n,"higher_scoring_correction_top1":displacement,"rate":ratio(displacement as f64,unchanged_n)}}),
    )
}

fn ratio(numerator: f64, denominator: u64) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator / denominator as f64)
    }
}
