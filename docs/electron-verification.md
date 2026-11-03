# Electron adapter verification

Local verification: 2026-11-03, macOS 27.0.1 arm64, Node 20.20.2,
Cargo 1.98.1. Base: `1e1c681221d83af176751648d74d02ec2b350aee`.

## Passed

```sh
cargo build --locked -j 2 -p predictor-cli --bin lp-electron
cargo fmt --all --check
cargo clippy --locked -j 2 --workspace --all-targets -- -D warnings
cargo test --locked -j 2 --workspace --all-targets
cargo test --locked -j 2 -p predictor-core --test runtime -- --ignored
npm test --prefix packages/electron
git diff --check
```

Node suite: **9 passed, 0 failed/skipped**, including:

- Real Rust process/model: startup, prediction, repeat calls, disposal and
  rejection after disposal; input mutation after submit cannot alter the snapshot.
- Real core correction/completion at end and in the middle, including emoji
  before the replaced word; exact suffix and resulting caret preservation.
- Missing/corrupt model, recovery after replacing a test model, EN manual mode.
- Real child processes for spawn/crash, malformed JSON/UTF-8, invalid replacement
  offsets, output overflow, cancellation, stale pending work and deadlines.
- Ignored-SIGTERM child terminated after timeout; cancelled/disposed child PIDs
  are absent when the promises finish. No fixed sleep is used for observation.
- Burst of 52 requests: only first/latest spawn, peak live children **1**.
- `npm pack`, exact five-file allowlist, GPL text, offline install, public ESM
  export import, and TypeScript consumer compilation (including expected errors
  for unsupported language and synchronous use). No runtime npm dependencies.

The Rust request-schema test was observed red (missing implementation), then
green. Initial TypeScript lifecycle narrowing errors were fixed before passing
the build. An added caret fixture initially assumed a word absent from the
synthetic vocabulary; it was corrected to actual fixture words (`хо`, `хчоу`,
`😀 хо`), retaining the nonempty/replacement/suffix assertions. No core search
behavior was changed to satisfy adapter tests.

## Not qualified

- Local tests are not a packaged Electron app, preload authorization test or
  real keyboard issue145 integration. Host app must implement its narrow IPC
  bridge and invalidate suggestions on all editor changes/navigation.
- Linux CI is configured but not claimed as passed by this local report.
- Windows, x64/other architectures, ASAR layout, executable permissions in the
  final app, code signing, notarization and hardened runtime are not qualified.
- No production Russian model, redistribution approval, AAC user acceptance,
  full-model latency/memory benchmark or warm-model SLA. The model reloads for
  each RU request; validate on target hardware before shipping.
- Private-host training/orphan test and optional differential harness remain
  skipped. Stored tiny runtime parity passed; no training was run.
- No npm release, credentials/access changes or Electron security setting
  changes are part of this work.
