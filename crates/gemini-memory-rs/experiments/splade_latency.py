"""SPLADE latency tax, measured on this machine.

Two placements, which cost very differently:
  - doc-side  : expand facts at index time (off the voice path)
  - query-side: expand the user's utterance every turn (ON the voice path)
"""
import time, json, statistics as st
import numpy as np, onnxruntime as ort
from tokenizers import Tokenizer

TOK = Tokenizer.from_file("/tmp/splade/tokenizer.json")

# Real traffic from the memory engine's own tests.
QUERIES = [
    "what do you remember about my dietary preferences",
    "where should we eat dinner tonight",
    "Mujhe yaad dilao, mera khaana ka preference kya hai?",
    "what does my wife like about restaurants",
    "Enakku enna coffee pidikkum theriyuma?",
]
FACTS = [
    "The user is vegetarian and does not eat meat.",
    "The user's wife Rhea dislikes loud restaurants.",
    "The user has filter coffee every morning.",
    "The user goes to the gym before work every day.",
]


def session(threads):
    o = ort.SessionOptions()
    o.intra_op_num_threads = threads
    o.inter_op_num_threads = 1
    o.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    return ort.InferenceSession("/tmp/splade/model.onnx", o, providers=["CPUExecutionProvider"])


def encode(sess, text, k=None):
    """One SPLADE expansion: tokenize -> forward -> log-saturate -> max-pool -> top-k."""
    enc = TOK.encode(text)
    ids = np.array([enc.ids], dtype=np.int64)
    mask = np.array([enc.attention_mask], dtype=np.int64)
    feeds = {i.name: v for i, v in zip(sess.get_inputs(), [ids, mask, np.zeros_like(ids)])}
    feeds = {k2: v for k2, v in feeds.items() if k2 in {i.name for i in sess.get_inputs()}}
    logits = sess.run(None, feeds)[0]                       # (1, seq, vocab)
    w = np.log1p(np.maximum(logits, 0)) * mask[..., None]
    vec = w.max(axis=1)[0]                                  # (vocab,)
    nz = np.nonzero(vec)[0]
    if k:
        nz = nz[np.argsort(-vec[nz])[:k]]
    return len(enc.ids), nz, vec[nz]


def bench(sess, texts, label, k, reps=12):
    for t in texts:  # warm
        encode(sess, t, k)
    lat, toks, terms = [], [], []
    for _ in range(reps):
        for t in texts:
            s = time.perf_counter()
            n, nz, _ = encode(sess, t, k)
            lat.append((time.perf_counter() - s) * 1000)
            toks.append(n)
            terms.append(len(nz))
    lat.sort()
    return dict(label=label, n=len(lat), mean=st.mean(lat), p50=lat[len(lat)//2],
                p95=lat[int(len(lat)*.95)], max=lat[-1],
                in_tokens=st.mean(toks), out_terms=st.mean(terms))


rows = []
for threads in (1, 2, 4):
    s = session(threads)
    rows.append({**bench(s, QUERIES, f"query-side (top-64), {threads} thread(s)", 64), "threads": threads})
    rows.append({**bench(s, FACTS, f"doc-side (top-256), {threads} thread(s)", 256), "threads": threads})

print(f"{'placement':<34}{'p50':>9}{'p95':>9}{'max':>9}{'in tok':>9}{'out terms':>11}")
for r in rows:
    print(f"{r['label']:<34}{r['p50']:>8.1f}m{r['p95']:>8.1f}m{r['max']:>8.1f}m{r['in_tokens']:>9.0f}{r['out_terms']:>11.0f}")

# What the expansion actually contains, for the cross-lingual question.
s = session(4)
print("\nexpansion samples (top-12 terms):")
vocab = {v: k for k, v in json.load(open("/tmp/splade/tokenizer.json"))["model"]["vocab"].items()}
for t in ["The user is vegetarian and does not eat meat.",
          "Mujhe yaad dilao, mera khaana ka preference kya hai?"]:
    _, nz, w = encode(s, t, 12)
    print(f"  {t[:52]:<54} -> {[vocab[int(i)] for i in nz]}")
json.dump(rows, open("/tmp/splade/results.json", "w"), indent=1)
