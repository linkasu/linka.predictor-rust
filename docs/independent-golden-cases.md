# Hand-counted independent checks

These are projection/action arithmetic cases, not ranked language-model expectations. Unicode escapes below describe literal scalar sequences, not six ASCII characters.

| Original q | Internal alternatives | Public expected | Applied results |
|---|---|---|---|
| `при вет` | replace all → `привет`; replace `вет` → `ветер` | pos=-7; text=[`привет`,`при ветер`] | `привет`; `при ветер` |
| `🙂 при вет` | replace `при вет` → `привет`; replace `вет` → `ветер` | pos=-7; text=[`привет`,`при ветер`] | `🙂 привет`; `🙂 при ветер` |
| `е\u0308` | replace all → `ёлка` | pos=-2; text=[`ёлка`] | `ёлка` |
| `е\u0308ж при` | replace `при` → `привет` | pos=-3; text=[`привет`] | original decomposed `е\u0308ж привет` |
| `я хочу` | append ` пить` | pos=0; text=[` пить`] | `я хочу пить` |
| `я хочу ` | append `пить` | pos=0; text=[`пить`] | `я хочу пить` |
| `я  при` | replace `при` → `привет`; append `мер` | pos=-3; text=[`привет`,`пример`] | `я  привет`; `я  пример` |

The emoji occupies two UTF-16 code units but is outside the replaced suffix in the emoji case. An implementation deriving pos from the entire q or normalized q is incorrect. Original byte offsets differ from UTF-16 positions. Test reference application should construct a UTF-16 vector and remove suffix units independently, without calling the production projector.

Action arithmetic with a deliberately fixed response oracle: target `я хочу`, initial empty response, type `я` (1 action), accept append ` хочу` (1/2/3 selection actions). Baseline has 6 graphemes, including the space. Total actions are 2/3/4, KSR is 2/3, 1/2, 1/3. If the oracle instead offers only `хочу` after typing `я `, the space costs one typed action: totals 3/4/5, KSR 1/2, 1/3, 1/6. These two cases must not yield the same arithmetic. Suggestions saving no more than selection cost are not selected.
