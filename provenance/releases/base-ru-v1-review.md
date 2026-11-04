# Release operator review: base-ru-v1 resources only

This hash-bound approval executes the owner's instruction to finish the release,
subject to the agreed gates. It is not an app-installer approval or an independent
legal/clinical certification.

- Required U and selected T were built on Book, with exact source/input/receipt
  and reconciled whole-job/child evidence. Original source records remain local.
- T won the preregistered private DEV comparison; approved selection/config was
  committed before opening FINAL. No tuning on FINAL occurred.
- FINAL aggregate import [41758860254](https://github.com/linkasu/linka.predictor-rust/actions/runs/41758860254)
  succeeded: 530 rows / 525 groups, UI_KSR_V1 24.90%, 95% CI [23.80%,26.00%],
  paired T-U gain 1.93 percentage points, 95% CI [1.41,2.46]. This is synthetic
  simulation evidence only; the original Book report and import semantics agree.
- Native Windows qualification [41756202814](https://github.com/linkasu/linka.predictor-rust/actions/runs/41756202814)
  succeeded for the same T hash: p95 266 ms, p99 268.4 ms, initial 177.9 ms,
  sidecar private commit <=7.7 MB under enforced 512 MiB, zero remaining children.
  Memory-limit positive/negative controls passed. This is recorded CI, not physical
  Windows 10/11 or installed-app qualification.
- Data permission comes from the verified official CC0 declaration and separate
  owner content decision; code is GPL-3.0-or-later. Source offers are pinned to
  actual model/runtime commits and retain notices/build recipes.
- The selected model was rebuilt on Book from its ARPA/frequency inputs with the
  identical LPK hash. Only these derived model inputs are approved for durable
  publication; original corpus records, full sentence attribution and private
  DEV/FINAL targets are excluded.
- Canonical published code LICENSE is exact corresponding Git-source text; the
  Windows artifact's CRLF-only difference is recorded separately, not hidden.

`base-ru-v1-approval.json` pins all bytes and successful run/artifact identifiers.
No production model/runtime binary is rebuilt or changed during publication.
App 4.0.0 still requires real installer inventory/security signoff and genuine
upgrade from the published 3.3.1 before its separate publication.
