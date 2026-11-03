# @linkasu/predictor-electron (draft, not published)

Typed ESM API for Electron **main**, Node >=20. GPL-3.0-or-later.
No Electron dependency, renderer access, model download or automatic learning.

```ts
import { Predictor, applySuggestion } from '@linkasu/predictor-electron';

const predictor = new Predictor({
  executablePath: '/absolute/resources/lp-electron',
  modelPath: '/absolute/resources/model.lpk',
});
await predictor.start();
const input = { text: 'я хочу  завтра', caret: 7, language: 'ru' as const };
const prediction = await predictor.predict(input); // optional {signal}
if (prediction.text.length) {
  const edit = applySuggestion(input, prediction, 0);
  // Apply only if this is still the latest request and editor snapshot.
}
await predictor.dispose();
```

Full contract, error codes, cancellation, UTF-16 rules and security boundary:
[docs/electron-api.md](https://github.com/linkasu/linka.predictor-rust/blob/feat/electron-main-585e7e05/docs/electron-api.md).
One instance per editor; a new request supersedes the previous one. RU predicts
the prefix before the caret and preserves the suffix. EN offers manual input
without prediction. Every RU call loads the model in a fresh bounded sidecar;
there is **no warm-latency claim**.

From repository root:

```sh
cargo build --locked -p predictor-cli --no-default-features --bin lp-electron
cd packages/electron
npm ci
npm test
npm pack
```

`npm test` requires the real debug binary (or absolute `LP_ELECTRON_BIN`). It also
checks real child cancellation/crash/protocol scenarios and the packed artifact.
No release command is provided; `private: true` blocks accidental npm publish.
Tarball intentionally excludes executable and model: the host app must package
approved resources outside ASAR, preserve license/model notices, and qualify its
own platform/signing/IPC. Local macOS tests do not qualify packaged Electron,
Windows, Linux or another architecture.

Windows x64 runtime qualification runs on PR candidates and by manual dispatch,
as documented in `docs/electron-api.md` in the source repository. It pins Rust
1.98.1, matching the repository, uses static CRT and records native smoke results
and DLL imports. Candidate artifacts are not production approval; approve only
a reviewed commit hash after successful native CI. The packaged EXE requires no user
Rust or Node installation; the API runs inside the host Electron main process.
