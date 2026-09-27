#!/usr/bin/env python3
"""Head-to-head, VARIANT B: jev-router with the free/Stealth provider excluded.

Distinct from the default-config run. Do not merge the two numbers -- this
answers "how good is jev-router when it must spend money?", the other answers
"what does a user get from the endpoint?".

Also adds a single retry on transport failure, applied symmetrically to both
routers, so an 8-12% 504 rate is not silently scored as a quality failure.
"""
import json, os, re, statistics, time, urllib.request, collections, sys

OR_KEY = os.environ["OPENROUTER_API_KEY"]
MISER_KEY = os.environ["MISER_API_KEY"]
OR = "https://openrouter.ai/api/v1/chat/completions"
MISER = "http://127.0.0.1:8787/v1/chat/completions"
JUDGE = "openai/gpt-4.1"
N = int(sys.argv[1]) if len(sys.argv) > 1 else 100

# Excludes the "Stealth" provider, which is where stealth/space-bunny-alpha
# (the $0 model) is served from.
NO_FREE = {"provider": {"only": ["OpenAI", "Wafer", "Anthropic", "Google", "DeepSeek"]}}

by_tier = collections.defaultdict(list)
with open("evals/classifier_cases.jsonl") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        d = json.loads(line)
        t = " ".join(str(m.get("content") or "") for m in d["request"].get("messages", [])).strip()
        if t:
            by_tier[d["expected_tier"]].append((d["id"], d["expected_tier"], t))
per = max(1, N // len(by_tier))
sample = [c for tier in by_tier for c in by_tier[tier][:per]][:N]
print(f"variant B: jev-router providers constrained, {len(sample)} prompts\n", flush=True)

fails = collections.Counter()


def call(url, key, model, text, extra=None, timeout=150, max_tokens=160, retry=True):
    body = {"model": model, "messages": [{"role": "user", "content": text}], "max_tokens": max_tokens}
    if extra:
        body.update(extra)
    for attempt in range(2 if retry else 1):
        req = urllib.request.Request(
            url, data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json", "Authorization": "Bearer " + key})
        try:
            d = json.load(urllib.request.urlopen(req, timeout=timeout))
        except Exception:
            if attempt == 0:
                time.sleep(2.0)
                continue
            fails[model] += 1
            return None
        u = d.get("usage") or {}
        msg = (d.get("choices") or [{}])[0].get("message") or {}
        c = msg.get("content")
        if not c:
            rd = msg.get("reasoning_details") or []
            if rd and isinstance(rd[0], dict):
                c = rd[0].get("text")
        return {"model": d.get("model") or "?", "cost": float(u.get("cost") or 0.0),
                "text": (c or "").strip()}
    return None


def judge(prompt, answer):
    if not answer:
        return 1
    r = call(OR, OR_KEY, JUDGE,
             ("Score the answer 1-5 for correctness and helpfulness against the request.\n"
              "5 = fully correct and complete, 4 = correct with a minor gap, 3 = partially "
              "correct, 2 = mostly wrong, 1 = wrong or empty.\nReply with ONLY the digit.\n\n"
              f"REQUEST:\n{prompt[:900]}\n\nANSWER:\n{answer[:900]}"),
             max_tokens=4, retry=False)
    if not r:
        return None
    m = re.search(r"[1-5]", r["text"])
    return int(m.group(0)) if m else None


mm, jm = collections.Counter(), collections.Counter()
mc = jc = 0.0
ms_all, js_all, rows = [], [], []
for i, (cid, tier, text) in enumerate(sample, 1):
    m = call(MISER, MISER_KEY, "auto", text)
    time.sleep(0.3)
    j = call(OR, OR_KEY, "typesafe/jev-router", text, extra=NO_FREE)
    time.sleep(0.3)
    if not m or not j:
        continue
    a, b = judge(text, m["text"]), judge(text, j["text"])
    time.sleep(0.3)
    if a is not None:
        ms_all.append(a)
    if b is not None:
        js_all.append(b)
    mm[m["model"]] += 1
    jm[j["model"]] += 1
    mc += m["cost"]
    jc += j["cost"]
    rows.append((tier, a, b, m["model"], j["model"]))
    if i % 20 == 0:
        print(f"  {i}/{len(sample)}  miser={statistics.mean(ms_all):.2f} "
              f"jev={statistics.mean(js_all):.2f}", flush=True)

n = len(rows)
wins = sum(1 for _, a, b, _, _ in rows if a and b and a > b)
loss = sum(1 for _, a, b, _, _ in rows if a and b and a < b)
print(f"\n=== variant B: {n} paired ===")
print(f"quality  miser {statistics.mean(ms_all):.3f}   jev-router(paid) {statistics.mean(js_all):.3f}")
print(f"cost     miser ${mc:.6f}   jev-router ${jc:.6f}")
print(f"quality  miser better {wins}, worse {loss}, tied {n - wins - loss}")
print(f"\nmodels  miser {len(mm)}  jev {len(jm)}")
print(f"  miser {dict(mm)}")
print(f"  jev   {dict(jm)}")
print(f"\ntransport failures after 1 retry: {dict(fails)}")
