#!/usr/bin/env python3
"""Generate the large prompt-classification tuning corpus.

Deterministic output (fixed seeds, no RNG). Labels are fixed by the
template's work level, never by tier keywords, per docs/EVALUATION.md:

  trivial   = single short factual sentence, greetings, one-command lookups
  simple    = one concept, snippet, regex, query, single-file edit
  standard  = multi-file feature/debug/infrastructure work
  hard      = system design at scale, incidents, threat models, migrations
  reasoning = formal proofs, derivations, correctness arguments

Usage: scripts/generate_classifier_corpus.py [--out PATH] [--repeat N]
Default out: evals/classifier_cases_large.jsonl
"""
import argparse
import json
from pathlib import Path

STACKS = ["Node.js", "Python", "Go", "Rust", "Java", "TypeScript"]
SERVICES = ["payments", "auth", "search", "inventory", "notifications", "billing"]
FEATURES = [
    "rate limiting", "caching", "pagination", "authentication",
    "webhook delivery", "audit logging", "file uploads", "email delivery",
]

TRIVIAL = []
TRIVIAL += ["hello", "hey!", "hi there", "thanks, that fixed it", "ok sounds good",
            "thank you!", "perfect, done", "got it", "yep", "no"]
TRIVIAL += [f"git {cmd}" for cmd in
            ["status", "diff", "log", "branch", "show", "stash list", "remote -v"]]
TRIVIAL += [
    "what is 2+2?", "what is your name?", "what's today's date?",
    "just answer yes or no: is Python interpreted?",
    "just answer yes or no: does Rust have a garbage collector?",
    "just answer yes or no: is HTTP stateless?",
    "yes or no: can a primary key be null?",
    "yes or no: is JSON a binary format?",
    "how many days are in a leap year?",
    "what does HTTP 404 mean?",
    "rename variable x to y in line 4",
    "uppercase the string 'deploy status'",
    "trim the whitespace from '  build.log  '",
    "lowercase this header name: Content-Type",
    "what port does HTTPS use?",
    "who created Git?",
    "what year was the Linux kernel released?",
    "spell 'kubernetes' backwards, just for fun",
    "what does 'CRDT' stand for? one sentence",
    "what is a bloom filter? two sentences max",
    "just say 'distributed consensus' in a sentence so I can quote you",
    "hello! no need to do anything with {svc} today, just saying hi",
    "thanks! that's all I needed about {feat}",
    "no questions about {stack}, just checking you're there",
]

SIMPLE_CONCEPTS = [
    "Docker networking", "HTTP caching", "JWT", "OAuth2", "DNS resolution",
    "TCP vs UDP", "CAP theorem", "database indexes", "REST vs GraphQL", "WebSockets",
    "garbage collection", "DNS TTL", "ACID transactions", "idempotency",
    "exponential backoff", "connection pooling", "TLS handshakes",
    "database indexes", "async/await", "event loops", "memoization",
    "Bloom filters", "consistent hashing", "circuit breakers", "CORS",
    "CSS specificity", "recursion", "big-O notation", "regular expressions",
    "semantic versioning",
]
SIMPLE_ARTIFACTS = [
    "write a regex that matches email addresses",
    "write a regex for ISO dates (YYYY-MM-DD)",
    "write a regex that matches the literal string 'jwt'",
    "write a SQL query to select users created after 2024",
    "write a SQL query counting orders per customer",
    "write a curl command to POST JSON to an API",
    "write a curl command with a Bearer token header",
    "write a unit test for a function that adds two numbers",
    "write a unit test for a password validator",
    "write a bash one-liner to count lines in all .py files",
    "write a bash one-liner to find the largest file in a repo",
    "write a one-line awk to sum the second column of a CSV",
    "write a small {stack} function to reverse a string",
    "write a small {stack} function to deduplicate a list",
    "define a TypeScript interface for a user with id, name, email",
    "define a JSON schema for an address object",
    "write a simple Dockerfile for a Node app",
    "write a .gitignore for a Python project",
    "give me the git command to undo the last commit",
    "give me the git command to see who changed a line",
    "convert this JSON to YAML: {{name: miser, port: 8787}}",
    "convert this cron expression to plain English: 0 9 * * 1-5",
    "add a null check to this function",
    "fix the trailing whitespace in these lines",
]
SIMPLE_EXPLAIN = [
    "explain {concept} in a few sentences",
    "what's the difference between {conceptA} and {conceptB}?",
    "how do I fix a CORS error?",
    "how do I add a dependency with pip?",
    "what is a CI/CD pipeline?",
    "how does {concept} work?",
    "explain how {concept} works in one short paragraph",
    "give me a one-line curl command to check the kubernetes pods status",
    "translate 'good morning' to French",
    "translate 'deployment complete' to German",
    "summarize this paragraph in one sentence: The gateway routes each request to the cheapest model that can handle it, preserving unknown OpenAI fields and forwarding streaming responses.",
    "what semicolons do in JavaScript, explained simply",
]

STANDARD = []
for feat in FEATURES:
    for stack in STACKS:
        STANDARD.append(f"implement {feat} for our {stack} service")
        STANDARD.append(f"debug why {feat} fails intermittently in {stack}")
for svc in SERVICES:
    STANDARD += [
        f"refactor the {svc} service to use a connection pool",
        f"add retry logic with exponential backoff to the {svc} client",
        f"write integration tests for the {svc} API",
        f"add request validation to the {svc} endpoints",
        f"optimize the slow queries in the {svc} module",
        f"write a GitHub Actions workflow that tests and deploys {svc}",
        f"containerize the {svc} service and add it to docker-compose",
        f"add structured logging and request IDs to {svc}",
    ]
STANDARD += [
    "design the database schema for a multi-tenant orders system",
    "add dataloader batching to prevent N+1 queries",
    "implement cursor-based pagination for the orders API",
    "configure nginx as a reverse proxy with TLS termination",
    "write a Terraform module for a VPC with public and private subnets",
    "set up a Kubernetes deployment manifest with liveness probes",
    "implement a queue consumer with a dead-letter queue",
    "implement a payment webhook with idempotency handling",
    "optimize the React list rendering, it re-renders on every keystroke",
    "build a React form with validation and error states",
    "implement WebSocket reconnection with jittered backoff",
    "migrate this REST controller to GraphQL resolvers",
    "write a load test script for the checkout endpoint",
    "add property-based tests for the pricing calculator",
    "implement AES-256 encryption for PII fields at rest",
    "upgrade the ORM across the models without breaking migrations",
    "split this monolith module into a package with clean boundaries",
    "implement exponential backoff with a token bucket limiter",
]

HARD = []
for svc in SERVICES:
    HARD += [
        f"design the {svc} system for one million requests per second across five regions",
        f"write the postmortem for the {svc} outage: latency spiked to 30s, then the cluster failed over",
        f"design a zero-downtime migration plan for the {svc} database across regions",
        f"threat-model the {svc} service before the PCI-DSS audit",
        f"plan the {svc} extraction from the monolith across 40 services without downtime",
        f"design the failover runbook and SLO/RTO/RPO for {svc}",
    ]
HARD += [
    "design end-to-end encryption for our chat app using the Signal protocol",
    "architect an event-sourcing system with CRDT-based consistency for offline collaboration",
    "design a saga-based distributed transaction across inventory, payments, and shipping",
    "design a consistent hashing ring for the distributed cache and analyze rebalancing",
    "plan SSO with SAML and SCIM provisioning across the whole organization",
    "investigate the deadlock in the connection pool under concurrent migrations",
    "design a secrets management rollout with Vault and field-level encryption",
    "design a notification system that fans out to 100M users with per-channel quotas",
    "plan a chaos engineering rollout for the checkout path with mutation testing",
    "refactor the 80-file auth module across five services without downtime",
    "design the scheduler for a distributed job system with strict ordering guarantees",
    "analyze the outage: queue backpressure caused cascading retries; design the fix",
    "design a multi-region active-active architecture for the payment gateway",
    "design the observability stack for 200 microservices with trace sampling policy",
    "plan the migration from per-service IAM to a service mesh with mTLS",
]

REASONING = [
    "prove that the amortized cost of dynamic array insertion is O(1)",
    "derive the posterior distribution for a conjugate Bayesian update with a Beta prior",
    "prove the halting problem is undecidable",
    "show a reduction from 3-SAT to vertex cover and bound the complexity",
    "analyze the recurrence T(n) = 2T(n/2) + n with the master theorem",
    "prove the correctness of Dijkstra's algorithm with non-negative weights",
    "prove that a distributed counter with CRDT merge converges",
    "analyze the serialization graph for this schedule and check for anomalies",
    "prove a lower bound of Omega(n log n) for comparison sorting",
    "formally prove the greedy algorithm for interval scheduling is optimal",
    "derive the closed form for the Fibonacci recurrence via generating functions",
    "prove that primality testing is in P using AKS, at a high level",
    "prove the sandwich theorem for the quicksort recurrence",
    "derive the expectation and variance of the coupon collector problem",
    "prove that B-trees guarantee O(log n) lookups with bounded fanout",
    "prove the type-soundness statement for a simply typed lambda calculus fragment",
    "derive the Kalman filter update equations from Bayes' rule",
    "prove that context-free languages are closed under union",
    "analyze the amortized complexity of splay tree operations",
    "prove the optimality of Huffman coding",
]

CONCEPT_PAIRS = [
    ("REST", "GraphQL"), ("TCP", "UDP"), ("npm", "yarn"),
    ("Docker", "Kubernetes"), ("SQL", "NoSQL"), ("monolith", "microservices"),
    ("optimistic", "pessimistic locking"), ("B-trees", "LSM-trees"),
    ("docker-compose", "kubernetes"), ("processes", "threads"),
]


def build_cases(repeat: int):
    cases = []

    prefix = {"trivial": "tv", "simple": "sm", "standard": "st",
              "hard": "hd", "reasoning": "rs"}

    def add(tier, text):
        for _ in range(repeat):
            cases.append({
                "id": f"{prefix[tier]}{len(cases):05d}",
                "expected_tier": tier,
                "request": {"model": "auto",
                            "messages": [{"role": "user", "content": text}]},
            })

    for t in TRIVIAL:
        add("trivial", t.format(svc="kubernetes", feat="migrations", stack="Go"))
    for concept in SIMPLE_CONCEPTS:
        add("simple", f"explain {concept} in a few sentences")
    for a, b in CONCEPT_PAIRS:
        add("simple", f"what's the difference between {a} and {b}?")
    for t in SIMPLE_ARTIFACTS:
        add("simple", t.format(stack="Python"))
    for t in SIMPLE_EXPLAIN:
        add("simple", t.format(concept="database indexes", conceptA="REST", conceptB="GraphQL"))
    for t in STANDARD:
        add("standard", t)
    for t in HARD:
        add("hard", t)
    for t in REASONING:
        add("reasoning", t)
    return cases


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="evals/classifier_cases_large.jsonl")
    parser.add_argument("--repeat", type=int, default=1,
                        help="duplicate every template N times (ids stay unique)")
    args = parser.parse_args()

    cases = build_cases(max(1, args.repeat))
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as f:
        for case in cases:
            f.write(json.dumps(case) + "\n")

    from collections import Counter
    print(f"wrote {len(cases)} cases -> {out}")
    print(dict(Counter(c["expected_tier"] for c in cases)))


if __name__ == "__main__":
    main()
