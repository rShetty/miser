#!/usr/bin/env python3
"""100-prompt head-to-head: miser vs typesafe/jev-router, judged on answer quality.

Three metrics, because any one alone is misleading:
  quality  -- an LLM judge scores each answer 1-5 against the prompt
  cost     -- what the answer actually cost
  routing  -- whether the model chosen matches the difficulty of the ask

Judge is openai/gpt-4.1 (full), which is a different model from anything either
router selected, so it is not judging its own output.
"""
import json, os, re, statistics, time, urllib.request, collections, sys

OR_KEY = os.environ["OPENROUTER_API_KEY"]
MISER_KEY = os.environ["MISER_API_KEY"]
OR = "https://openrouter.ai/api/v1/chat/completions"
MISER = "http://127.0.0.1:8787/v1/chat/completions"
JUDGE = "openai/gpt-4.1"
N = int(sys.argv[1]) if len(sys.argv) > 1 else 100

by_tier = collections.defaultdict(list)
with open("evals/classifier_cases.jsonl") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        d = json.loads(line)
        text = " ".join(str(m.get("content") or "") for m in d["request"].get("messages", [])).strip()
        if text:
            by_tier[d["expected_tier"]].append((d["id"], d["expected_tier"], text))

# Stratify so the sample spans the ladder instead of following file order.
per = max(1, N // len(by_tier))
sample = [c for tier in by_tier for c in by_tier[tier][:per]][:N]
print(f"sampled {len(sample)} prompts, {per}/tier\n", flush=True)


def call(url, key, model, text, timeout=150, max_tokens=160):
    body = json.dumps(
        {"model": model, "messages": [{"role": "user", "content": text}], "max_tokens": max_tokens}
    ).encode()
    req = urllib.request.Request(
        url, data=body,
        headers={"Content-Type": "application/json", "Authorization": "Bearer " + key},
    )
    try:
        d = json.load(urllib.request.urlopen(req, timeout=timeout))
    except Exception as e:
        return None
    u = d.get("usage") or {}
    msg = (d.get("choices") or [{}])[0].get("message") or {}
    content = msg.get("content")
    # Some routers emit reasoning-only turns with null content.
    if not content:
        rd = msg.get("reasoning_details") or []
        if rd and isinstance(rd[0], dict):
            content = rd[0].get("text")
    return {
        "model": d.get("model") or "?",
        "cost": float(u.get("cost") or 0.0),
        "text": (content or "").strip(),
    }


def judge(prompt, answer):
    if not answer:
        return 1
    rubric = (
        "Score the answer 1-5 for correctness and helpfulness against the request.\n"
        "5 = fully correct and complete, 4 = correct with a minor gap, "
        "3 = partially correct, 2 = mostly wrong, 1 = wrong or empty.\n"
        "Reply with ONLY the digit.\n\n"
        f"REQUEST:\n{prompt[:900]}\n\nANSWER:\n{answer[:900]}"
    )
    r = call(OR, OR_KEY, JUDGE, rubric, max_tokens=4)
    if not r:
        return None
    m = re.search(r"[1-5]", r["text"])
    return int(m.group(0)) if m else None


rows = []
m_models, j_models = collections.Counter(), collections.Counter()
m_cost = j_cost = 0.0
m_scores, j_scores = [], []

for i, (cid, tier, text) in enumerate(sample, 1):
    m = call(MISER, MISER_KEY, "auto", text)
    time.sleep(0.3)
    j = call(OR, OR_KEY, "typesafe/jev-router", text)
    time.sleep(0.3)
    if not m or not j:
        continue
    ms = judge(text, m["text"])
    js = judge(text, j["text"])
    time.sleep(0.3)
    if ms is not None:
        m_scores.append(ms)
    if js is not None:
        j_scores.append(js)
    m_models[m["model"]] += 1
    j_models[j["model"]] += 1
    m_cost += m["cost"]
    j_cost += j["cost"]
    rows.append((tier, ms, js, m["model"], j["model"]))
    if i % 10 == 0:
        print(f"  {i}/{len(sample)}  miser={statistics.mean(m_scores):.2f} "
              f"jev={statistics.mean(j_scores):.2f}", flush=True)

n = len(rows)
print(f"\n=== {n} paired comparisons ===")
print(f"quality  miser {statistics.mean(m_scores):.3f}   jev-router {statistics.mean(j_scores):.3f}")
print(f"cost     miser ${m_cost:.6f}   jev-router ${j_cost:.6f}")
wins = sum(1 for _, a, b, _, _ in rows if a is not None and b is not None and a > b)
loss = sum(1 for _, a, b, _, _ in rows if a is not None and b is not None and a < b)
print(f"quality  miser better on {wins}, worse on {loss}, tied on {n - wins - loss}")
print(f"\ndistinct models: miser {len(m_models)}  jev-router {len(j_models)}")
print(f"  miser      {dict(m_models)}")
print(f"  jev-router {dict(j_models)}")
json.dump(rows, open("/tmp/opencode/head-to-head.json", "w"))
