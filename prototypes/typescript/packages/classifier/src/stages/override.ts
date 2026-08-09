import type { ClassifiableRequest, StageResult } from "../types.ts";
import type { ComplexityTier } from "@miser/config";

const OVERRIDE_PATTERNS: Array<{ regex: RegExp; tier: ComplexityTier; signal: string }> = [
  { regex: /^@fast\b/i, tier: "trivial", signal: "@fast override" },
  { regex: /^@cheap\b/i, tier: "trivial", signal: "@cheap override" },
  { regex: /^@simple\b/i, tier: "simple", signal: "@simple override" },
  { regex: /^@standard\b/i, tier: "standard", signal: "@standard override" },
  { regex: /^@think\b/i, tier: "reasoning", signal: "@think override" },
  { regex: /^@deep\b/i, tier: "reasoning", signal: "@deep override" },
  { regex: /^@hard\b/i, tier: "hard", signal: "@hard override" },
  { regex: /^@reasoning\b/i, tier: "reasoning", signal: "@reasoning override" },
  { regex: /^@opus\b/i, tier: "hard", signal: "@opus override" },
  { regex: /^@max\b/i, tier: "reasoning", signal: "@max override" },
];

export function classify(req: ClassifiableRequest): StageResult {
  const lastUserMsg = [...req.messages].reverse().find((m) => m.role === "user");
  if (!lastUserMsg) return { matched: false };

  const text =
    typeof lastUserMsg.content === "string"
      ? lastUserMsg.content
      : JSON.stringify(lastUserMsg.content);

  for (const pattern of OVERRIDE_PATTERNS) {
    if (pattern.regex.test(text)) {
      return {
        matched: true,
        tier: pattern.tier,
        confidence: 1.0,
        signals: [pattern.signal],
      };
    }
  }

  return { matched: false };
}
