"""RouterBench offline replay for Miser (Mode 1).

Replays Miser's production classifier (Jev) over a stratified sample of
RouterBench prompts, maps each tier decision to the capability-equivalent
model in RouterBench's 2024 pool, and looks up the pre-computed outcome.
Zero inference cost; classification spend ~$0.25 for 5k prompts.
"""
import json, os, re, time, urllib.request, pickle, random
from concurrent.futures import ThreadPoolExecutor, as_completed
from collections import defaultdict

import pandas as pd

JEV_KEY = os.environ["JEV_API_KEY"]
JEV_URL = "https://api.typesafe.ai/v1/systemone"
SAMPLE_N = int(os.environ.get('RB_SAMPLE_N', '5000'))
CONFIDENCE_THRESHOLD = 0.65

# Miser tier -> capability-equivalent model in RouterBench's 2024 pool.
# trivial(mistral-nemo)->mistral-7b, simple(qwen3-30b)->mixtral-8x7b,
# standard/hard/reasoning(gpt-4.1-mini)->gpt-4-1106-preview.
TIER_TO_MODEL = {
    "trivial": "mistralai/mistral-7b-chat",
    "simple": "mistralai/mixtral-8x7b-chat",
    "standard": "gpt-4-1106-preview",
    "hard": "gpt-4-1106-preview",
    "reasoning": "gpt-4-1106-preview",
}

def jev_classify(prompt_text):
    body = {
        "model": "jev-latest",
        "state": {"request": prompt_text, "tools": [], "tool_history": False},
        "questions": {
            "tier": {
                "type": "choice",
                "instructions": "Classify the minimum capability tier required to answer this request. Judge the work and model capability required, never keywords: technical jargon in a trivial request stays trivial, and closing or small-talk messages stay trivial regardless of which technologies they mention. Decide from the inside out: is the answer one bare fact (date, arithmetic, protocol constant, yes/no) with no concept explanation? Then trivial. Asked to explain a concept, produce an artifact (snippet, regex, query, command, translation), or make a small edit? Simple, even when brief. Multi-file feature work, debugging, or infrastructure configuration? Standard. Designing or analyzing a system at scale, production incidents, security threat modeling, distributed transactions or migrations, large cross-service refactors, or mutating or multi-step tool operations? Hard. Formal proofs, complexity or algorithm analysis, derivations, correctness or optimality arguments? Reasoning.",
                "criteria": {
                    "trivial": "Greetings, thanks, and conversation-closing small talk, pure yes/no answers, bare facts like dates or port numbers, one-command lookups without tools, and tiny mechanical text edits such as rename or uppercase - even if they name complex technology",
                    "simple": "Explaining a concept even briefly, writing a snippet, regex, query, command, or single test, translating or summarizing text, single-file changes",
                    "standard": "Implementing features, debugging failures, multi-file or multi-component changes, API/database/schema work, CI or infrastructure configuration with substance, or single read-only tool operations",
                    "hard": "Architecture or system design at scale (high traffic, many services, many regions), production incident analysis, threat modeling, distributed transactions or migrations, large cross-service refactors, or mutating or multi-step tool operations",
                    "reasoning": "Formal proofs, complexity or algorithm analysis, derivations, correctness or optimality arguments"
                }
            }
        }
    }
    req = urllib.request.Request(JEV_URL, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "Authorization": f"Bearer {JEV_KEY}"},
                                 method="POST")
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                data = json.loads(r.read())
            tier = data["answers"]["tier"]["choice"]
            conf = data["answers"]["tier"].get("confidence")
            if conf is None:
                probs = data["answers"]["tier"].get("probabilities", {})
                conf = probs.get(tier, 0.0)
            return tier, float(conf)
        except Exception:
            if attempt == 2:
                return None, 0.0
            time.sleep(1.0 + attempt)

def effective_tier(tier, conf):
    # Mirror PolicyEngine::effective_tier (no tools, no response_format):
    # low confidence floors the tier at standard.
    order = ["trivial", "simple", "standard", "hard", "reasoning"]
    if conf < CONFIDENCE_THRESHOLD:
        i = max(order.index(tier), order.index("standard"))
        return order[i]
    return tier

def main():
    df = pd.read_pickle(os.environ.get('ROUTERBENCH_PKL', '/tmp/routerbench_raw.pkl'))
    # One row per (sample_id, model); sample prompts, stratified by eval_name.
    prompts = df[['sample_id', 'prompt', 'eval_name']].drop_duplicates('sample_id')
    random.seed(42)
    n_per = max(1, SAMPLE_N // prompts.eval_name.nunique())
    parts = []
    for ev, g in prompts.groupby('eval_name'):
        parts.append(g.sample(min(len(g), n_per), random_state=42))
    sampled = pd.concat(parts)[['sample_id', 'prompt', 'eval_name']]
    sampled = sampled.sample(min(SAMPLE_N, len(sampled)), random_state=42)
    print(f"sampled {len(sampled)} prompts across {sampled['eval_name'].nunique()} evals")

    def prompt_text(p):
        # prompts are JSON-ish lists of message dicts/lists
        try:
            msgs = json.loads(p) if isinstance(p, str) else p
            texts = []
            for m in msgs:
                if isinstance(m, dict) and 'content' in m:
                    texts.append(str(m['content']))
                else:
                    texts.append(str(m))
            return "\n".join(texts)[:6000]
        except Exception:
            return str(p)[:6000]

    rows = [(r.sample_id, r.eval_name, prompt_text(r.prompt)) for r in sampled.itertuples()]
    results = {}
    with ThreadPoolExecutor(max_workers=10) as ex:
        futs = {ex.submit(jev_classify, t): sid for sid, _, t in rows}
        done = 0
        for f in as_completed(futs):
            sid = futs[f]
            results[sid] = f.result()
            done += 1
            if done % 500 == 0:
                print(f"  classified {done}/{len(rows)}")

    failures = sum(1 for v in results.values() if v[0] is None)
    print(f"classification failures: {failures}")

    # Lookup table: (sample_id, model) -> (performance, cost)
    lookup = df.set_index(['sample_id', 'model_name'])[['performance', 'cost']].to_dict('index')

    # Strategies evaluated on the same prompts
    strategies = {
        'miser_replay': {},      # tier-mapped model per prompt
        'best_fixed': {},        # gpt-4-1106-preview (best fixed model)
        'cheapest': {},          # mistral-7b (cheapest)
        'oracle': {},            # best model per prompt
        'cheapest_correct': {},  # cheapest model with performance==1.0 (RouterBench marginal)
    }
    missing = 0
    for sid, eval_name, _ in rows:
        tier, conf = results.get(sid, (None, 0.0))
        if tier is None:
            missing += 1
            continue
        et = effective_tier(tier, conf)
        model = TIER_TO_MODEL[et]
        for name, pick in [
            ('miser_replay', model),
            ('best_fixed', 'gpt-4-1106-preview'),
            ('cheapest', 'mistralai/mistral-7b-chat'),
        ]:
            cell = lookup.get((sid, pick))
            if cell:
                strategies[name][sid] = (cell['performance'], cell['cost'], eval_name)
        # oracle + marginal
        cand = [(m, c) for (s, m), c in lookup.items() if s == sid] if False else None
        per_model = df[df.sample_id == sid][['model_name', 'performance', 'cost']]
        best = per_model.loc[per_model.performance.idxmax()]
        strategies['oracle'][sid] = (best.performance, best.cost, eval_name)
        solved = per_model[per_model.performance >= 0.99]
        pick = solved.loc[solved.cost.idxmin()] if len(solved) else per_model.loc[per_model.performance.idxmax()]
        strategies['cheapest_correct'][sid] = (pick.performance, pick.cost, eval_name)

    out = {}
    print(f"\n{'strategy':18s} {'quality':>8s} {'cost/call':>10s} {'cost/qual':>10s} {'n':>6s}")
    for name, s in strategies.items():
        if not s: continue
        perf = [v[0] for v in s.values()]
        cost = [v[1] for v in s.values()]
        q, c = sum(perf)/len(perf), sum(cost)/len(cost)
        out[name] = {'quality': round(q, 4), 'cost_per_call': round(c, 6), 'n': len(s), 'missing': missing}
        print(f"{name:18s} {q:8.4f} {c:10.6f} {c/q if q else 0:10.6f} {len(s):6d}")

    # category breakdown for miser vs best_fixed vs oracle
    cats = defaultdict(lambda: defaultdict(list))
    for name in ('miser_replay', 'best_fixed', 'cheapest', 'oracle'):
        for sid, (p, c, ev) in strategies[name].items():
            cats[ev][name].append((p, c))
    print("\n=== per-eval quality (miser / best_fixed / oracle) ===")
    breakdown = {}
    for ev in sorted(cats):
        row = {}
        for name in ('miser_replay', 'best_fixed', 'oracle'):
            vals = cats[ev][name]
            if vals:
                row[name] = round(sum(p for p, _ in vals)/len(vals), 3)
        breakdown[ev] = row
        print(f"  {ev:34s} {row}")

    with open(os.environ.get('RB_OUT', '/tmp/routerbench_replay.json'), 'w') as f:
        json.dump({'summary': out, 'per_eval': breakdown, 'sampled': len(rows),
                   'classification_failures': failures}, f, indent=1)
    print('\nsaved ' + os.environ.get('RB_OUT', '/tmp/routerbench_replay.json'))

main()