# Native CLI v1

Build: `cargo build --locked --release -p predictor-cli`; binary `target/release/lp`.
All paths below are positional; `lp help` lists commands. The public checkout
disables private-host training and qualification: its host guard always fails.
The historical pipeline protocol below does not provide a public training setup.

```sh
lp acquire CACHED_SOURCE OUTPUT EXPECTED_SHA256
lp prepare RAW_TSV_OR_BZ2 OUTPUT_DIRECTORY HOUSEHOLD_HOLDOUT
lp lexicon OPENCORPORA_XML_OR_BZ2 WORDS_OUTPUT
lp train CONFIG TRAIN_TXT NEW_OUTPUT_DIRECTORY
lp compile ARPA OUTPUT_LPK [FREQUENCIES_TSV|-] [EXTERNAL_WORDS|-]
lp complete MODEL QUERY [LIMIT] [PERSONAL_STORE]
lp explain MODEL QUERY [LIMIT] [PERSONAL_STORE]
lp score MODEL SENTENCE
lp run CONFIG NEW_RUN_DIRECTORY
lp resume CONFIG RUN_DIRECTORY
lp evaluate MODEL DEV_PHRASES OUTPUT_JSON [MAX_GROUPS] [FROZEN_TEST_MANIFEST]
lp benchmark CONFIG MODEL QUALIFICATION_INPUTS_JSON NEW_OUTPUT_DIRECTORY
lp personal-replay MODEL EVENTS_JSONL GENERAL_PHRASES NEW_OUTPUT_DIRECTORY
```

## Preparation / attribution

The historical `lp prepare` entry point below remains six-column Tatoeba/CC-BY.
An explicit-source API and standalone `prepare_source` example additionally
support the official four-column Tatoeba CC0 export, with hash/license/schema
checks before decoding and no fabricated author. See the
[model-400 preparation boundary](../docs/model-400-preparation.md) for profiles,
first-party candidate evidence and the separately approved manual Linux CI recipe.
Neither entry point grants legal, model-distribution or AAC approval.

The explicit source API requires the curator-generated `linka-eval-exclusions-v1`
JSON index (no FINAL plaintext); legacy `lp prepare` keeps its plaintext holdout.
`prepare_source review|train MANIFEST RAW NEW_OUTPUT HASH_INDEX` exposes the new
contract separately from this legacy CLI. Manual model CI defaults to review:
only hash/ID/statistics artifacts, no corpus, ARPA, model or automatic approval.
Training requires a separate source/exclusions/run-bound human decision.

Acquisition is deliberately **cached/offline**: validates the supplied SHA-256,
verifies every existing `.part` prefix byte before resuming a copy and atomically
promotes only a complete matching file. No implicit network fallback. Initial
approved downloads are bootstrap operations recorded under provenance/evidence.

Preparation streams detailed six-column TSV, optionally through Rust's bz2
decoder. SQLite with 8 MiB page cache and disk-backed temp storage owns family
deduplication, attribution and train word counts. Source lines exceeding 64 KiB
fail the stage, not silently filter a record. Malformed/non-Russian/authorless
rows and reserved markers have separate counters. Sentence boundaries `.?!`
are split; segment identity retains original sentence ID, author and license.
Families are shared-core normalized word sequences (case/punctuation variants,
without conflating ё/е). Ingestion first records all eligible segments in SQLite.
If any segment matches holdout, **every segment of that original sentence ID**
is excluded before constructing the graph or counting frequencies, including
other rows carrying that ID. `holdout_matches` counts exact matching occurrences;
`holdout_related_excluded` counts its nonmatching sibling segments;
`holdout_excluded` is their total, with `holdout_original_records` separate.

The split unit is a connected component of the bipartite original-ID ↔ normalized
segment-family relation. All segments of one original and every duplicate family
therefore stay together, including transitive links through another original.
SQLite retains the relation; an integer union-by-size/path-compression DSU uses
two vectors with a hard **64 MiB reservation cap**, plus SQLite's 8 MiB page cache.
Budget exhaustion fails clearly; it never drops families. Component ID is the
minimum SHA-256 of any member normalized family, encoded as lowercase 64-character
hex. Bucket is the first u64 little-endian of
`SHA256(b"linka-components-v1\0" + component_id.as_bytes())` modulo 100:
train 0..89/dev 90..94/test 95..99. Assignment and emitted files are independent
of input row order. LM sentences remain separate lines; components are not
concatenated into artificial training sentences.

Outputs: `train.txt`, `dev.txt`, `test.txt`, matching `train.groups.tsv`,
`dev.groups.tsv`, `test.groups.tsv`, `frequencies.tsv`, `attribution.jsonl`,
`families.sqlite`, `report.json`. Each groups file has exactly one line for each
phrase in its corresponding txt, in the same order, with no header:
`SHA256(normalized_phrase)\tcomponent_sha256\n`. Reports declare
`grouping_version="duplicate-original-components-v1"`, `family_counts` and
`independent_groups` maps keyed train/dev/test, plus independently checked
`original_overlap`, `component_overlap`, and duplicate-family `split_overlap`.
Legacy `train/dev/test` counts are family counts, **not independent group counts**.
All six text/groups files are checksum-bound in report and pipeline checkpoints.
Counts use unique **train** families only; no vocabulary cutoff. Token maximum
byte length and occurrences exceeding 16384 bytes are reported, never truncated
or discarded; an unsupported token subsequently fails package compilation.
The sidecar includes attribution for all accepted split records, not just train.
The report includes split SHA-256 values and measured process peak RSS (not heap;
earlier activity in the same process may contribute). Compiled bundles retain all
author records in `attribution.jsonl.bz2` in the compact format below. Verbose audit
rows retain original sentence ID, author, segment, family, component, split and
source/license links. Preparation before this grouping fix, including book
`T06-native-prepare-01`, is **source_old / unqualified**: its zero overlap counter
did not check original-document isolation. Do not reuse those splits or treat
their segment counts as independent groups. Pinned raw source bytes remain valid.

### Common attribution and unpacked budget

Common credits are bzip2-compressed **compact JSONL**, not the verbose audit. The
first line declares `linka-compact-attribution-jsonl-v1`, its source/license table,
global `source_ref=0` (Tatoeba), row fields and full verbose `audit_sha256`.
Each following row is `[sentence_id, author, segment, family_ref, component_ref]`;
every original attribution row is retained, including duplicates and every author.
Source URL is losslessly resolved by the header's sentence-ID URL template;
license ID/link are shared rather than repeated. Family references are 1-based
binary-ordered family labels, component references are 1-based SHA256-ordered
component labels in the hash-bound audit. They retain equivalence/group relations;
the verbose phrase strings/URLs are not repeated in the common pack. Full verbose
JSONL and its SQLite relation remain in run/cache, unmodified. Model provenance
binds raw-source SHA-256 and verbose-audit/group-file hashes; compact header binds
the same retained audit. Old grouping versions are rejected by this publication
path, not silently upgraded.

The **500000000-byte budget counts the entire unpacked common directory**,
including credits, notices, model and ancillary files. `directory_bytes` keeps
its signature but returns unpacked bytes; `bundle_sizes` exposes physical and
unpacked totals separately. Every member of every `.bz2` stream is decoded with
a fixed 64 KiB buffer and a remaining-budget bound. Corruption, overflow, symlinks,
special files and exceeding the limit fail explicitly. Both compile publication
and benchmark enforce this bound. A 187-byte compressed credit stream expanding
to 192000 bytes is charged 192000 bytes, not 187. Compact **expanded** representation
is the saving; compression is not a budget waiver. Real full-model size is still
unknown and may fail even with compact credits; no full-model fit is claimed.

OpenCorpora extraction streams `<l t>` and `<f t>` into disk-backed normalized
word deduplication. It produces lexical candidates only, not invented context
counts. `compile` can accept the resulting word list. The current real-source
HTTP 522 **still blocks the required real OpenCorpora on/off ablation**; synthetic
parser tests are not a substitute. Baseline config includes Tatoeba only.

## Checkpoints / trainer safety

`run CONFIG NEW_RUN_DIRECTORY` exclusively claims a **nonexistent** directory
using `create_dir`. Its parent must already exist; parents are never recursively
created. Any existing root is rejected, including an empty directory, file or
symlink. When an operation wrapper already owns an audit directory, pass a new
child directory for the run, not that occupied audit directory. A later failure
does not automatically delete or force-reuse the reservation.

`resume CONFIG RUN_DIRECTORY` requires an existing real directory and a readable
regular `manifest.json`. Missing roots/manifests are errors, never initialization.
Admission validates deserialization, nonempty identity, recognized state and
well-formed active-stage/directory tracking and checkpoint metadata **before any
write**, including creation of `run.lock`. Invalid or unmanifested UNKNOWN roots
are left unchanged for owner reconciliation. A valid manifest is reread under
the run lease before decisions, so admission does not use a stale pre-lock copy.

The historical `run` protocol performs cached acquire → prepare/split → MKN trigram → compile → dev
evaluation. Private baseline configuration is not distributed. Tiny config explicitly enables
`--discount_fallback`; baseline does not. Both use `-o 3 -S 6G`, no pruning.

Every attempt has a new directory. An atomic manifest references completed output
hashes only. Identity binds resolved config, raw source, holdout, source-tree files
(`crates`, `spec`, root manifests/lock/toolchain), actual executable, KenLM binary
and provenance tree. Per-stage identity chains upstream checkpoint identity and
output hashes. Changed identity/corruption invalidates dependent stages. Failed
and signalled/cancelled trainer receipts remain distinct; partial output is never
accepted. Old attempts are retained, not deleted. A second lp cannot claim the
same run's OS file lock.
Cancellation intent is an explicit `cancel.requested` marker in the owned trainer
attempt directory. The marker alone neither signals a process nor proves its
termination. Only an independently authorized caller may stop the verified PID.
After an actual wait result, the worker distinguishes requested cancellation
from an unrequested crash (failed); a successful exit remains completed even if
the request raced with completion. Exit code and signal are always preserved.

Training and benchmarks verify macOS hardware UUID of the authorized book.local
machine. A shared OS `flock` lease guards `book_root/runs/heavy.lock`. The worker
and the **actual KenLM child** inherit the same open descriptor; only async-signal-
safe `fcntl` clears CLOEXEC in `pre_exec`. Killing the lp parent cannot release a
live orphan trainer's lease. PID/process start identity and command/stdio paths
are durable. An independent worker waits using `wait4`, writes exit/signal/RSS/
CPU receipt and validates complete trigram ARPA before promotion. On resume a
busy OS lease blocks retry, regardless of parent PID. If the worker itself dies,
the actual child still owns the lease; absence of a receipt is **unknown**, not
success. Receipt verification, not lease release, determines the permitted next
action; an unverified partial ARPA is never an accepted checkpoint.
**A free lease is not an outcome.** Active train without a valid terminal receipt
returns `reconcile_required` before dispatch, retaining `active_stage`,
`active_directory`, old identity and checkpoints even when inputs changed.
Missing/malformed/inconsistent receipts or incomplete ARPA are never replayed
automatically. Explicit failed/cancelled receipts remain separate terminal states
and return an error requiring a distinct explicit attempt. Only a verified
completed receipt (exit 0, no signal, complete trigram ARPA), after lease release,
can be adopted for matching identity; a known completed stale identity can then
be invalidated normally. No PID absence, timeout, or lease status substitutes for
terminal receipt verification.

### Private replay publication

Only the personal replay report uses private atomic publication: a fresh
`<stem>.<pid>.private.part` file is created with mode 0600 **before writing any
query/event data**, synced, then renamed over the final report. A replacement
also becomes 0600; a pre-existing temporary path fails without changing either
it or the previous report. Failed partial files remain private and are not
automatically discarded. Snapshot privacy remains the core's responsibility.
No global umask or permissions of ordinary shared reports are changed. Personal
CLI stdout contains the explicitly requested report; callers must protect any
external redirection themselves.

## Evaluation and qualification

KSR uses a JSON round-trip of the real public Response and an independent UTF-16
client applicator. Typed graphemes/spaces cost one action; selection costs 1/2/3
are separate. Empty q yields no free suggestion. Only useful target-prefix
extensions saving more typing than selection cost are chosen. Reports retain
per-phrase values, unweighted mean, aggregate paid-action KSR, paired bootstrap
CI (10000 resamples, xorshift seed 906), next-word top1/top5/MRR numerators and
denominators, prefix completion, full-scan candidate-pool oracle and truncations.
Synthetic insert/delete/substitute/transpose/split/join/mixed correction recall
is separate; negative controls compare correction scores against unchanged_score.
Run-stage dev evaluation and the direct command do not evaluate final test.
Both direct evaluation and the pipeline stage use `read_grouped_phrases` followed
by `evaluate_grouped`, preserving the `GroupedPhrases` object through CI. The
bounded sample selects the lowest component SHA-256 IDs over the whole input
(default maximum 250 components), retaining **all phrases of each selected
component**. Selection protocol is `lowest-component-sha256-whole-file-v1`.
The aligned `.groups.tsv` is verified and hashed. Inputs without a map are
explicitly `normalized_phrase_fixture_unqualified`, not qualified corpus groups.
CI/denominators must not flatten a component into independent segment samples.
The pipeline supplies actual train counts; arbitrary low-level packages without
counts may use the core's ARPA-unigram fallback and cannot claim the count-baseline
gain gate. The report identifies the bound frequencies sidecar when present.

Final test requires the optional frozen manifest (automatically required for a
filename containing `test`; callers must also supply it for renamed test files).
It binds `model_sha256`, `binary_sha256`, `phrases_sha256`, `evaluator_sha256`,
`source_tree_sha256`, `resolved_config_sha256`, `pipeline_identity`, `config_path`,
`policy_path`, `policy_sha256`, plus `maximum_groups`, `selection_protocol`,
`grouping_version`, `grouping_mode`, `group_map_sha256`. Fixture mode still requires
an **explicit null** group-map hash. Identity and selection/group bindings are
checked **before** claiming the final test.
An exclusive adjacent `*.evaluation-claim.json` prevents automatic repeated test
evaluation; interrupted/unknown claims require owner reconciliation.

Benchmark protocol uses the frozen synthetic `configs/qualification-inputs.json` map,
never invents/tunes prompts. Five modes completion/next_word/letters/spaces/mixed,
limits 5 and 100 separately, each 50 warmups + 1000 measurements. Every timing and
actual query is retained in `timings.jsonl`; workload/model/binary hashes, p50/95/99,
open validation and first query, RSS, power and load are reported. Release binary
and exclusive book slot are mandatory. Heap is explicitly null without allocator
instrumentation; file size is not memory. OS page cache is not forcibly flushed.

Personal replay JSONL rows contain `event` (ConfirmedEvent), `query`, `expected`.
Each query is measured before learning; only then is its explicit causal event
observed and queried again. Save/reopen, immutable global hash and before/after
general-set KSR are reported. Use a declared new OOV probe and explicit repeated
events to demonstrate availability and idempotence; no success is fabricated.
General replay uses the same whole-component selection reader. It retains input/
map hashes, grouping version/mode, selection protocol, maximum groups and separate
component/phrase counts in its private report. Its current before/after statistic
is per-phrase KSR at selection cost 1 within those selected whole components, not
a newly claimed personal CI or complete cost-1/2/3 study.

Quality reports deliberately do not claim complete qualification: the parent
must combine >=200 held-out groups, CI, regression limits, independent fixtures,
OpenCorpora ablation, personal replay and resource/delivery gates. Tiny runs are
functional evidence, not representative quality. Direct low-level compilation
without source sidecars is not a redistributable package by itself.
