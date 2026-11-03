# LINKa Predictor — Rust core and CLI

Offline Russian end-of-text completion and correction, implemented in native Rust.
`predictor-core` loads an immutable mmap-backed `.lpk` model; `predictor-cli`
provides the `lp` command. A draft [Electron main API](docs/electron-api.md) wraps
the dedicated stdin-only `lp-electron` sidecar. No HTTP service, GUI or English
predictor is delivered here. The CLI currently targets Unix; Windows and Android
are not qualified. Rust 1.98.1 is pinned in `rust-toolchain.toml`.

## Build and reproduce (synthetic data only)

From a fresh clone, with Rust/rustup installed:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
cargo test --locked -p predictor-core --test runtime -- --ignored
cargo build --locked --release --workspace --examples
cargo build --locked --release -p predictor-cli
mkdir -p .local
target/release/lp compile artifacts/tiny/model.arpa .local/tiny.lpk
target/release/lp complete .local/tiny.lpk "я хочу " 5
target/release/lp score .local/tiny.lpk "я хочу чай"
target/release/lp complete artifacts/tiny/runtime-v1.lpk "я хочу " 5
```

The compile/complete/score sequence is a tiny E2E, not training or a full-model
benchmark. Cargo downloads pinned dependencies on first build; inference itself
is offline. The stored KenLM parity test consumes only the included synthetic
reference, not a private machine. Privacy helpers marked ignored run through
their process-isolated parent tests. The private-host trainer/orphan test is
**not** run or qualified. No full Russian model,
external corpus, profile or binary release is distributed.

Optional stored-reference differential tests (no trainer; the setup deliberately
refuses an existing `.local/differential.lpk`):

```sh
export LP_DIFFERENTIAL_MODEL="$PWD/.local/differential.lpk"
export LP_DIFFERENTIAL_INPUT="$PWD/artifacts/tiny/query.txt"
export LP_DIFFERENTIAL_REFERENCE="$PWD/artifacts/tiny/query.stdout"
cargo test --locked -p predictor-core --test kenlm_differential -- --ignored --exact compile_stored_tiny_package_for_harness
cargo test --locked -p predictor-core --test kenlm_differential -- --ignored --exact stored_kenlm_differential
cargo test --locked -p predictor-core --test kenlm_differential -- --ignored --exact stored_harness_negative_controls
```

## Rust API

```rust,no_run
use predictor_core::{Predictor, Request, Result};

fn main() -> Result<()> {
    let predictor = Predictor::load("artifacts/tiny/runtime-v1.lpk")?;
    let response = predictor.complete(
        Request { q: "я хочу ", lang: "ru", limit: 5 },
        None,
    )?;
    println!("{:?}", response);
    Ok(())
}
```

`Request::new(q)` defaults to `lang="ru"`, `limit=5`. The response serializes as
`{text, pos, endOfWord}`. Each suggestion replaces the original **UTF-16 tail**:
negative `pos` removes that many UTF-16 code units from the end, zero appends.
Spaces are explicit; unaffected input is preserved. There is no mid-text cursor
API or automatic edit application in the core; the Electron adapter accepts a
UTF-16 caret and preserves its suffix. See [runtime API](spec/runtime-api.md),
[text contract](spec/text-contract.md) and [package format](spec/package-v1.md).
Querying never learns; personal learning requires explicit confirmed events and
an explicit separately stored profile. Applications must serialize profile writers.

## Checkpoint and measured performance

The core is copied byte-for-byte from the verified performance checkpoint.
The public CLI differs only by a fail-closed private-host guard and its regression
test: training and qualification commands cannot run here. No private hardware
identity, baseline configuration or remote operations scripts are distributed.
Preparation, ARPA compilation, inference and local evaluation remain available.
The [CLI spec](spec/cli.md) also describes historical pipeline functionality; it
is not a ready-to-run public full-training recipe.

Historical measurement on Apple M2 Pro, 10 CPU cores, 16 GiB, macOS, AC power:
24,000 original + 4,000 retained warm calls, Russian/limit 5. Original-suite p95:
**70.509 ms** without profile, **80.054 ms** with its final scripted profile;
worst per-scenario p95 **80.606 ms** (not the maximum single call). Request-local
LM-ID and continuation caches preserve responses, scoring order and semantics;
they retain no cross-request query history. The checkpoint passed **77 tests**
(ignored tests excluded). The public checkout adds one host-guard regression.

These are repeated frozen synthetic inputs, not 28,000 independent utterances or
a guarantee of <100 ms on all hardware. Full-model loading/validation was about
one second and is outside the warm timing; disk-cold performance was not measured.
The private measurement model and CLI hashes are recorded only as identities in
[the reproduction manifest](provenance/reproduction.json), not as downloads or
claims that a newly compiled public binary has the same hash. The original
full-model measurement cannot be reproduced from the tiny fixture.

## Quality limitations — not production accepted

Latency success is not AAC quality acceptance. Synthetic AAC journeys still show
harmful suggestions and a confirmed name outside the top five. Wrong-choice,
undo and repair can cost more than typing. OpenCorpora was excluded after HTTP
403; near-duplicate/translation-family isolation is incomplete. No real-human
AAC acceptance, English, packaged GUI/Electron integration or cross-device latency claim
is made. The separate quality work is not included in this snapshot.

## Licenses and provenance

Project-owned code: **GPL-3.0-or-later**, under the owner's explicit publication
license choice, superseding the earlier unpublished MIT manifest declaration.
The root LICENSE is the unchanged GNU GPL v3 text; the **or later** grant is
declared here and in the workspace/crate metadata. Synthetic tiny files:
**CC0-1.0**, separate from code. Dependencies retain
their own licenses; see [dependency manifest](provenance/dependency-licenses.json)
and [data / third-party notices](provenance/README.md). KenLM is an external build
tool, not linked into the runtime. No third-party authorship is reassigned here.

Tatoeba-derived models and credits are **not GPL** merely because the code is.
No corpus/model redistribution approval is asserted. A pinned download and
prepare recipe is documented separately; it preserves sentence IDs/authors and
requires independent licensing review before distributing resulting data.

The [publication manifest](provenance/publication-files.json) records the original
public baseline files, hashes and snapshot differences (not later adapter additions). Private histories, ledgers,
process logs, owner requests, credentials, personal profiles, full bundles and
unrelated work-in-progress are deliberately excluded. CI runs fmt/clippy/tests
with read-only repository permission, no secrets and no deployment.
