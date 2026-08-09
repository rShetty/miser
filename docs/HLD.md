# High-Level Design

## 1. Purpose

Miser is a remotely hosted, low-latency AI gateway. Any OpenAI-compatible harness sends one request to Miser. Miser classifies the workload, applies safety and capability policy, chooses a cost-efficient model, and forwards the request to OpenRouter.

The gateway is stateless in the MVP. It does not persist prompts, completions, or API keys.

## 2. Goals

- OpenAI-compatible chat-completions ingress.
- Transparent streaming and tool-call forwarding.
- Heuristic, local-LLM, cloud-LLM, and hybrid classification modes.
- Cost-aware tier-to-model routing.
- Conservative fallback when a classifier is unavailable.
- Low classification latency on common requests.
- Deployable as a non-root VPS service or OCI container.
- Reproducible offline and live evaluation.
- Safe open-source operation without committed secrets.

## 3. Non-goals for the MVP

- Training a proprietary foundation model.
- Semantic response caching by default.
- Storing prompt bodies or completions.
- Arbitrary user-selected upstream URLs.
- Multi-region consensus or a distributed database.
- Guaranteed quality equivalence between models.

## 4. Context and deployment

```text
OpenCode / Codex / Aider / OpenAI SDK
                 |
                 | OpenAI-compatible HTTP
                 v
        Miser Gateway on VPS
        :8787 / TLS proxy
                 |
       +---------+----------+
       |                    |
 local classifier       cloud classifier
 optional Ollama       optional OpenRouter
       |                    |
       +---------+----------+
                 v
        Policy and model route
                 |
                 v
        OpenRouter completion API
                 |
                 v
        Selected model/provider
```

The gateway is the central integration point. A future edge daemon may perform local-only heuristics before forwarding to this gateway, but the gateway must remain fully functional without an edge component.

## 5. Main components

### Gateway server

Axum handles health, model discovery, and chat completions. It validates bearer authentication, creates request metadata, invokes classification, applies policy, and streams the provider response.

### Shared types

`miser-types` defines OpenAI request envelopes, content parts, flattened unknown fields, complexity tiers, classifier configuration, provider configuration, and route configuration.

### Classifier

`miser-classifier` contains deterministic overrides, structural features, compiled regex sets, task tagging, and OpenAI-compatible LLM classification. It supports four modes:

- `heuristic`: deterministic local path only.
- `local_llm`: configured local OpenAI-compatible endpoint.
- `cloud_llm`: configured cloud endpoint.
- `hybrid`: accept high-confidence heuristics, then attempt bounded local/cloud classification.

### Policy

`miser-policy` maps a classification tier to a configured model route. Capability filtering and budget constraints are planned extensions; the current route table is explicit and auditable.

### Provider

`miser-provider` owns OpenRouter forwarding, provider preference injection, model listing, classifier calls, safe response headers, and upstream response status handling.

### Evaluator

`miser-evals` consumes versioned JSONL cases and reports per-case labels, exact accuracy, adjacent-tier accuracy, and a confusion matrix.

## 6. Request lifecycle

1. Client connects to `POST /v1/chat/completions`.
2. Gateway authenticates the bearer token when ingress auth is configured.
3. Gateway parses the OpenAI envelope while preserving unknown fields.
4. Classifier checks a valid route override at byte zero.
5. Classifier computes structural and lexical signals.
6. Depending on mode, classifier calls configured local/cloud endpoint under a timeout.
7. Gateway selects the route for the resulting tier.
8. Gateway rewrites only policy-controlled fields such as `model` and route output limits.
9. Provider forwards to OpenRouter with provider preferences.
10. Gateway returns the upstream status, safe headers, and byte stream.
11. Routing headers identify request ID, tier, model, classifier, and confidence.

## 7. Availability and degradation

- Heuristics remain available if Ollama is down.
- Cloud classification is optional and bounded by its own timeout.
- Classification failure falls back to the heuristic result.
- Provider failure is returned to the client in the current MVP; provider retries/circuit breakers are planned.
- Systemd restarts a crashed process.
- Readiness confirms process configuration and route availability, not an uncached upstream request.

## 8. Scalability

The gateway is stateless and can run behind a reverse proxy or load balancer. Horizontal replicas require only shared configuration and independent upstream connection pools. Future shared rate limits and budgets require an external store or gateway-level quota service.

Latency priorities:

1. Avoid model classification when a high-confidence heuristic is sufficient.
2. Keep classifier timeouts lower than completion deadlines.
3. Reuse reqwest connection pools.
4. Stream provider bytes without buffering.
5. Avoid prompt logging and synchronous persistence in the request path.

## 9. Observability

The service emits structured tracing logs without prompt bodies or credentials. Planned metrics include request count, classification duration, selected tier/model, provider status, time-to-first-byte, and upstream token usage. Labels must not contain prompts, raw API keys, or unbounded user-controlled values.
