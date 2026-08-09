import json
import os
import re
import subprocess
import sys
import time
import urllib.request
import urllib.error
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

TIERS = ["Trivial", "Simple", "Standard", "Hard", "Reasoning"]
ROOT = Path(sys.argv[1] if len(sys.argv) > 1 else "/opt/miser")
CORPUS = ROOT / "evals" / "cases.jsonl"
RESULTS = ROOT / "results"
LOCAL_URL = os.getenv("MISER_LOCAL_URL", "http://127.0.0.1:11434/v1")
LOCAL_MODEL = os.getenv("MISER_LOCAL_MODEL", "qwen3:1.7b")
CLOUD_URL = os.getenv("MISER_CLOUD_URL", "https://openrouter.ai/api/v1")
CLOUD_MODEL = os.getenv("MISER_CLOUD_MODEL", "openai/gpt-4.1-mini")
TIMEOUT = float(os.getenv("MISER_BENCHMARK_TIMEOUT", "45"))


def key_from_env():
    raw = os.getenv("OPENROUTER_API_KEY", "")
    if raw.startswith("openrouter:"):
        raw = raw.split("openrouter:", 1)[1]
    match = re.search(r"sk-[A-Za-z0-9_-]+", raw)
    return match.group(0) if match else raw


KEY = key_from_env()


def load_cases():
    return [json.loads(line) for line in CORPUS.read_text().splitlines() if line.strip()]


def run_heuristics(cases):
    command = ["/usr/local/bin/miser-evals", "--corpus", str(CORPUS), "--mode", "heuristic"]
    started = time.perf_counter()
    output = subprocess.run(command, capture_output=True, text=True, timeout=120).stdout
    elapsed = (time.perf_counter() - started) * 1000
    parsed = {}
    for line in output.splitlines():
        match = re.match(r"^(\S+) expected=(\w+) predicted=(\w+) confidence=([0-9.]+) classifier=(\S+)", line)
        if match:
            parsed[match.group(1)] = {
                "predicted": match.group(3),
                "confidence": float(match.group(4)),
                "classifier": match.group(5),
                "latency_ms": 0.0,
                "prompt_tokens": 0,
                "completion_tokens": 0,
            }
    per_case = []
    for case in cases:
        result = parsed.get(case["id"], {"error": "missing heuristic result", "latency_ms": 0.0})
        per_case.append(result)
    return per_case, elapsed


def request_classification(case, endpoint, model, api_key):
    payload = {
        "model": model,
        "messages": [
            {"role": "system", "content": "Classify the minimum model capability required. Return only JSON with tier exactly one of trivial, simple, standard, hard, reasoning; confidence number; reason string. Judge work required, not keywords."},
            {"role": "user", "content": json.dumps(case["request"], separators=(",", ":"))[:12000]},
        ],
        "temperature": 0,
        "max_tokens": 180,
        "think": False,
        "response_format": {"type": "json_object"},
    }
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = "Bearer " + api_key
    request = urllib.request.Request(endpoint.rstrip("/") + "/chat/completions", data=json.dumps(payload).encode(), headers=headers, method="POST")
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            body = json.loads(response.read())
        latency = (time.perf_counter() - started) * 1000
        content = body["choices"][0]["message"]["content"]
        match = re.search(r"\{.*\}", content, re.DOTALL)
        parsed = json.loads(match.group(0) if match else content)
        tier = str(parsed["tier"]).strip().capitalize()
        if tier not in TIERS:
            raise ValueError("invalid tier")
        usage = body.get("usage", {})
        return {"predicted": tier, "confidence": float(parsed.get("confidence", 0.7)), "classifier": model, "latency_ms": latency, "prompt_tokens": usage.get("prompt_tokens", 0), "completion_tokens": usage.get("completion_tokens", 0)}
    except Exception as error:
        return {"error": type(error).__name__ + ": " + str(error)[:160], "latency_ms": (time.perf_counter() - started) * 1000, "prompt_tokens": 0, "completion_tokens": 0}


def run_remote(cases, endpoint, model, workers=1):
    started = time.perf_counter()
    results = [None] * len(cases)
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = {executor.submit(request_classification, case, endpoint, model, KEY): index for index, case in enumerate(cases)}
        for future in as_completed(futures):
            results[futures[future]] = future.result()
    return results, (time.perf_counter() - started) * 1000


def summarize(name, cases, results, wall_ms):
    valid = [result for result in results if result.get("predicted") in TIERS]
    exact = sum(result.get("predicted") == case["expected_tier"].capitalize() for case, result in zip(cases, results) if result.get("predicted") in TIERS)
    adjacent = sum(abs(TIERS.index(result["predicted"]) - TIERS.index(case["expected_tier"].capitalize())) <= 1 for case, result in zip(cases, results) if result.get("predicted") in TIERS)
    under = sum(TIERS.index(result["predicted"]) < TIERS.index(case["expected_tier"].capitalize()) for case, result in zip(cases, results) if result.get("predicted") in TIERS)
    latencies = sorted(result.get("latency_ms", 0) for result in results)
    p50 = latencies[len(latencies) // 2] if latencies else 0
    p95 = latencies[min(len(latencies) - 1, int(len(latencies) * 0.95))] if latencies else 0
    prompt_tokens = sum(result.get("prompt_tokens", 0) for result in results)
    completion_tokens = sum(result.get("completion_tokens", 0) for result in results)
    failures = len(results) - len(valid)
    summary = {"strategy": name, "cases": len(cases), "valid": len(valid), "failures": failures, "exact": exact, "exact_pct": round(exact / len(cases) * 100, 1), "adjacent": adjacent, "adjacent_pct": round(adjacent / len(cases) * 100, 1), "under_routing": under, "under_routing_pct": round(under / len(cases) * 100, 1), "wall_ms": round(wall_ms, 1), "p50_ms": round(p50, 1), "p95_ms": round(p95, 1), "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "errors": [result.get("error") for result in results if result.get("error")]}
    print(json.dumps(summary, indent=2))
    return summary


def main():
    RESULTS.mkdir(exist_ok=True)
    cases = load_cases()
    all_summaries = []
    heuristic_results, heuristic_wall = run_heuristics(cases)
    all_summaries.append(summarize("heuristic", cases, heuristic_results, heuristic_wall))
    if KEY:
        cloud_results, cloud_wall = run_remote(cases, CLOUD_URL, CLOUD_MODEL, workers=4)
        all_summaries.append(summarize("cloud_llm", cases, cloud_results, cloud_wall))
        auto_results, auto_wall = run_remote(cases, CLOUD_URL, "openrouter/auto", workers=4)
        all_summaries.append(summarize("openrouter_auto", cases, auto_results, auto_wall))
    else:
        print("cloud_llm/openrouter_auto skipped: missing key")
        cloud_results = [{"error": "missing key"} for _ in cases]
    local_results, local_wall = run_remote(cases, LOCAL_URL, LOCAL_MODEL, workers=1)
    all_summaries.append(summarize("local_llm", cases, local_results, local_wall))
    hybrid_results = []
    for heuristic, case in zip(heuristic_results, cases):
        if heuristic.get("predicted") and heuristic.get("confidence", 0) >= 0.72:
            hybrid_results.append(heuristic)
        else:
            hybrid_results.append(local_results[len(hybrid_results)])
    all_summaries.append(summarize("hybrid", cases, hybrid_results, sum(result.get("latency_ms", 0) for result in hybrid_results)))
    report = {"timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "hardware": {"cpu": os.cpu_count()}, "summaries": all_summaries}
    path = RESULTS / "rust-vps-comparison.json"
    path.write_text(json.dumps(report, indent=2))
    print("REPORT=" + str(path))


if __name__ == "__main__":
    main()
