# Portable LINKa package v1

All integers little-endian, fixed width. Probabilities/backoff are IEEE-754 f32,
base-10 logarithms. Strings are UTF-8. No pointers, usize or Rust struct layouts.
Maximum file size: 500,000,000 bytes including metadata. WordId is its zero-based
position in lexicographically sorted normalized vocabulary (UTF-8 byte order).
ARPA compilation is deterministic given identical inputs and metadata. Compiler
holds build tables in memory; only runtime reading has bounded buffers.

## Header (32 bytes)

| Offset | Type | Meaning |
|---|---|---|
| 0 | byte[8] | ASCII `LINKALPK` |
| 8 | u32 | format version = 1 |
| 12 | u32 | model order, 1..4 |
| 16 | u32 | directory entries = model order + 6 |
| 20 | u32 | normalizer version = 1 |
| 24 | byte[4] | `ru\0\0` |
| 28 | u32 | reserved = 0 |

Directory immediately follows header. Each entry is 56 bytes:
u32 type; u32 reserved=0; u64 offset; u64 length; byte[32] SHA-256 of section.
Sections are contiguous, in directory order, no overlap, no trailing bytes.
Section types are unique and all listed below are mandatory. Checksum is integrity,
not authentication. Offsets and lengths must fit the file and host address space.

## Sections

### Type 1: lexicon fixed records (24 bytes each)

u64 offset in type-2 string pool; u32 byte length (1..16,384); f32 unigram log10
probability; u32 external-only flag (0 or 1); u32 train occurrence count.
WordId is implicit record index. String offsets are contiguous in lexicon order.
No implicit NUL terminators. Keys must be NFC/lowercase, strictly sorted, unique.
Unigram probability is used as prefix priority only when positive count is absent.
An absent count is zero, **not** an estimated corpus count.

### Type 2: string pool

Concatenation of normalized dictionary strings, including `<s>`, `</s>`, `<unk>`.
V1 uses normalized forms as output surfaces; it does not restore inferred case.
Original text outside edits remains byte-identical. External wordforms are records
with external-only=1 and OOV probability; words already in the LM retain flag=0.

### Types 11..(10+order): ngrams

Each record: u32 WordId[4]; f32 probability; f32 backoff (24 bytes).
An ngram uses its first n IDs, remaining IDs must be 0xffffffff. Strict ascending
lexicographic WordId[4] order. Probabilities finite and ≤0; backoff finite (may be
positive). Omitted ARPA backoff is zero. Special-token unigrams must exist.
Binary search directly in mmap, then standard longest-context recursive backoff.
Unknown and external-only wordforms map to `<unk>` in targets and contexts.
Sentence scoring includes `<s>` context and `</s>` target, not a `<s>` target score.

### Type 3: continuation shortlist

Each 24-byte record: u32 context[3], u32 next_word, f32 observed ngram probability,
u32 reserved=0. Unused context IDs are 0xffffffff after the used prefix. Lists
contain at most 128 highest-probability observed continuations per context, ties
by WordId. Records globally sorted by (context[0],context[1],context[2],next_word).
Runtime binary-searches a context and unions its list with prefix/personal hits.

### Type 4: range-max index

Let B be next power of two ≥ lexicon record count. Array of 2*B u32 WordIds,
index 0 unused sentinel 0xffffffff. Leaf B+i holds i for ordinary words, sentinel
for special tokens and padding leaves. Internal node stores maximum-priority
descendant ID: positive train count if supplied, otherwise unigram log probability;
ties choose smallest WordId. Compare positive counts above log probabilities.
Prefix lower/upper bounds come from lexicon binary searches. Range-max plus a
priority queue extracts top-k without lexicographic truncation or full scanning.

### Type 5: metadata JSON

JSON fields provenance and attribution. Public build-input `PackageMetadata`
also accepts external_words and word_counts, but these build-only inputs are
compiled into the lexicon/count records and are not duplicated in type 5.
Readers accept their absence (and tolerate older v1 fixtures containing them).
Provenance should include train/split/config/toolchain/source hashes. Attribution
retains source license/author references and/or mandatory notice content; the
pipeline is responsible for supplying complete release metadata. Maximum 40MB.
Compiler enforces the 40MB serialized metadata bound before publishing any output;
failure preserves an existing package. Loader validates JSON streaming and does
not retain it on the runtime heap. Global keys over 16,384 bytes are explicitly
rejected, never silently discarded; the separate personal-word limit remains 256.

### Type 6: policy JSON

Exact UTF-8 bytes (without newline):
```json
{"normalizer":"nfc-lower-v1","letter_cost":1.5,"space_cost":1.0,"continuations":128,"max_operations":12000}
```
Version-1 loader rejects other policy bytes; explicit runtime SearchConfig can
override these defaults for declared ablations. Changing binary meaning needs a
new format/normalizer version, not guessing based on available fields.

## Loading / publication

Compiler writes same-directory exclusive temporary output, fsyncs, renames and
fsyncs directory. Interrupted temporary files are not complete packages. Caller
must not overwrite/truncate an inode while mapped: publish replacements by rename;
retain immutable package files for reader lifetime. Read-only mapping cannot stop
another process modifying an existing inode; violating this rule can cause SIGBUS.

Loader checks header, sizes, directory, SHA-256, normalized sorted lexicon,
finite/valid ordered ngram records and IDs, bounded indices, JSON and policy.
Validation uses streaming buffers and small indexed reads before creating the
read-only mmap; it does not heap-copy the vocabulary or ngrams. Prefix index
leaves/internal maxima are checked against dictionary priority with bounded reads.
Full verification has real first-load time/I/O cost, and warms filesystem cache.
Benchmark must report that cost separately; a post-validation map is not an
unwarmed-storage cold start. No signature/trust or delivery solution is claimed.
