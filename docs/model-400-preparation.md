# Model-400: review first, train only after a separate decision

Only local implementation and **synthetic** tests have run. No real corpus has
been decoded locally, no CI was dispatched, and no training/model approval is
claimed. The private-host guard, runtime, `main.rs`, `lib.rs` and Cargo manifests
are untouched. Primary integrates this uncommitted branch; do not rebase it onto
the separately merged runtime work.

## First-party source boundary

`provenance/tatoeba-cc0-candidate.json` records the Russian option in the official
**Sentences (CC0)** section at <https://tatoeba.org/en/downloads>:

```
https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_CC0.tsv.bz2
bytes: 516148
sha256: 3b6645bbc18e4e3444ebc9195c4a6c41ff521a8bc4306fec8fcc217ae734db76
Last-Modified: Sat, 31 Oct 2026 06:41:23 GMT
```

The compressed bytes were previously hashed with a hard 1 MiB streaming cap,
without saving or decoding them. The official four-column schema is ID, language,
text, modified date. Detailed CC-BY exports instead have six columns: ID, language,
text, author, created date, modified date. Export generation is weekly Saturday
06:30 UTC; the URL is mutable and a changed hash must fail.

Primary approved this **source/license declaration and pinned input identity**,
not content safety, raw-blob archival, training or distribution. These are public
sentence examples, not private correspondence, personal history or clinical/
pediatric records. CC0 has no title/non-infringement/privacy warranty; see its
[legal code](https://creativecommons.org/publicdomain/zero/1.0/legalcode).
Code's GPL does not relicense corpus data. No CC-BY fallback is admitted to this
CI recipe without a separately reviewed source/policy change.

## Blind evaluation exclusions: exact schema for M3

Curator JSONL input (one record per line):

```json
{"id":"record-1","group_id":"group-1","text":"SYNTHETIC CURATOR EXAMPLE"}
```

M3, **not the training agent**, runs:

```sh
cargo build --locked -p predictor-cli --example make_eval_exclusions
target/debug/examples/make_eval_exclusions SOURCE_SEAL_SHA256 NEW_INDEX_JSON < CURATOR_JSONL
```

Primary must register both `prepare_source` and `make_eval_exclusions` examples
with `required-features = ["tooling"]` when integrating the W Cargo changes.
This owner does not edit `Cargo.toml` or rebase the shared uncommitted work.

`SOURCE_SEAL_SHA256` is curator-supplied metadata binding the source seal; this
helper does not open a FINAL seal or dataset path. It writes a new hash-only JSON
file, no stdout text and no raw-target errors. Unknown input fields fail closed.
Whole messages and nonempty `.?!` segments use the **same public Rust `family`**
function as preparation: NFC, lowercase, distinct ё/е, space-delimited normalized
tokens. Identical normalized whole/segment entries within a group are deduplicated;
a repeated family across different groups is rejected. Segment IDs append
`#segment-N` (zero-based original split position). Output families are ID-sorted.

The exact output/index contract, with no extra fields:

```json
{
  "schema": "linka-eval-exclusions-v1",
  "normalizer": "nfc-lower-v1",
  "source_seal_sha256": "<64 lowercase hex>",
  "families": [{
    "id": "record-1",
    "group_id": "group-1",
    "sha256": "<SHA256 of family(text).UTF8>",
    "token_count": 3,
    "shingles": ["<64 lowercase hex>"]
  }],
  "near_duplicate": {"width": 3, "numerator": 4, "denominator": 5}
}
```

Shingles are a **set**, width `min(3, token_count)`, of consecutive normalized
tokens joined with one ASCII space. Each hash is SHA-256 of:

```
b"linka-eval-shingle-v1\0" + width_as_u32_little_endian + shingle.UTF8
```

Short families therefore cannot collide with a different-width hash domain.
Near matching uses fixed integer `5 * intersection >= 4 * union` (Jaccard >=4/5),
never FINAL-based tuning. An inverted shingle index restricts comparisons to
posting-list hits; it does not scan every source family against every target.

Before creating a preparation directory, reject wrong schema/normalizer/policy,
unknown fields, invalid hashes, duplicate IDs/family hashes/shingles and limits:
16 MiB index/input JSONL, 20000 families/records, 4096 tokens/family, 1000000 total
shingles. IDs/group IDs are 1..128 ASCII bytes `[A-Za-z0-9_.:#-]`; leave room for
generated segment suffixes. Family shingle cardinality cannot exceed its number
of windows. Empty index is supported for synthetic fixtures, **not a FINAL gate**.
Hash-only inputs are not cryptographic anonymization or a proof that a curator
computed every hash correctly; use the shared helper and retain its source seal.

## Preparation and review filtering

Historical `prepare(raw, out, plaintext_holdout)` and `lp prepare` remain unchanged
for legacy detailed Tatoeba callers. Explicit `prepare_with_source` now requires
the JSON hash index; there is **no plaintext fallback or invented source default**.
`review_with_source` uses the same ingestion/filter/component logic but emits no
train/DEV/reserved text, frequencies or attribution. Its private SQLite working
file still contains normalized public input and must never be a review artifact.

```sh
cargo build --locked -p predictor-cli --example prepare_source
target/debug/examples/prepare_source review SOURCE_MANIFEST RAW NEW_OUTPUT HASH_INDEX_JSON
target/debug/examples/prepare_source train  SOURCE_MANIFEST RAW NEW_OUTPUT HASH_INDEX_JSON
```

Do not run these on a real corpus locally. Synthetic fixtures only on the Mac.
Both explicit paths verify source profile/license/evidence/bytes/hash before
decoding. Decode limits are 64 MiB total, 100000 rows and 64 KiB/line; exhaustion
fails rather than truncates. CC0 authors are null; dates and IDs are preserved
when training attribution is emitted. No other data is relabeled Tatoeba.

Whole-message and segment exact/near matches plus conservative direct email,
URL and phone-like identifier screens mark original sentence IDs. **Build the
complete original-ID/duplicate-family graph first**, then exclude every connected
component containing any marked original. No connected duplicate sibling reaches
train counts. Remaining deterministic 90/5/5 splits retain group maps and train-only
unique-family frequencies. Review and train must reproduce the same excluded IDs.

`report.source_filter` binds the index hash, source seal and review mode. It reports
exact/near original counts (exact takes precedence), direct privacy original count,
and `excluded_ids: {source_id: [reason,...]}`. Reasons are `eval-exact`, `eval-near`,
`direct-email`, `direct-url`, `phone-like-identifier`, `component-connected`.
Privacy counts may overlap the independent evaluation-match category; legacy
plaintext holdout counters are not reused for these new semantics.

No ordinary need/pain vocabulary is banned or mislabeled a clinical record.
These screens have false positives and false negatives: dates/numeric examples
may look like identifiers; names, addresses and sensitive narratives can escape.
They are **not perfect privacy review**. Fixed token-shingle matching is not
semantic/translation isolation; source train/DEV near overlap is not measured.

Only new directories/databases are written; no existing SQLite migration/resume.
New code can read old preparation, but old compact-credit builders must not read
new CC0 reports because they hardcode CC-BY. Immutable code-bound run directories
are mandatory; LPKv1/runtime state is unchanged.

## Manual CI modes, inputs and artifacts

`model-candidates.yml`: `mode=review|train`, **default review**, `contents: read`,
no user secrets, push, release or deployment. One non-cancelling concurrency group.
Both data-processing modes use the same dedicated unit: 6 GiB memory, no swap,
200% CPU quota, 45-minute lifetime, 128 tasks and 1 GiB individual file cap;
overall job timeout 60 minutes. No GPU/rented server. Runner disk is not asserted
to have a separate hard quota. Full recipe execution refuses macOS/non-CI.

Review inputs: `source_manifest` (default pinned candidate) and `exclusion_file`
(tracked hash-only M3 index). No content approval is required or generated.
Review checks bytes before decode, runs only bounded Rust preparation/review,
and uploads `review/review.json` plus sanitized JSON/resource receipts. It does
**not** build KenLM/ARPA/models or upload raw source, SQLite, texts, stdout/stderr.
The report explicitly records `content_reviewed=false`, `raw_blob_uploaded=false`.

Train additionally requires `source_review` and `review_report`, separately
reviewed **tracked** files. Copy `review.json` from the completed review artifact;
do not manufacture a successful review. The exact approval JSON keys are:

```
schema: "linka-model-content-review-v1"
status: "approved-for-ci-training-only"
source_sha256: pinned compressed source hash
evidence_sha256: source manifest's official-page evidence hash
exclusions_sha256: exact exclusion-index file hash
source_seal_sha256: review report's source seal
preparation_code_sha256: review report's code binding (must still match current code)
excluded_ids_sha256: review report's excluded-ID/reason mapping hash
review_run_id: "<review GITHUB_RUN_ID>:<review GITHUB_RUN_ATTEMPT>"
review_report_sha256: exact review.json file hash
reviewer: nonempty responsible reviewer
reference: nonempty human decision/evidence reference
content_reviewed: true
raw_blob_artifact_approved: false | true (separate permission, not required for training)
```

No real approval file is supplied. **Only the human reviewer** may set these two
booleans after examining actual review evidence and residual risks. Unknown keys,
missing/pending approvals and mismatched source/exclusions/seal/run/code/report
bindings fail closed. Code binding covers preparation/index/tokenizer/helpers,
lock/toolchain/manifests and this workflow/recipe, but not approval documents, so
committing a decision alone need not invalidate its review. JSON duplicate keys
are rejected. Excluded-ID mapping hash uses compact sorted-key UTF-8 JSON
(`json.dumps(..., sort_keys=True, separators=(",", ":"))`).

**No raw upload before that decision.** Review's worker-local pinned blob is
ephemeral. Train re-fetches and verifies identical bytes. Only an explicit
`raw_blob_artifact_approved=true` additionally stages the owned
`model-approved-source-RUN-ATTEMPT` artifact bound to the approval/review.
With `false`, the original corpus stays worker-local and is not uploaded; filtered
training outputs do not grant permission to redistribute the original raw corpus.
Actions artifacts in a public repository are not a private-data store. An approved
archive is not a release and expires after seven days. If upstream replaces
the weekly file before approval, fail: a new source pin/review is required, not
silent substitution. Archive approval must not authorize private data ingestion.

## Candidate failure matrix and receipts

The first, still unpublished artifact contract is **`linka-model-candidates-v1`**.
Canonical names only: `U.lpk`, `T.lpk`, `U.dev.json`, `T.dev.json`,
`U.receipt.json`, `T.receipt.json`; no legacy filename aliases or migration path.
DEV reports stay private until their candidate completes. Failed T has neither
`T.lpk` nor `T.dev.json` in the candidate bundle.

`provenance.json` contains `schema`, `source_revision`, `preparation_code_sha256`,
`source_sha256`, **`source_manifest_sha256`**, **`exclusions_sha256`** (plural),
`review_run_id`, `train_sha256`, `frequencies_sha256`, `lp_sha256`, `tool_versions`,
`arpa_sha256`, `files_sha256`; `quality_approved`, `redistribution_approved` and
`final_evaluated` are explicitly false. Existing external-tool/environment metadata
is retained. `source_manifest_sha256` is the digest of the copied source manifest
and equals `files_sha256["source-manifest.json"]`. The file map hashes every bundle
file except `provenance.json` itself (no recursive self-hash).

Actual completed receipt fields are:

```json
{"state":"COMPLETED","child_reaped":true,"code":0,"model_sha256":"<64hex>","bytes":12345}
```

`bytes` is the exact unpacked file size; `model_sha256` must equal the corresponding
`files_sha256["U.lpk"|"T.lpk"]`. The receipt and DEV JSON are themselves included
in the file map. U must complete. Only completed candidates may appear in
`arpa_sha256` or be selected; a failed T receipt is never a model qualification.
The producer validates all of these bindings before completing the job.

Failed T receipt fields are `state="FAILED"`, `child_reaped=true`, nonzero integer
`code`, `log`, `log_sha256` (null for a policy failure without a diagnostic file).
Signals/uncertain outcomes use CANCELLED/UNRECONCILED, never this recoverable case.

Training builds **U first** from independently recomputed train-only counts:
lexical count probabilities, train-sentence EOS count, one unknown pseudocount,
non-predicted BOS. Existing `lp compile ARPA NEW_LPK FREQUENCIES -` and grouped
`lp evaluate MODEL DEV NEW_JSON 250` are used. U model + COMPLETED receipt are
mandatory before any candidate artifact is uploaded.

T is optional external MKN3, official KenLM commit
`4cb443e60b7bf2c0ddf3c745378f76cb59e254e5`, `lmplz -o 3 -S 2G -T OWNED_SORT`.
No pruning, vocabulary cutoff, neural framework or discount fallback. Optional
checkout/dependency failures do not prevent U. A known nonzero reaped T operation
records `T.receipt.json: FAILED` with code/log/hash/reap evidence; valid U remains
eligible for DEV selection. No fabricated T scores or successful placeholder.

| Outcome | Job/candidate result |
| --- | --- |
| Preparation or U fails | Whole job fails; no candidate publication |
| U completes, T known/reaped failure | U retained; T FAILED; job may complete |
| Child signal, cancellation, RUNNING/unreconciled receipt | Whole job cannot succeed |
| Candidate >134217728 unpacked bytes | U fails job; T records failure; no automatic retry |

Each command has PID, argv, code, wait/reap state, elapsed time and resource logs.
Signal/cancellation handling stops only its owned process group; workflow cleanup
stops only its exact run-specific systemd unit. Uncertain child outcome is never
an optional failure. No automatic resume/retry. Private diagnostic stderr is
archived only in **train** mode with separate raw-archive approval, never review. Candidate archives retain
U, optional successful T, receipts, DEV metrics, original credit metadata/notices,
source/review/code/tool/model hashes; a separate source archive binding exists only
when its upload was explicitly approved. Without it, durable-source reproduction
remains unresolved and is not silently declared complete.
No FINAL data is read, generated, tuned against or evaluated here.

Linux execution itself, real coverage/whole-bundle budgets, semantic/translation
isolation, Windows CPU/8 GiB latency and AAC acceptance remain separate gates.
The evaluation owner supplies a new independent FINAL with >=500 groups.

## Focused synthetic proofs

```sh
python3 scripts/model_recipe.py --self-test
cargo test --locked -p predictor-cli --lib --test prepare --test budget --test checkpoint
cargo build --locked -p predictor-cli --example prepare_source --example make_eval_exclusions
actionlint .github/workflows/model-candidates.yml
```

Rust red tests preceded the hash/index and review/component implementation. Python
red tests preceded optional-T handling and stricter review/run approval binding.
Fixtures cover integer 4/5 boundaries, short-width domains, validation, no target
text output, transitive component exclusions, identifier screening without a pain
word ban, legacy compatibility, required U/optional failed T and interrupted T.
