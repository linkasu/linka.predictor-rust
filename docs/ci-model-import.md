# Metadata-only model CI import

`model-candidates.yml` is a main-only, manual Book output importer. It never trains,
reviews content, builds models or grants approval. The sole dispatch input is
`import_manifest`, a repository-relative tracked JSON path in the executing main
workflow tree. No URL, capability, PAT or raw data belongs in this file/input.

The exact `linka-book-candidate-import-v1` fields are:

| Field                                                                 | Type / constraint                                                                          |
| --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `schema`                                                              | literal `linka-book-candidate-import-v1`                                                   |
| `model_source_sha`, `app_tooling_sha`                                 | lowercase exact 40-hex main-history commits in predictor/app respectively                  |
| `source_manifest`, `source_review`, `review_report`, `exclusion_file` | canonical repo-relative `.json` paths, at most 512 characters, no traversal or backslashes |
| `book_release_id`                                                     | positive decimal **string**, safe integer                                                  |
| `book_archive_sha256`                                                 | lowercase 64-hex SHA256 of complete ZIP                                                    |
| `book_archive_bytes`                                                  | integer, 22 through 314572800 inclusive                                                    |
| `book_transport`                                                      | literal `repository-secret`                                                                |

Unknown/missing fields, duplicate JSON keys, untracked/symlink/modified manifest
files and oversized manifest bytes are rejected before Book downloads. The fixed
`LINKA_BOOK_CANDIDATE_READ_CAPABILITY` secret contains the existing task-owned,
one-asset read-only data capability, not a repository token. Masking, private temp
file, pinned private asset URL, expiry, full ZIP hash/size and cleanup remain.

The source checkout is `.local/model-source` at `model_source_sha`, independent of
the import workflow SHA. Existing tracked source-review/exclusion/preparation-code
checks, separate human flags (`raw_blob_artifact_approved=false` included), Book
execution receipt and global completion proof run against that selected source.
Canonical U/T bundle files and self-hash exclusion of provenance are unchanged.
Only a successful importer uploads `model-candidates-RUN-ATTEMPT`; consumers must
also require the complete producer workflow to finish successfully.

The original Book `source_revision`, `execution.source_sha`, receipts and model
bytes remain unchanged. Only import provenance gains:

```json
{
  "ci_import": {
    "schema": "linka-model-ci-import-v1",
    "run_id": "123",
    "run_attempt": 1,
    "workflow_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "manifest_path": "provenance/synthetic-import.json",
    "manifest_sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
    "model_source_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }
}
```

This is illustrative metadata, not an executable import or approval. The app
helper `scripts/ci/model-import-contract.cjs` owns pure manifest/origin validation;
`import-book-candidate.cjs` is the I/O adapter. It obtains model identity only from
the validated tracked manifest, not arbitrary `BOOK_SOURCE_SHA`/input overrides.
CLI: `IMPORT_MANIFEST=provenance/reviewed-import.json node
.local/app-tooling/scripts/ci/import-book-candidate.cjs` in the trusted workflow
workspace with its normal Actions context and private capability file.

## Consumer origin validation

The pure origin validator must receive **actual GitHub API** run metadata plus
manifest bytes fetched at that producer's verified workflow SHA/path. JSON claims
alone are not evidence. Preserve own-repository, main/manual workflow identity,
artifact membership/digest/attempt, main-history and terminal global-success checks.
New `ci_import` markers never fall back to legacy; marker-free historical artifacts
retain the existing `source_revision == run.head_sha` requirement without mutation.

The metadata-only integration uses the same pure contract in each consumer:

- `model-final.yml` first checks actual producer/API artifact metadata, with artifact
  `head_sha` bound to the producer workflow SHA, not requested model source. After
  exact source/app checkouts and main-history checks, the read-only
  `model-import-origin.cjs META ZIP` CLI fetches the exact tracked manifest from the
  fixed predictor Contents API at `run.head_sha`. It verifies regular file, base64,
  64 KiB bound, Git blob SHA1 and manifest SHA256. Only this step receives GH_TOKEN.
- Metadata `model_import` contains raw `provenance_json` and `manifest_json` (null
  for legacy), never a self-asserted PASS. `validateMetadata` verifies actual archive
  digest/size/flat entries, bounded provenance and exact raw envelope bytes before
  targets are read. New origin requires actual ZIP bytes; two-argument legacy calls
  cannot accept an import envelope or independent SHA. Metric/seal behavior is unchanged.
- App `windows-model-benchmark.cjs` resolves the same origin after ZIP verification.
  Qualification `source.json` retains actual model `source_sha`, adds separate
  `workflow_sha` and, for v2, `import_manifest_sha256`. Runtime identity is unchanged.
- App `promote-base-model.cjs` resolves origin before synchronous qualification.
  Approval run `head_sha` binds actual CI; model provenance, FINAL source and model
  Git source archive still bind the training source. Windows source proof binds
  both identities and manifest digest. Existing licenses, capsules, source guards,
  approved version records and human flags are unchanged.

`model-final.yml` now offers only `import-dev` and `import-final`, retaining the
existing strict aggregate-only schema, hashes, freeze/seal and Book report checks.
There are no legacy build/inference branches. No existing DEV/FINAL run is repeated.

Changing workflow bytes changes `preparation_code_hash` for **future** Book source
reviews/training. New training therefore needs a fresh matching source review; this
does not require retraining/resealing historical models. Historical 451/456 artifacts,
published base, manifests, approvals, model binaries and public releases stay frozen.

Focused synthetic tests live in the app `scripts/ci/*model*test*` and importer test
files. Importer tests no longer assume a sibling worktree: supply `BOOK_SOURCE_ROOT`
for the explicit predictor checkout (canonical Python validator integration and
workflow checks), or `BOOK_CANDIDATES_WORKFLOW` for the standalone candidate YAML
gate test. These checkout-dependent checks explicitly skip if not configured; pure
origin, ZIP/API binding and consumer identity fixtures are hermetic and mocked.
Future protocols must bind the new pure metadata validator as appropriate; this
change never rewrites an existing frozen seal or executes its targets.

## Evidence retention

Future model candidate, UI aggregate, Windows model qualification and Windows
runtime artifacts request `retention-days: 90`, the public Actions maximum. Upload
file sets, matrix/modes and success/status guards are unchanged: no raw corpus or
new payload channel is added. This does not extend already uploaded GitHub
artifacts retroactively. Expired artifacts remain invalid evidence, not a reason
to silently rebuild models or rerun frozen evaluation. Published historical
receipts remain immutable; final public release provenance and its reviewed build
capsule are the durable evidence, independent of temporary Actions retention.
