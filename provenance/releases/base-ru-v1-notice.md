# LINKa base-ru-v1: Russian offline word prediction

Model: MKN trigram T, CPU-only, LPK v1, 13,330,710 bytes.
SHA-256: `b3e736c8b42f6b75f39c26076d13ac99a65e6d10ef43b92de13ac288953b430a`.

## Rights and source

- Model data: **CC0-1.0**, derived from the official public Tatoeba Russian CC0
  export observed 2026-10-31. Original compressed input SHA-256:
  `3b6645bbc18e4e3444ebc9195c4a6c41ff521a8bc4306fec8fcc217ae734db76`.
  Declaration: <https://tatoeba.org/en/downloads>;
  terms: <https://creativecommons.org/publicdomain/zero/1.0/>.
- Changes: NFC/lowercase normalization, segmentation, grouped deduplication,
  train-only counts, frozen evaluation exclusions and direct-identifier/component
  exclusions. The owner accepted the declared source, sanitized review and
  residual heuristic risk; no perfect-privacy certification is claimed.
- Rust runtime/build code: **GPL-3.0-or-later**. Model code source:
  `linkasu/linka.predictor-rust@4568465fcc436345eb56dc49b44555b251b9c733`.
  Windows runtime source: `ec07b2cd49c1c2c684e6643f0dab75cf5895ebca`.
- Estimation used KenLM `4cb443e60b7bf2c0ddf3c745378f76cb59e254e5`;
  its license is retained with the release. Existing source notices are preserved.

## Quality evidence and limitations

Canonical **UI_KSR_V1** measures simulated input-action savings through the real
application editing/filter classes, with three suggestion slots and selection
cost 1. It is not clinical AAC effectiveness or a user study.

- Independent project-authored AI-synthetic FINAL: 530 rows, 525 curator-assigned
  groups; no tuning on FINAL. T KSR **24.90%**, group-bootstrap 95% CI
  **[23.80%, 26.00%]**. U baseline **22.97%**; paired gain **1.93 percentage
  points**, 95% CI **[1.41, 2.46]**. Conditional on the frozen grouping/protocol;
  statistical independence beyond that annotation is not independently certified.
- Source/model/parameters were fixed by DEV before FINAL. Quality inference ran
  on `book.local`; GitHub CI imported immutable aggregates without inference.
- Recorded Windows CI: 1000 repeated completed UI requests, p95 **266 ms**, p99
  **268.4 ms**, initial query **177.9 ms**; sidecar peak private commit **7.7 MB**
  under an enforced 512 MiB Job Object cap. These are runner-specific measurements,
  not proof on every 8 GB PC, an installed package or physical consumer devices.
- Physical tracking/calibration, audible speech and actual Windows 10/11 consumer
  hardware are **NOT TESTED**. The owner accepted CI-only qualification and an
  unsigned Windows application release; those boundaries are not converted to PASS.

## Reproduction and privacy boundary

`MODEL_BUILD_INPUTS.zip` contains the exact selected ARPA model weights,
frequency statistics, build instructions and hash-bound reconstruction receipt.
The LPK was regenerated on Book with the identical SHA-256 from these inputs.
Corresponding model/runtime Rust sources and build recipes are supplied separately.

This is exact production-model generation, not an archive of original corpus
records. The original raw corpus, full sentence attribution, private profiles,
and DEV/FINAL target text are **not published**. Original-source preprocessing
availability is a separate boundary; the weekly export URL may change.
