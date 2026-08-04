# @kirkforge/correction-core

Correction prompt generation from `ReducedStatePacket` results. Takes verification output and builds a structured prompt for the worker model.

## Key exports

- `buildCorrectionPrompt(packet, language?)` — generate a correction prompt
- `toolNames` — map of language → tool name arrays
- `Reducer` — event-to-packet reduction logic
