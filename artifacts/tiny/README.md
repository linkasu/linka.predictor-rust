# Synthetic fixture (CC0-1.0)

Original synthetic household sentences authored for this project and dedicated
to CC0-1.0: https://creativecommons.org/publicdomain/zero/1.0/ . This covers
`train.txt`, `model.arpa`, `query.txt`, `query.stdout`, `reference.json` and
`runtime-v1.lpk`; no external corpus or personal profile is included.

The ARPA is an actual modified Kneser–Ney trigram estimated by KenLM using
`-o 3 -S 6G --discount_fallback`, without pruning or vocabulary cutoff. Saved
reference output covers backoff, OOV and BOS/EOS. The `host` label in the
reference is a historical test contract, not a network dependency. The preserved
KenLM build-tool license is in `provenance/licenses/KenLM-LICENSE`.

The 14,450-byte runtime is a functional fixture, not a general Russian model or
representative quality benchmark. Recompile it locally with the README commands;
no training, external data, private host or download is needed for tiny E2E.
