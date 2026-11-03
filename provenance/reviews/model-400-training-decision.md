# Owner decision: filtered CC0 training only

On 2026-11-04 the release owner explicitly selected **"Обучать без raw-upload"**
after seeing review `41722106455:1`, its exact report/source hashes and these limits:

- 23,094 public Tatoeba Russian CC0 examples; no malformed/non-Russian rows.
- 24 direct-identifier originals; 63 connected IDs excluded as complete components.
- Zero exact/near evaluation-index matches and zero split/component overlaps.
- This is acceptance of the declared source, sanitized review evidence and residual
  heuristic risk, **not manual inspection of every sentence or a privacy guarantee**.

Permission covers required U and optional T training on the filtered portion in
bounded Linux CI. `content_reviewed=true` records this human scope decision, not
an independent legal/privacy certification. Source, evidence, report, code,
exclusions and excluded-ID hashes are fixed in `model-400-training-approval.json`.

The original raw corpus must remain ephemeral: **no raw artifact upload or public
snapshot is authorized**. Durable-source reproduction remains unresolved. This
does not approve model publication, production resources, FINAL quality or Windows
performance. FINAL/DEV content is not included here or exposed to training.
