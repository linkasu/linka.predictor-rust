# Text and prediction contract v1

`complete(Request { q, lang: "ru", limit })` is read-only and deterministic.
`q` is valid Rust UTF-8, at most 16,384 **bytes**; limits are 1..100. Empty input,
no lexical tokens, or punctuation after the last token returns empty `text`,
`pos=0`, `endOfWord=false`. Default limit is 5. Unsupported languages, oversize
queries and invalid limits return errors. No cursor-in-middle API exists.

## Normalization and spans

Search uses NFC → Unicode lowercase → NFC. Original bytes are never normalized
in place. Ё and е remain distinct. A substitution between them is a letter-error
hypothesis, not dictionary conflation. Unicode tables are pinned by Cargo.lock:
unicode-normalization 0.1.24 and unicode-segmentation 1.12.0.

Tokens are contiguous extended grapheme clusters whose first scalar is alphabetic;
all other clusters delimit tokens. Each token carries normalized text and the
original half-open UTF-8 byte interval. Numbers/URLs/punctuation do not form
lexical spans and no merge crosses those delimiters. Search can replace only the
last token, or join the last two tokens separated exclusively by horizontal
whitespace (ordinary spaces, tabs and NBSP are preserved). LF, CR, VT, FF, NEL
U+0085, LS U+2028 and PS U+2029 are hard boundaries, never joinable whitespace.
LM context resets across hard boundaries and non-whitespace delimiters and retains only its last
three tokens (model order ≤4). A fresh sentence uses `<s>`. Last-token completion
after a hard boundary preserves every preceding line byte. A query ending in a
hard boundary (without a lexical token on the new line) returns no suggestions;
it never edits the previous line.

Letter hypotheses insert/delete/substitute one Russian scalar or transpose two
adjacent normalized scalars; fuzzy search runs for 3..32 characters only. Exact
prefix completion has no length cap other than the request limit. Split/join
allows one space edit and optionally one letter edit, with two output words at
most in the edited span. Completing the rightmost prefix costs zero letter edits.
Splitting checks that the left word is complete and the right side has a matching
completion. Exact candidates are generated before expensive fuzzy hypotheses.

The operation budget is deterministic (12,000 by default), not a timer. Trace
`truncated=true` means search exhausted it; candidates already found survive.
Candidate pool includes range-max frequency top-128, up to 128 continuations per
context order, personal words, and correction hypotheses. Context shortlists
can lose recall without exhausting the operation budget; compare full-scan oracle.
Oracle enumerates the entire lexicon before budgeted candidate consumption and is
not claimed to obey normal production memory/latency targets.

## Scores and ablations

All alternatives are scored from the same original boundary: earliest editable
token among the last two, with unchanged preceding text included in each tail.
Score is sum of log10 LM conditional probabilities minus 1.5 per letter edit and
1.0 per space edit, plus bounded personal preference. These conservative weights
are policy constants, not fitted probabilities or quality claims. No token-count
normalization. Unchanged input is scored as a control, never returned as a button.
Case-only or NFC-equivalent alternatives are also no-ops and do not occupy a
button; ё/e remain distinct. Fully typed uppercase/decomposed words still permit
next-word append preserving their original bytes; partial completion case
restoration remains out of scope.
Final text is deduplicated; ties use letter edits, space edits, replacement UTF-8
lexicographic order, then original start byte.

Frequency-only disables LM context candidates and scores add-one smoothed
train word counts; with no supplied counts it explicitly falls back to ARPA
unigram scores. Prefix ranking uses positive train counts, then ARPA unigram
probabilities for words without counts; equal priorities use lexicon WordId.
External-only words have no invented contextual counts and use the `<unk>` path.
SearchConfig exposes independent letters/spaces/external/oracle flags.

## Legacy projection

`TailEdit { start_byte, replacement }` replaces through the original end.
Apply rejects non-character or interior grapheme boundaries. For returned edits,
`s = min(start_byte)`; each button is `q[s..start_byte] + replacement`.
`pos = -utf16_len(q[s..])`, or zero for pure append. Spaces are explicit in the
replacement; this implementation never emits the optional `pos=1` shorthand.
Applying the original client's negative JS slice followed by insertion is exactly
equivalent to the edit, including decomposed ё, emoji, case and repeated spaces.
All unaffected original bytes remain unchanged.

`endOfWord=true` means there is at least one completed-word suggestion, and false
means an empty response. This is a **local agreement**, not a verified historical
Yandex field interpretation. Responses serialize this field as `endOfWord`.

## Personal overlay

Only explicit `observe(ConfirmedEvent)` changes the overlay. Confirmation increments
only confirmed_count per token occurrence; selection increments only selection_count.
The boost per word is `min(1.5, .15 log10(1+confirmed) + .30 log10(1+selected))`.
Observe rejects input over 16KiB, ignores words over 256 bytes, saturates counters.
Duplicate event_id does nothing. A second event with the same utterance_id,
origin, normalized word and original word span does not count the action twice.
Different origins remain distinct. Event IDs should be monotonically increasing
for meaningful recency; uniqueness, not timestamps, controls delivery deduplication.

10,000 words maximum; evict smallest total counter, oldest last_event, then word.
At most 20,000 delivered IDs and 10,000,000 serialized bytes. On exhausted ID/byte
retention observe returns an error and leaves the profile unchanged, rather than
silently forgetting IDs and counting retries twice. Call clear explicitly to reset.
Deduplication actions survive word eviction until clear. This bounded replay
window is a deliberate v1 limitation, not unbounded lifetime event processing.

Persistence is an explicit bounded JSON snapshot (`load`/`save`), same-directory
exclusive temporary file, fsync, atomic rename, directory fsync. General model
files are never written by the overlay. Queries cannot save or learn. This v1
snapshot has no multi-writer merge: the application must serialize writers;
concurrent processes must not save the same profile. Save errors are propagated.
