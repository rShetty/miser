import json, os, re, time, urllib.request, urllib.error
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path('/opt/miser')
CASES = [json.loads(x) for x in (ROOT/'evals/quality_cases.jsonl').read_text().splitlines() if x.strip()]
KEY = re.search(r'sk-[A-Za-z0-9_-]+', os.environ.get('OPENROUTER_API_KEY','')).group(0) if os.environ.get('OPENROUTER_API_KEY') else ''
OR = 'https://openrouter.ai/api/v1/chat/completions'
GATEWAY = 'http://127.0.0.1:8787/v1/chat/completions'
MODELS = [('miser_auto', GATEWAY, 'auto'), ('openrouter_auto', OR, 'openrouter/auto'), ('gpt_4_1_mini', OR, 'openai/gpt-4.1-mini')]

PRICING = {
    'openai/gpt-4.1-mini': (0.40, 1.60),
    'deepseek/deepseek-chat': (0.14, 0.28),
    'meta-llama/llama-3.2-3b-instruct:free': (0.0, 0.0),    'anthropic/claude-sonnet-4': (3.0, 15.0),
    'anthropic/claude-opus-4': (15.0, 75.0),
    'openai/o4-mini': (1.10, 4.40),
    'z-ai/glm-5.2': (0.59, 0.59),
    'openrouter/auto': (0.0, 0.0),
}

def cost_for(model, prompt_tokens, completion_tokens):
    p = PRICING.get(model, (0,0))
    return round(prompt_tokens/1e6*p[0] + completion_tokens/1e6*p[1], 6)

def call(case, endpoint, model):
    body = {'model': model, 'messages': [{'role':'user','content':case['prompt']}], 'temperature':0, 'max_tokens':500}
    headers = {'Content-Type':'application/json'}
    if endpoint == OR: headers['Authorization'] = 'Bearer '+KEY
    else: headers['Authorization'] = 'Bearer local'
    req = urllib.request.Request(endpoint, data=json.dumps(body).encode(), headers=headers, method='POST')
    start=time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            data=json.loads(r.read())
            response_headers=dict(r.headers)
        text=data.get('choices',[{}])[0].get('message',{}).get('content','') or ''
        required=case['required']; lower=text.lower()
        coverage=sum(x.lower() in lower for x in required)/max(1,len(required))
        if case['task']=='structured':
            try: json.loads(text); valid_json=True
            except Exception: valid_json=False
            if not valid_json: coverage*=.25
        usage=data.get('usage',{}) or {}
        actual_model=data.get('model',model)
        pt=usage.get('prompt_tokens',0); ct=usage.get('completion_tokens',0)
        cost=cost_for(actual_model, pt, ct)
        return {'ok':True,'id':case['id'],'score':coverage,'latency_ms':(time.perf_counter()-start)*1000,
                'prompt_tokens':pt,'completion_tokens':ct,'cost_usd':cost,
                'model':actual_model,'route_tier':response_headers.get('x-miser-tier'),
                'route_model':response_headers.get('x-miser-model'),
                'quality_header':response_headers.get('x-miser-quality-score'),'text_len':len(text)}
    except Exception as e:
        return {'ok':False,'id':case['id'],'score':0,'latency_ms':(time.perf_counter()-start)*1000,
                'prompt_tokens':0,'completion_tokens':0,'cost_usd':0,'error':type(e).__name__+': '+str(e)[:180]}

def main():
    allout=[]
    for name,endpoint,model in MODELS:
        start=time.perf_counter(); out=[]
        with ThreadPoolExecutor(max_workers=2) as pool:
            fs=[pool.submit(call,c,endpoint,model) for c in CASES]
            for f in as_completed(fs): out.append(f.result())
        wall=(time.perf_counter()-start)*1000; good=[x for x in out if x['ok']]
        latencies=sorted(x['latency_ms'] for x in out)
        p50=latencies[len(latencies)//2]
        p95=latencies[min(len(latencies)-1,int(len(latencies)*.95))]
        p99=latencies[min(len(latencies)-1,int(len(latencies)*.99))]
        prompt=sum(x.get('prompt_tokens',0) for x in out)
        completion=sum(x.get('completion_tokens',0) for x in out)
        total_cost=sum(x.get('cost_usd',0) for x in out)
        route_tiers=sorted(set(x.get('route_tier') for x in good if x.get('route_tier')))
        route_models=sorted(set(x.get('route_model') for x in good if x.get('route_model')))
        summary={'strategy':name,'model':model,'cases':len(out),'successes':len(good),'failures':len(out)-len(good),
                 'quality_mean':round(sum(x['score'] for x in out)/len(out),4),
                 'quality_pass_pct':round(sum(x['score']>=.7 for x in out)/len(out)*100,1),
                 'latency_p50_ms':round(p50,1),'latency_p95_ms':round(p95,1),'latency_p99_ms':round(p99,1),
                 'wall_ms':round(wall,1),'prompt_tokens':prompt,'completion_tokens':completion,
                 'total_cost_usd':round(total_cost,6),'route_tiers':route_tiers,'route_models':route_models,
                 'errors':[x.get('error') for x in out if not x['ok']]}
        print(json.dumps(summary,indent=2)); allout.append({'summary':summary,'cases':out})
    report={'timestamp':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),'cases':CASES,'results':allout}
    path=ROOT/'results'/'completion-quality-optimized.json'; path.write_text(json.dumps(report,indent=2))
    print('REPORT='+str(path))
main()
