import type { ClassifiableRequest, StageResult } from "../types.ts";

const TIER_KEYWORDS: Record<import("@miser/config").ComplexityTier, string[]> = {
  trivial: [
    "hello", "hi", "hey", "thanks", "ok", "yes", "no",
    "what is", "list files", "git status", "rename",
  ],
  simple: [
    "explain", "summarize", "format", "fix lint", "add comment",
    "write a function", "create test", "convert", "compare",
    "typescript type", "simple script", "documentation",
  ],
  standard: [
    "implement feature", "debug", "refactor code", "optimize",
    "api endpoint", "database query", "write tests", "integrate",
    "build module", "troubleshoot", "diagnose",
  ],
  hard: [
    "architect", "design distributed", "production incident",
    "security vulnerability", "concurrency", "race condition",
    "performance profiling", "migrate", "orchestrate", "scale",
  ],
  reasoning: [
    "prove", "theorem", "algorithm complexity", "formal logic",
    "consensus protocol", "byzantine", "mathematical proof",
    "optimization strategy", "step by step reasoning",
  ],
};

const TIER_VECTORS: Record<string, number[]> = {};

function tokenize(text: string): string[] {
  return text
    .toLowerCase()
    .replace(/[^\w\s]/g, " ")
    .split(/\s+/)
    .filter((t) => t.length > 0);
}

function buildVector(text: string, vocab: Map<string, number>): number[] {
  const tokens = tokenize(text);
  const vec = new Array(vocab.size).fill(0);
  for (const token of tokens) {
    const idx = vocab.get(token);
    if (idx !== undefined) vec[idx]++;
  }
  const magnitude = Math.sqrt(vec.reduce((s, v) => s + v * v, 0));
  if (magnitude > 0) {
    for (let i = 0; i < vec.length; i++) vec[i] /= magnitude;
  }
  return vec;
}

function cosineSim(a: number[], b: number[]): number {
  let dot = 0;
  for (let i = 0; i < a.length; i++) dot += a[i] * b[i];
  return dot;
}

let initialized = false;
let vocab: Map<string, number> = new Map();

function init() {
  if (initialized) return;
  const allKeywords = new Set<string>();
  for (const keywords of Object.values(TIER_KEYWORDS)) {
    for (const kw of keywords) {
      for (const tok of tokenize(kw)) allKeywords.add(tok);
    }
  }
  let idx = 0;
  for (const word of allKeywords) vocab.set(word, idx++);

  for (const [tier, keywords] of Object.entries(TIER_KEYWORDS)) {
    const combined = keywords.join(" ");
    TIER_VECTORS[tier] = buildVector(combined, vocab);
  }
  initialized = true;
}

export function classify(req: ClassifiableRequest): StageResult {
  init();

  const lastUserMsg = [...req.messages].reverse().find((m) => m.role === "user");
  if (!lastUserMsg) return { matched: false };

  const text =
    typeof lastUserMsg.content === "string"
      ? lastUserMsg.content
      : JSON.stringify(lastUserMsg.content);

  const reqVec = buildVector(text, vocab);
  if (reqVec.every((v) => v === 0)) return { matched: false };

  const scores: Array<{ tier: import("@miser/config").ComplexityTier; sim: number }> = [];
  for (const tier of Object.keys(TIER_VECTORS) as import("@miser/config").ComplexityTier[]) {
    const sim = cosineSim(reqVec, TIER_VECTORS[tier]);
    scores.push({ tier, sim });
  }

  scores.sort((a, b) => b.sim - a.sim);
  const best = scores[0];

  if (best.sim < 0.05) return { matched: false };

  const confidence = Math.min(best.sim * 2.5, 0.8);
  const signals = scores
    .filter((s) => s.sim > 0)
    .map((s) => `ml:${s.tier}(${s.sim.toFixed(3)})`)
    .slice(0, 3);

  return {
    matched: true,
    tier: best.tier,
    confidence,
    signals,
  };
}
