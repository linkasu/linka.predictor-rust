# Runtime API v1 (frozen integration surface)

Crate `predictor-core`, Rust 1.98.1; error type `Box<dyn Error + Send + Sync>`.

```rust,ignore
Model::compile_arpa(input: impl AsRef<Path>, output: impl AsRef<Path>, metadata: &PackageMetadata) -> Result<()>;
Model::load(path: impl AsRef<Path>) -> Result<Model>;
Model::score(&self, context: &[&str], word: &str) -> f64; // log10, backoff, OOV
Model::score_sentence(&self, words: &[&str]) -> f64; // BOS and EOS
Model::frequency_score(&self, word: &str) -> f64; // add-one train counts, ARPA fallback if no counts
Model::prefix_top(&self, prefix: &str, limit: usize) -> Vec<String>;
Model::vocabulary(&self) -> impl ExactSizeIterator<Item = (&str, u32, bool)>;
Predictor::load(path: impl AsRef<Path>) -> Result<Predictor>;
Predictor::complete(&self, request: Request<'_>, personal: Option<&PersonalModel>) -> Result<Response>;
Predictor::explain(&self, request: Request<'_>, personal: Option<&PersonalModel>) -> Result<Completion>;
```

`PackageMetadata { provenance: serde_json::Value, attribution: serde_json::Value }`
implements Default. Compiler accepts KenLM ARPA up to order 4; no training here.
It also has `external_words: Vec<String>` (default empty), incorporated into the
sorted lexicon with an external-only flag and OOV LM score.
`word_counts: BTreeMap<String, u32>` (default empty) supplies train-only counts
for frequency prefix ranking. Missing/zero counts use ARPA unigram probability
as an explicitly approximate frequency proxy; they are never invented counts.
Counts must use shared normalized keys. Raw corpus counting stays in CLI.
`vocabulary()` streams sorted mmap-backed `(normalized_key, observed_count,
external_only)` records without copying vocabulary/ngrams. Includes special
tokens. `is_supported_external_word(&str) -> bool` is the shared admission policy
for an already-normalized external wordform (alphabetic, 1..16,384 UTF-8 bytes).
This external dictionary policy is never a train vocabulary cutoff.
`Request { q: &str, lang: &str, limit: usize }` (`Request::new(q)` defaults ru/5).
`Response { text: Vec<String>, pos: i32, end_of_word: bool }` serializes
`endOfWord`. `Completion { response, candidates: Vec<Candidate>, pool: Vec<Candidate>, unchanged_score: f64, truncated: bool }`.
`Candidate { edit: TailEdit, score: f64, letter_edits: u8, space_edits: u8 }`.
`TailEdit { start_byte: usize, replacement: String }`; `apply(q)` validates boundary.
`project(q, edits: &[TailEdit]) -> Result<Response>` shared UTF16 legacy span.

`normalize(&str) -> String` NFC/lowercase, no ё conflation.
`tokenize(&str) -> Vec<Token>`; `Token { normalized: String, start_byte, end_byte }`.
`PersonalModel::default()`, `load(path)`, `save(path)`, `observe(ConfirmedEvent) -> Result<bool>`,
`clear()`, `words() -> impl Iterator<Item=(&str, &PersonalCounts)>`.
`ConfirmedEvent { event_id: u64, utterance_id: u64, origin: EventOrigin, text: String }`;
origin `Confirmation` or `Selection`. `PersonalCounts { confirmed_count: u32,
selection_count: u32, last_event: u64, surface: String }`. The optional serialized
`surface` defaults to empty for old stores. Only confirmation records the most
recent original token spelling; search/ranking still use the normalized key.
Responses use this spelling; the input buffer changes only on explicit selection.
Old binaries ignore the additional JSON field; saving with an old binary loses
display spelling, not counters or event IDs. Loading rejects a surface whose
normalization differs from its key or which is not one bounded token.
Duplicate delivery and same
utterance/origin/word-span deduplicated; different origins have separate counters.
Only explicit observe mutates; save explicitly publishes atomically. Bounds and
retention are documented in text-contract.md. CLI may use serde JSON types directly.

Public structs and enums derive Debug, Clone and serde as appropriate. Model and
Predictor contain read-only mmaps, not heap copies of the ngram tables.

## Ablations / oracle

`Predictor::with_config(self, SearchConfig) -> Result<Self>`;
`Predictor::model(&self) -> &Model`.
`SearchConfig` (serde + Default) fields: `frequency_only: bool` (false),
`letters: bool` (true), `spaces: bool` (true), `external_lexicon: bool` (true),
`full_scan_oracle: bool` (false), `candidate_limit: usize` (128),
`max_operations: usize` (12000), `letter_cost: f64` (1.5), `space_cost: f64` (1.0).
Frequency-only disables context candidate lists and scores add-one-smoothed
observed train word_counts (sum count denominator includes the vocabulary).
Only when the package has no counts does it fall back to ARPA unigrams. It does not
implicitly disable letters/spaces/personal: caller sets those independently.
Full-scan enumerates the lexicon rather than range-max prefix shortlist; set
candidate_limit and max_operations generously for oracle runs, inspect truncated.
`Model::prefix_top_full_scan(prefix, limit)` is an independent exhaustive prefix
ranking oracle. Neither oracle is a latency-qualified production mode.
`Completion.candidates` is selected top-limit. `Completion.pool` contains all
ranked/deduplicated non-no-op candidates before top-limit for candidate recall.
`unchanged_score` is the no-op control at the same scoring origin. Empty or
punctuation-only requests use a zero control and empty lists. `truncated` reports
operation-budget exhaustion (a shortlist alone is not marked exhausted).
