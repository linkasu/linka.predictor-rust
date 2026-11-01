# Native CLI pipeline contract v1

Historical pipeline protocol: private-host training and qualification are disabled
in the public checkout. Preparation, compilation and offline inference remain available.

One Rust CLI (`lp`) orchestrates prepare, external KenLM estimation, portable package compilation, evaluation and checkpoint resume. External C++ KenLM is a build tool only. All training/reference scoring and final benchmarks must run on `book.local`; refuse a local trainer invocation. Native pipeline logic is Rust, not a Python/shell implementation hidden behind a Rust launcher. Bootstrap operations may use shell/curl.

Required stages and artifacts:

1. Acquire: cached pinned source bytes, `.part` on incomplete downloads, expected SHA-256 validation before promotion; explicit provenance/attribution. An offline run never acquires network resources.
2. Prepare/split: stream detailed Tatoeba TSV (possibly bz2), keep source sentence IDs/authors and license links in an attribution sidecar; normalize through shared core; preserve negations/short Russian utterances and ё/е. Group duplicates before a deterministic 90/5/5 split, exclude household holdout families before train-only frequency counts. Report counters/overlap, split hashes and memory. Reject reserved KenLM markers from source. Vocabulary cutoff is forbidden in v1.
3. Train: exact pinned KenLM executable, MKN trigram, external sort buffer 6GiB, one heavy slot on book. Capture command/stdout/stderr/exit/time/RSS, verify real output before atomic promotion. Tiny fallback discount settings must be explicitly recorded, not silently used to claim ordinary full-data estimation.
4. Compile: ARPA plus train-only lexical frequencies, optional OpenCorpora wordforms, resolved config and provenance produce `.lpk`; artifact hash and mandatory notices are recorded. OpenCorpora contributes lexical candidates only, not fake contextual frequencies.
5. Evaluate: call the same runtime and serialize public responses; independent UTF-16 application, paid-action KSR for selection cost1/2/3, clean and synthetic errors separately. Frequency-only, trigram, letters, spaces and personal ablations use identical phrase groups. Test stays unopened until frozen manifest.
6. Resume: bind checkpoints to code, source, config, tools and upstream artifact hashes; partial files never count as complete. A second active run cannot claim the heavy slot. Corrupt output or changed dependency invalidates descendants. Do not kill unrelated processes or automatically replay an operation with unknown outcome.

Commands must include direct diagnostics (`prepare`, `train`, `compile`, `complete`, `score`, `evaluate`, `benchmark`, `personal-replay`) and one full `run`/`resume` path. CLI syntax may be kept minimal and documented in README; no daemon, plugin system or deployment machinery.

The run directory contains resolved config, manifest, source/split locks, immutable model, notices/attribution, logs and metrics. A complete artifact records exact source tree/revision, binary/model/config/split/evaluator hashes. Resource and qualification evidence is measured separately, never generated as a success placeholder.
