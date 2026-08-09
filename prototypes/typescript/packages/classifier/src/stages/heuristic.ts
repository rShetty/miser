import type { ClassifiableRequest, StageResult } from "../types.ts";

const TRIVIAL_PATTERNS: RegExp[] = [
  /^(what|how|why)\s+(is|are|do|does)\b.*\?$/i,
  /\b(hi|hello|hey|thanks|thank you|ok|okay|got it)\b/i,
  /\b(git status|git diff|git log|git branch)\b/i,
  /\b(list|show|display)\s+(files|dir|directory|contents?)\b/i,
  /\b(what\s+(time|date|day))\b/i,
  /\b(capitalize|lowercase|uppercase|trim)\b/i,
  /\b(rename|move|copy|delete)\s+(a\s+)?(file|var|variable|function|class)\b/i,
  /^\s*(yes|no|y|n|true|false)\s*$/i,
];

const SIMPLE_PATTERNS: RegExp[] = [
  /\b(add|create|make|generate|write)\s+(a\s+)?(function|method|class|component|test|file)\b/i,
  /\b(format|lint|fix|refactor)\s+(the\s+)?(code|file|imports?)\b/i,
  /\b(explain|describe|summarize|outline)\b/i,
  /\b(compare|difference between)\b/i,
  /\b(convert|translate|transform|parse)\b/i,
  /\b(docs|documentation|comment|readme)\b/i,
  /\b(type|interface|schema|types?)\b.*\b(typescript|ts)\b/i,
];

const STANDARD_PATTERNS: RegExp[] = [
  /\b(implement|build|develop|integrate)\s+(a\s+)?(feature|system|module|service)\b/i,
  /\b(debug|troubleshoot|diagnose|investigate)\b/i,
  /\b(refactor|restructure|reorganize)\s+(the\s+)?(architecture|codebase|project)\b/i,
  /\b(optimize|improve|enhance)\s+(performance|speed|memory)\b/i,
  /\b(api|endpoint|route|middleware)\s+(design|implementation|creation)\b/i,
  /\b(database|schema|migration|query)\s+(design|optimization)\b/i,
  /\b(write|create|generate)\s+(unit\s+tests?|integration\s+tests?|e2e\s+tests?)\b/i,
];

const HARD_PATTERNS: RegExp[] = [
  /\b(architect|design)\s+(a\s+)?(distributed|microservice|scalable|enterprise)\b/i,
  /\b(production\s+)?(incident|outage|failure|bug)\s+(analysis|postmortem|root cause)\b/i,
  /\b(security|vulnerability|exploit|attack\s+vector|threat\s+model)\b/i,
  /\b(concurrency|race\s+condition|deadlock|thread\s+safety)\b/i,
  /\b(performance\s+)?(profiling|benchmarking|bottleneck)\b/i,
  /\b(migrate|migration)\s+(from|to)\s+/i,
  /\b(orchestrate|coordinate|choreograph)\s+/i,
];

const REASONING_PATTERNS: RegExp[] = [
  /\b(prove|proof|theorem|lemma|corollary)\b/i,
  /\b(algorithm|complexity|big-o|np-complete|halting\s+problem)\b/i,
  /\b(formal\s+)?(logic|verification|specification)\b/i,
  /\b(consensus|byzantine|paxos|raft)\b/i,
  /\b(mathematical|statistical|probabilistic)\s+(proof|derivation|analysis)\b/i,
  /\b(reason|reasoning|step.by.step|chain.of.thought)\b/i,
  /\b(optimize|optimization)\s+(algorithm|strategy|approach)\b/i,
];

interface RuleSet {
  tier: import("@miser/config").ComplexityTier;
  patterns: RegExp[];
  label: string;
}

const RULE_SETS: RuleSet[] = [
  { tier: "trivial", patterns: TRIVIAL_PATTERNS, label: "trivial" },
  { tier: "simple", patterns: SIMPLE_PATTERNS, label: "simple" },
  { tier: "standard", patterns: STANDARD_PATTERNS, label: "standard" },
  { tier: "hard", patterns: HARD_PATTERNS, label: "hard" },
  { tier: "reasoning", patterns: REASONING_PATTERNS, label: "reasoning" },
];

export function classify(req: ClassifiableRequest): StageResult {
  const lastUserMsg = [...req.messages].reverse().find((m) => m.role === "user");
  if (!lastUserMsg) return { matched: false };

  const text =
    typeof lastUserMsg.content === "string"
      ? lastUserMsg.content
      : JSON.stringify(lastUserMsg.content);

  const matches: Array<{ tier: RuleSet["tier"]; count: number }> = [];

  for (const ruleSet of RULE_SETS) {
    let count = 0;
    for (const pattern of ruleSet.patterns) {
      if (pattern.test(text)) count++;
    }
    if (count > 0) {
      matches.push({ tier: ruleSet.tier, count });
    }
  }

  if (matches.length === 0) return { matched: false };

  matches.sort((a, b) => b.count - a.count);
  const best = matches[0];
  const totalMatches = matches.reduce((sum, m) => sum + m.count, 0);
  const confidence = Math.min(0.5 + best.count / (totalMatches * 2), 0.85);

  const signals = matches.map(
    (m) => `heuristic:${m.tier}(${m.count} match${m.count > 1 ? "es" : ""})`
  );

  return {
    matched: true,
    tier: best.tier,
    confidence,
    signals,
  };
}
