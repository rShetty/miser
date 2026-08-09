import json, os, re, time, urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path('/opt/miser')
CASES = [json.loads(x) for x in (ROOT/'evals/se_quality_cases.jsonl').read_text().splitlines() if x.strip()]
KEY = re.search(r'sk-[A-Za-z0-9_-]+', os.environ.get('OPENROUTER_API_KEY','')).group(0) if os.environ.get('OPENROUTER_API_KEY') else ''
OR = 'https://openrouter.ai/api/v1/chat/completions'
GATEWAY = 'http://127.0.0.1:8787/v1/chat/completions'
MISER_KEY = os.environ.get('MISER_USER_KEY', '')
MODELS = [('miser_auto', GATEWAY, 'auto', MISER_KEY), ('openrouter_auto', OR, 'openrouter/auto', KEY), ('gpt_4_1_mini', OR, 'openai/gpt-4.1-mini', KEY), ('glm_5_2', OR, 'z-ai/glm-5.2', KEY), ('claude_sonnet', OR, 'anthropic/claude-sonnet-4', KEY)]

TIERS = ["trivial", "simple", "standard", "hard", "reasoning"]

def classify_accuracy(case, actual_tier):
    return 1.0 if actual_tier == case["expected_tier"] else 0.0

def judge_quality(case, response_text):
    if not response_text or len(response_text.strip()) < 5:
        return 0.0
    if not KEY:
        coverage = sum(x.lower() in response_text.lower() for x in case["required"])/max(1,len(case["required"]))
        return coverage
    body = {
        "model": "z-ai/glm-5.2",
        "messages": [
            {"role": "system", "content": "You are a strict code quality judge. Score the response 0.0 to 1.0 for correctness, completeness, and relevance to the prompt. Return only JSON: {\"score\": 0.0, \"passed\": true}"},
            {"role": "user", "content": f"Task: {case['prompt']}\n\nResponse (truncated): {response_text[:3000]}\n\nRequired concepts: {', '.join(case['required'])}\n\nScore this response."}
        ],
        "temperature": 0,
        "max_tokens": 80,
        "response_format": {"type": "json_object"}
    }
    req = urllib.request.Request(OR, data=json.dumps(body).encode(), headers={"Content-Type": "application/json", "Authorization": "Bearer " + KEY}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            data = json.loads(r.read())
        content = data["choices"][0]["message"]["content"]
        match = re.search(r'\{.*\}', content, re.DOTALL)
        if match:
            result = json.loads(match.group(0))
            return float(result.get("score", 0.5))
    except:
        pass
    coverage = sum(x.lower() in response_text.lower() for x in case["required"])/max(1,len(case["required"]))
    return coverage

def call(case, endpoint, model, auth_key):
    body = {"model": model, "messages": [{"role": "user", "content": case["prompt"]}], "temperature": 0, "max_tokens": 600}
    headers = {"Content-Type": "application/json", "Authorization": "Bearer " + auth_key}
    req = urllib.request.Request(endpoint, data=json.dumps(body).encode(), headers=headers, method="POST")
    start = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            data = json.loads(r.read())
            response_headers = dict(r.headers)
        text = data.get("choices", [{}])[0].get("message", {}).get("content", "") or ""
        usage = data.get("usage", {}) or {}
        actual_model = data.get("model", model)
        pt = usage.get("prompt_tokens", 0)
        ct = usage.get("completion_tokens", 0)
        route_tier = response_headers.get("x-miser-tier", "")
        route_model = response_headers.get("x-miser-model", "")
        cache_status = response_headers.get("x-miser-cache", "none")
        judge_score = judge_quality(case, text)
        tier_correct = classify_accuracy(case, route_tier) if route_tier else 0.0
        return {
            "ok": True, "id": case["id"], "expected_tier": case["expected_tier"],
            "routed_tier": route_tier, "tier_correct": tier_correct,
            "quality_score": judge_score, "latency_ms": (time.perf_counter() - start) * 1000,
            "prompt_tokens": pt, "completion_tokens": ct, "model": actual_model,
            "cache": cache_status, "text_len": len(text)
        }
    except Exception as e:
        return {"ok": False, "id": case["id"], "expected_tier": case["expected_tier"],
                "tier_correct": 0.0, "quality_score": 0.0, "latency_ms": (time.perf_counter() - start) * 1000,
                "prompt_tokens": 0, "completion_tokens": 0, "error": type(e).__name__ + ": " + str(e)[:150]}

def main():
    allout = []
    for name, endpoint, model, auth_key in MODELS:
        print(f"Running {name}...", flush=True)
        start = time.perf_counter()
        out = []
        with ThreadPoolExecutor(max_workers=3) as pool:
            fs = [pool.submit(call, c, endpoint, model, auth_key) for c in CASES]
            for f in as_completed(fs):
                out.append(f.result())
        wall = (time.perf_counter() - start) * 1000
        good = [x for x in out if x["ok"]]
        latencies = sorted(x["latency_ms"] for x in out)
        p50 = latencies[len(latencies)//2]
        p95 = latencies[min(len(latencies)-1, int(len(latencies)*.95))]
        p99 = latencies[min(len(latencies)-1, int(len(latencies)*.99))]
        prompt_tokens = sum(x.get("prompt_tokens", 0) for x in out)
        completion_tokens = sum(x.get("completion_tokens", 0) for x in out)
        total_tokens = prompt_tokens + completion_tokens
        quality_scores = [x["quality_score"] for x in out]
        tier_scores = [x["tier_correct"] for x in good] if good else [0]
        per_tier = {}
        for x in good:
            t = x["expected_tier"]
            if t not in per_tier:
                per_tier[t] = {"count": 0, "tier_correct": 0, "quality": []}
            per_tier[t]["count"] += 1
            per_tier[t]["tier_correct"] += x["tier_correct"]
            per_tier[t]["quality"].append(x["quality_score"])
        tier_acc_by_tier = {}
        for t, stats in per_tier.items():
            tier_acc_by_tier[t] = round(stats["tier_correct"] / stats["count"], 4) if stats["count"] else 0
        summary = {
            "strategy": name, "model": model, "cases": len(out), "successes": len(good), "failures": len(out) - len(good),
            "quality_mean": round(sum(quality_scores) / len(out), 4),
            "quality_pass_pct": round(sum(s >= 0.7 for s in quality_scores) / len(out) * 100, 1),
            "classification_accuracy": round(sum(tier_scores) / len(tier_scores), 4) if tier_scores else 0,
            "per_tier_classification": tier_acc_by_tier,
            "latency_p50_ms": round(p50, 1), "latency_p95_ms": round(p95, 1), "latency_p99_ms": round(p99, 1),
            "wall_ms": round(wall, 1),
            "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "total_tokens": total_tokens,
            "tokens_per_quality_point": round(total_tokens / max(1, sum(quality_scores)), 1),
            "errors": [x.get("error") for x in out if not x["ok"]]
        }
        print(json.dumps(summary, indent=2), flush=True)
        allout.append({"summary": summary, "cases": out})
    report = {"timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "judge": "z-ai/glm-5.2", "corpus": "se_quality_cases.jsonl", "results": allout}
    path = ROOT / "results" / "se-benchmark.json"
    path.write_text(json.dumps(report, indent=2))
    print("REPORT=" + str(path))

main()
