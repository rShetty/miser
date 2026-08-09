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
    'anthropic/claude-sonnet-4': (3.0, 15.0),
    'openai/o4-mini': (1.10, 4.40),
    'z-ai/glm-5.2': (0.59, 0.59),
    'openrouter/auto': (0.0, 0.0),
}

def cost_for(model, prompt_tokens, completion_tokens):
    p = PRICING.get(model, (0,0))
    return round(prompt_tokens/1e6*p[0] + completion_tokens/1e6*p[1], 6)

def judge_quality(case, response_text):
    if not KEY:
        coverage = sum(x.lower() in response_text.lower() for x in case['required'])/max(1,len(case['required']))
        if case['task']=='structured':
            try: json.loads(response_text); valid_json=True
            except: valid_json=False
            if not valid_json: coverage*=.25
        return coverage
    body = {'model':'z-ai/glm-5.2','messages':[
        {'role':'system','content':'You are a quality judge. Score the response 0.0-1.0 for correctness, completeness, and relevance. Return only JSON: {"score":0.0,"passed":true}'},
        {'role':'user','content':f'Task: {case["prompt"]}\n\nResponse: {response_text[:3000]}\n\nRequired elements: {", ".join(case["required"])}'}
    ],'temperature':0,'max_tokens':100,'response_format':{'type':'json_object'}}
    req = urllib.request.Request(OR, data=json.dumps(body).encode(), headers={'Content-Type':'application/json','Authorization':'Bearer '+KEY}, method='POST')
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            data=json.loads(r.read())
        content=data['choices'][0]['message']['content']
        match=re.search(r'\{.*\}',content,re.DOTALL)
        if match:
            result=json.loads(match.group(0))
            return float(result.get('score',0.5))
    except: pass
    coverage = sum(x.lower() in response_text.lower() for x in case['required'])/max(1,len(case['required']))
    return coverage

def call(case, endpoint, model):
    body = {'model': model, 'messages': [{'role':'user','content':case['prompt']}], 'temperature':0, 'max_tokens':800}
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
        usage=data.get('usage',{}) or {}
        actual_model=data.get('model',model)
        pt=usage.get('prompt_tokens',0); ct=usage.get('completion_tokens',0)
        cost=cost_for(actual_model, pt, ct)
        cache_status=response_headers.get('x-miser-cache','none')
        judge_score=judge_quality(case, text)
        return {'ok':True,'id':case['id'],'score':judge_score,'latency_ms':(time.perf_counter()-start)*1000,
                'prompt_tokens':pt,'completion_tokens':ct,'cost_usd':cost,
                'model':actual_model,'route_tier':response_headers.get('x-miser-tier'),
                'route_model':response_headers.get('x-miser-model'),
                'cache':cache_status,'text_len':len(text)}
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
        cache_hits=sum(1 for x in good if 'hit' in x.get('cache','none'))
        route_tiers=sorted(set(x.get('route_tier') for x in good if x.get('route_tier')))
        route_models=sorted(set(x.get('route_model') for x in good if x.get('route_model')))
        quality_scores=[x['score'] for x in out]
        summary={'strategy':name,'model':model,'cases':len(out),'successes':len(good),'failures':len(out)-len(good),
                 'quality_mean':round(sum(quality_scores)/len(out),4),
                 'quality_pass_pct':round(sum(s>=0.7 for s in quality_scores)/len(out)*100,1),
                 'latency_p50_ms':round(p50,1),'latency_p95_ms':round(p95,1),'latency_p99_ms':round(p99,1),
                 'wall_ms':round(wall,1),'prompt_tokens':prompt,'completion_tokens':completion,
                 'total_cost_usd':round(total_cost,6),'cache_hits':cache_hits,
                 'cost_per_quality_point':round(total_cost/max(0.01,sum(quality_scores)/len(out)),6),
                 'tokens_per_quality_point':round((prompt+completion)/max(1,sum(quality_scores)),1),
                 'route_tiers':route_tiers,'route_models':route_models,
                 'errors':[x.get('error') for x in out if not x['ok']]}
        print(json.dumps(summary,indent=2)); allout.append({'summary':summary,'cases':out})
    report={'timestamp':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),'judge':'z-ai/glm-5.2','cases':CASES,'results':allout}
    path=ROOT/'results'/'completion-quality-judged.json'; path.write_text(json.dumps(report,indent=2))
    print('REPORT='+str(path))
main()
