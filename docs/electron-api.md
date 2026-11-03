# Electron main API — keyboard issue145 contract

Draft contract, GPL-3.0-or-later. Import `Predictor`, `PredictorError` and
`applySuggestion` from `@linkasu/predictor-electron` in **main only**.

```ts
const predictor = new Predictor({ executablePath, modelPath }); // absolute trusted paths
await predictor.start(); // validates model; optional, predict also initializes
const input = { text: 'я хочу  завтра', caret: 7, language: 'ru' as const };
const result = await predictor.predict(input, { signal });
const edit = applySuggestion(input, result, 0); // { text, caret, language }
await predictor.dispose(); // idempotent; await before main exits
```

`predict(input: {text: string; caret: number; language: 'ru' | 'en'},
options?: {signal?: AbortSignal}): Promise<Prediction>`.
`Prediction` has `{text: string[], pos: number, endOfWord: boolean, input: Input}`.
`pos <= 0` is UTF-16 tail deletion relative to the caret, **not** full text end.
Zero appends. `applySuggestion(current, result, index)` checks exact text, caret,
language and UTF-16 boundaries; replaces `[caret + pos, caret)` and preserves
`text.slice(caret)` verbatim. It never inserts an implicit space. The caller must
also invalidate displayed suggestions after editing/navigation (including an ABA
edit back to identical text). Selections must be collapsed by the host first.

RU sends only `text.slice(0, caret)` to the end-only core. Mid-word behavior is
prefix completion, not replacement of the suffix. EN returns an empty result
without starting a process or requiring a model; manual typing is unchanged.
Only RU and EN are accepted. No learning, profile writes or telemetry.

One instance serves one input stream. A newer predict/start invalidates older
pending work (`STALE`); AbortSignal rejects with `CANCELLED`; dispose rejects
pending work with `DISPOSED`. No prediction result is applied automatically.
State: `idle -> running -> ready | failed`; another request may retry a failed
instance; `disposed` is terminal. EN can be ready without a loaded model.

Errors are `PredictorError` with stable `code`: `INVALID_INPUT`, `MODEL_MISSING`,
`MODEL_CORRUPT`, `SPAWN`, `PROCESS`, `PROTOCOL`, `TIMEOUT`, `CANCELLED`, `STALE`,
`DISPOSED`. Do not forward raw Error objects across Electron IPC; map to a
serializable `{code,message}`. Error messages never include query or model data.

## Transport / bounds

The dedicated `lp-electron` binary receives `{q, lang:"ru", limit:5}` via stdin
and writes exactly one JSON `{text:string[],pos:int,endOfWord:bool}` to stdout.
Only the model path is in argv; no shell. Each operation uses a fresh process
and loads the immutable model, including `start()` (empty-query validation).
This deliberately trades warm-model latency for simple process cancellation;
no warm-cache or <100ms claim. At most one child lives per instance: replacement
waits for the previous child's close. Only owned children are terminated.
The full editor input is limited to 16,384 UTF-16 units; the RU prefix sent to
the core is additionally limited to 16,384 UTF-8 bytes (checked before spawning).
The sidecar applies the same UTF-8 byte limit to `q`. EN remains manual and has
no RU byte limit. Transport is limited to 128 KiB JSON, output to 256 KiB,
deadline 5s by default (configurable 1–60s). Overdue/cancelled children receive
SIGTERM, then SIGKILL after 250ms if still alive. No unbounded request queue:
superseded callers reject before spawning.

## Electron boundary / packaging

Keep `contextIsolation: true`, `sandbox: true`, `nodeIntegration: false`.
Do not expose this class, executable/model paths, arbitrary channels, or Node
APIs to renderer. A host-owned narrow preload bridge validates sender/frame and
payload, owns one predictor per editor, and returns only serializable results.
AbortSignal stays in main; renderer cancellation uses a validated request ID.
No IPC handler is installed by this library; host authorization remains required.

Build `cargo build --locked -p predictor-cli --no-default-features --bin lp-electron`; copy binary and
approved immutable model through the host packager's `extraResources`, outside
ASAR. Pass absolute paths under `process.resourcesPath`, not renderer input.
Ship GPL license/source obligations and separate model notices. Tiny test model
is synthetic CC0, not a production Russian model. No npm publication or release.

Tests exercise real child processes and the real Rust core with tiny model.
Package checks cover npm tarball contents and installed imports. macOS local
execution is not signed/notarized Electron application qualification. Linux,
Windows, architecture matrices, ASAR extraction, code signing, hardened runtime
and packaged Electron IPC are unqualified until run on those targets.

## Windows runtime qualification (PR/manual, not a release)

The default `tooling` feature preserves the existing Unix `lp` CLI and all six
tooling library modules. `lp` requires `tooling`; `lp-electron` and predictor-core
can build without it. Runtime-only builds exclude CLI SQLite/bzip2/XML/libc
dependencies; predictor-core dependencies are unchanged. This does not port
`host_guard`, preparation or training to Windows. Runtime-only tests must select
`--bin lp-electron`, not the tooling integration-test suite.

`.github/workflows/windows-runtime.yml` runs automatically on `pull_request`
against `github.event.pull_request.head.sha`, including before the workflow is
merged to the default branch. This produces a `pr-unreviewed` candidate artifact.
It also supports `workflow_dispatch`: select the reviewed workflow ref and supply
`source_sha` as the full 40-character reviewed source commit (including the
workflow), producing a `manual-reviewed` artifact. Manual dispatch requires the
workflow to be available on the default branch. Both modes validate the SHA
before checkout and verify exact HEAD afterward. No secrets, publishing, push
trigger, model training, `pull_request_target` or approval bypass is used.
Neither mode grants production approval: approve only a specific reviewed
commit hash after successful native CI for that exact hash. Artifact names and
source evidence distinguish the modes and record `production-approved=false`.

Qualification explicitly pins **Rust 1.98.1**, matching the source's
`rust-toolchain.toml`. Every workflow Cargo invocation uses `+1.98.1`.
Historical local verification used the isolated Rust **1.98.0** toolchain:
Windows-target check/clippy, parser tests, scoped CLI/core tests and Electron
tests passed; cross-build reached linking but lacked macOS `link.exe`.
Those results do not verify 1.98.1; native CI must run it. The legacy app CI
1.98.0 pin is to be updated separately during integration. The Windows-2022 runner
builds `x86_64-pc-windows-msvc` with `-C target-feature=+crt-static`, records
`dumpbin /dependents`, and rejects imported dynamic CRT DLLs. Windows system
DLLs are still expected. It runs the real release EXE on the synthetic tiny
fixture with Rust/Node removed from PATH; end users need neither installed.
Smoke drains stdout and stderr asynchronously before waiting for child exit,
and checks an empty query plus an eligible query at the 16,384-byte boundary.
Timeout cleanup terminates only the process started by that smoke invocation.

The artifact contains the EXE, GPL license, tiny fixture (not a production
model), source/workflow revision, compiler version, dependency tree, SHA-256
hashes, DLL imports and smoke result. An uploaded artifact is not proof of
success: require a successful job and `smoke.txt`. Native candidate CI runs
before merge; production approval still requires review of the successfully
tested exact SHA. Local macOS Windows-target `cargo check` does not qualify an
EXE. App resource packaging, signing and real-model Windows acceptance remain
separate gates. No Windows Electron/Node API test coverage is claimed by this
Rust-only workflow; the existing fake-sidecar tests use a Unix executable script.
