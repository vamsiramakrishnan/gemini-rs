# Experiments

Things measured before deciding, kept so the decision can be re-checked when
the hardware or the model changes.

## `splade_latency.py` — should SPLADE go in the retrieval path?

SPLADE is learned sparse retrieval: a BERT forward pass expands a piece of text
into a weighted bag of vocabulary terms, so `vegetarian` also matches documents
saying `meat`, `eat`, `food`. That is precisely the vocabulary-mismatch gap
listed under Known Limits, so it is worth pricing.

```bash
pip install onnxruntime tokenizers numpy
mkdir -p /tmp/splade && cd /tmp/splade
curl -sSLO https://huggingface.co/prithivida/Splade_PP_en_v1/resolve/main/onnx/model.onnx
curl -sSLo tokenizer.json https://huggingface.co/prithivida/Splade_PP_en_v1/resolve/main/tokenizer.json
python3 <path-to-this-dir>/splade_latency.py
```

Measured on a 4-core container, `Splade_PP_en_v1` (BERT-base, fp32, ONNX
Runtime CPU), batch size 1:

| placement | threads | p50 | p95 |
|---|---|---|---|
| query-side, top-64 | 4 | 21.1 ms | 29.0 ms |
| query-side, top-64 | 2 | 35.6 ms | 47.3 ms |
| query-side, top-64 | 1 | 64.2 ms | 84.6 ms |
| doc-side, top-256 | 4 | 21.2 ms | 25.6 ms |
| doc-side, top-256 | 1 | 65.3 ms | 70.6 ms |

Compare `cargo run --release --example latency_budget`: the entire existing
synchronous path is **83 µs p50** at 1 000 records. Query-side SPLADE is
250–750× that, and it wants the same cores the audio pipeline is using.

The second result decided it. Expansions of the same claim in two languages:

```
"The user is vegetarian and does not eat meat."
  -> user, users, vegetarian, meat, vega, not, eat, eating, no, cannot, animal, food

"Mujhe yaad dilao, mera khaana ka preference kya hai?"
  -> mer, dil, ky, preference, mu, hai, ya, ##ao, india, ##j, ##ad, ##ha
```

The English expansion is genuinely good and would fix `diet`/`dietary`. The
Hinglish one is WordPiece debris — the model has no Hindi, so `khaana` never
becomes `food`. SPLADE cannot serve the code-switched case at all; that stays
the extraction model's job via multilingual `search_terms`.

**Conclusion: doc-side only, cached at write time.** Expanding a record costs
~21 ms once, when the record is created or refined during post-session
consolidation, and the terms are stored in the OKF front matter alongside the
existing tags. Query time stays pure lexical and unchanged. Nothing on the
voice path moves.

Not adopted yet: it means an ONNX Runtime dependency and a 532 MB model for a
recall win on English only, when the corpus is small enough that BM25 over
model-written aliases already finds what is there.
