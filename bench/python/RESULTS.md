# Python binding benchmarks

Produced by `python bench/python/bench.py` (release build of the extension,
`maturin develop --release`). In-process and model-free: no network, no
model. Request latency to Gemini is not measured; it is network-bound and
the binding does not change it. The README quotes only numbers from this
file.

Revision `b20be80`; Linux x86_64 (4 vCPU cloud container); Python 3.13.18;
gemini_adk 4.0.0; google-genai 2.34.0; google-adk 2.13.0.

| Workload | Implementation | p50 (ms) | p99 (ms) | n |
|---|---|---:|---:|---:|
| Live audio frame, wire JSON to Python bytes (per frame) | gemini_adk scripted session | 0.0276 | 0.0324 | 20 |
| Live audio frame, wire JSON to Python bytes (per frame) | google-genai `LiveServerMessage` | 0.0167 | 0.0176 | 20 |
| Live audio frame, wire JSON to Python bytes (per frame) | `json.loads` + `b64decode` (floor) | 0.0041 | 0.0042 | 20 |
| Tool round trip: model call, Python tool, model call | gemini_adk + `MockLlm` | 0.0649 | 0.0998 | 2000 |
| Tool round trip: model call, Python tool, model call | google-adk + scripted `BaseLlm` | 3.7509 | 4.3323 | 300 |
| `SessionSpec.validate()` | gemini_adk | 0.0016 | 0.0020 | 2000 |
| `Conversation` compile | gemini_adk | 0.0122 | 0.0333 | 2000 |
| Scenario, 9 steps, offline | gemini_adk | 0.0163 | 0.0388 | 2000 |
| `Sim.step` (one state write) | gemini_adk | 0.0084 | 0.0229 | 5000 |

What each row measures, so the comparison is read correctly:

- **Live audio frame.** For gemini_adk the sample is one scripted session of
  1 000 frames end to end: replay transport, frame decoder, processor fast
  lane, broadcast, bridge task, bounded channel, and the Python dict with a
  `bytes` payload the consumer receives; divided by 1 000. For google-genai
  it is `LiveServerMessage.model_validate(json.loads(text))` and reading the
  bytes out, which is the per-message work its Live client does minus the
  socket. The floor row is the parse alone. The binding's figure is a
  whole pipeline, the comparison's a parse; the point of the row is that
  the pipeline costs tens of microseconds per 40 ms frame, not that it
  beats a parse.
- **Tool round trip.** One turn in which the model asks for a tool, the
  Python tool runs, and the model answers; the model is scripted on both
  sides. For google-adk the sample includes creating the `LlmAgent`,
  `InMemoryRunner` and session for the turn, which is how a request is
  served in that framework; for gemini_adk it includes building the
  `Agent`. Both include the Python tool call itself.
- **Spec, compile, scenario, sim.** gemini_adk only; neither comparison
  package has an offline governed simulation to compare with.

Reproduce: `pip install google-genai google-adk`, build the extension in
release mode, run the script from the repository root.
