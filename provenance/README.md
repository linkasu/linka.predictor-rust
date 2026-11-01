# Code, dependencies and data notices

## Code and build dependencies

Project-owned Rust code is licensed **GPL-3.0-or-later**, explicitly selected by
the owner before initial publication, superseding the earlier unpublished MIT
workspace declaration. Both crates inherit this workspace license. The unchanged
GPL v3 LICENSE text comes from `linkasu/linka.looks-electron` commit
`1cd6fcbc0b208705f4ad45138b0b7f8e8a2b8bd0`, blob
`f288702d2fa16d3cdf0035b15a9fcbc552cd88e7`; the **or later** grant is specified
in the metadata and README, not by modifying that standard license text.
There was no conflicting source copyright header in the copied Rust files.
This does not reassign third-party ownership or relicense data. Dependency
versions, registry checksums, repositories and declared SPDX expressions are in
`dependency-licenses.json`, generated from locked Cargo metadata and Cargo.lock.
This is a source distribution, not a vendored dependency/binary distribution.
When redistributing a compiled binary, collect the selected dependency license
texts and required notices from the exact resolved packages as well.

In particular: libbz2-rs-sys uses bzip2-1.0.6; foldhash uses Zlib;
unicode-ident additionally requires Unicode-3.0; ryu offers Apache-2.0 or BSL-1.0.
MIT/Apache dual-license choices do not erase Unicode notices. libsqlite3-sys is
MIT; its bundled SQLite C implementation is upstream public-domain software
(https://www.sqlite.org/copyright.html), not newly authored project code.
Target-specific dependencies listed by Cargo are not all linked on every host.
No dependency source or external corpus is vendored here.

KenLM is an external estimator/reference tool; its original license, including
its bundled-code notice, is preserved in `licenses/KenLM-LICENSE`. No KenLM code
is copied into the Rust runtime. Included ARPA/reference data are generated from
the project's original synthetic CC0 fixture, not from Tatoeba.

## Included data

`artifacts/tiny/` is an original synthetic CC0-1.0 fixture; see its README.
`configs/qualification-inputs.json` contains synthetic diagnostic queries, not
personal history. The reference's historical host label is not an address that
the tests contact. No actual personal overlay, real-user history, external
sentence/author records, Tatoeba weights or full attribution bundle is included.

## Optional pinned Tatoeba acquisition / preparation

The historical input identity is recorded in `reproduction.json`. The upstream
export URL is mutable: a different hash must **fail**, not silently become this
checkpoint. If that version is no longer available, obtain a lawfully retained
copy or declare a new input version. No cached/private path is required:

```sh
mkdir -p .local
curl --fail --location --output .local/tatoeba.tsv.bz2 \
  https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_detailed.tsv.bz2
printf '%s  %s\n' \
  16e6147510b4f09f97636940aaf674dee68a47910276c9a122e81453527eb329 \
  .local/tatoeba.tsv.bz2 | shasum -a 256 --check
# Run the following only after the checksum command succeeds.
target/release/lp acquire .local/tatoeba.tsv.bz2 .local/verified.tsv.bz2 \
  16e6147510b4f09f97636940aaf674dee68a47910276c9a122e81453527eb329
target/release/lp prepare .local/verified.tsv.bz2 .local/prepared artifacts/tiny/train.txt
```

The tiny training text is a public synthetic exclusion list for this example,
**not** the private historical holdout. This is not reproduction of the complete
historical training/split/model. `prepare` retains attribution and grouped split
metadata, excludes missing authors and uses train-only counts; it does not train.
No corpus download or training was performed during publication.

Historical source declaration: Tatoeba CC BY 2.0 France,
https://creativecommons.org/licenses/by/2.0/fr/ ; usage guidance:
https://en.wiki.tatoeba.org/articles/show/using-the-tatoeba-corpus . Before
redistribution, verify eligibility of the exact records and preserve sentence
IDs, authors, source links, license links and all required attribution/credits.
Preparation metadata alone is not proof of full legal eligibility. Code license
does not license data, models or credits. No blanket redistribution claim is made.

OpenCorpora was excluded (HTTP 403 in the final checkpoint); its earlier proposed
CC BY-SA license was not verified for acquired data. SOVA was not acquired or
reviewed. Neither source is distributed or silently replaced by other corpora.

## Publication boundary

Only the core/CLI, specs, tests, synthetic fixtures and sanitized reproduction,
performance and license documentation are published. The original private git
graph, worklogs, ledgers, owner requests, session/bridge identifiers, raw logs,
personal profiles, credentials, external corpora and model bundles are omitted.
The public host guard fails closed rather than publishing private paths or IDs.
