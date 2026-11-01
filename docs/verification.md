# Public snapshot verification

Local clean publication checkout, macOS, Rust 1.98.1. Exit codes below are actual
results, not the historical M2 Pro benchmark or production acceptance.

| Check | Exit |
|---|---:|
| `cargo fmt --all --check` | 0 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 0 |
| `cargo test --locked --workspace --all-targets` | 0 |
| `cargo test --locked -p predictor-core --test runtime -- --ignored` | 0 |
| `cargo build --locked --release --workspace --examples` | 0 |
| `cargo build --locked --release -p predictor-cli` | 0 |
| README tiny ARPA compile, complete, score, stored-fixture complete | 0 each |
| README optional differential setup, parity and negative controls | 0 each |

The all-targets output reports 78 passing executions including a nested privacy
helper, 7 ignored entries and 1 filtered entry in that child run. Both privacy
helpers execute through parent tests. The standalone ignored tiny parity test
and all three ignored differential tests also passed separately using only
public synthetic inputs. The private-host trainer/orphan test is not run: the
publication host guard deliberately refuses training/qualification operations.
No full-corpus training, full-model benchmark or external-host operation was run.

The new host-guard regression was first observed failing (exit 101), then passing
(exit 0). Clippy, all-targets tests and release CLI were rechecked after changing
the package license to GPL-3.0-or-later; all returned 0.

Recompiled and stored tiny fixtures return the same five completions for
`я хочу `: `чай`, `пить`, `есть`, `воды`, `спать`, `pos=0`, `endOfWord=true`.
The sentence score for `я хочу чай` is -2.1052134037017822 log10 including EOS.

All copied core files are byte-identical to the supplied freeze. The only Rust
behavior change outside the core is the fail-closed private-host guard. No state
format, scoring, weights or query behavior was changed for publication.
The exact GNU GPL v3 license bytes match upstream git blob
`f288702d2fa16d3cdf0035b15a9fcbc552cd88e7`; the project grant is **or later**.
The dependency manifest retains 66 locked third-party package declarations.

Publication scans cover all staged bytes, including the synthetic binary, and
the initial clean commit diff: credential patterns, private home paths,
session/bridge/hardware identifiers, email-like PII and forbidden file classes.
No matched secret values are emitted. Standard GPL text and preserved mandatory
third-party notices are not private personal data. These bounded scans and scope
review are not a guarantee that a general-purpose secret detector finds everything.
