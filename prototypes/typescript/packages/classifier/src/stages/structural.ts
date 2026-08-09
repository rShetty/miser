import type { ClassifiableRequest, StageResult } from "../types.ts";

const COMPLEXITY_SIGNALS = {
  hasTools: "tools present — tool use requires capable model",
  hasToolChoice: "tool_choice specified — agentic workflow",
  jsonSchema: "response_format is json_schema — structured output",
  longConversation: "messages.length > 10 — deep conversation context",
  longSystemPrompt: "system prompt > 2000 chars — complex instructions",
  longUserMessage: "user message > 8000 chars — large input processing",
};

const SIMPLICITY_SIGNALS = {
  shortMaxTokens: "max_tokens < 100 — short output likely simple",
  lowTemperature: "temperature < 0.3 — factual/deterministic query",
  singleMessage: "single user message — no conversation depth",
  shortPrompt: "user message < 200 chars — brief query",
};

export function classify(req: ClassifiableRequest): StageResult {
  const signals: string[] = [];
  let complexityScore = 0;

  if (req.tools && req.tools.length > 0) {
    signals.push(COMPLEXITY_SIGNALS.hasTools);
    complexityScore += 3;
  }

  if (req.tool_choice) {
    signals.push(COMPLEXITY_SIGNALS.hasToolChoice);
    complexityScore += 2;
  }

  if (req.response_format?.type === "json_schema") {
    signals.push(COMPLEXITY_SIGNALS.jsonSchema);
    complexityScore += 2;
  }

  if (req.messages.length > 10) {
    signals.push(COMPLEXITY_SIGNALS.longConversation);
    complexityScore += 2;
  }

  const systemMsg = req.messages.find((m) => m.role === "system");
  if (systemMsg) {
    const sysLen =
      typeof systemMsg.content === "string"
        ? systemMsg.content.length
        : JSON.stringify(systemMsg.content).length;
    if (sysLen > 2000) {
      signals.push(COMPLEXITY_SIGNALS.longSystemPrompt);
      complexityScore += 2;
    }
  }

  const lastUserMsg = [...req.messages].reverse().find((m) => m.role === "user");
  if (lastUserMsg) {
    const userLen =
      typeof lastUserMsg.content === "string"
        ? lastUserMsg.content.length
        : JSON.stringify(lastUserMsg.content).length;
    if (userLen > 8000) {
      signals.push(COMPLEXITY_SIGNALS.longUserMessage);
      complexityScore += 2;
    }
  }

  if (req.max_tokens !== undefined && req.max_tokens < 100) {
    signals.push(SIMPLICITY_SIGNALS.shortMaxTokens);
    complexityScore -= 2;
  }

  if (req.temperature !== undefined && req.temperature < 0.3) {
    signals.push(SIMPLICITY_SIGNALS.lowTemperature);
    complexityScore -= 1;
  }

  if (req.messages.length === 1) {
    signals.push(SIMPLICITY_SIGNALS.singleMessage);
    complexityScore -= 1;
  }

  if (lastUserMsg) {
    const userLen =
      typeof lastUserMsg.content === "string"
        ? lastUserMsg.content.length
        : JSON.stringify(lastUserMsg.content).length;
    if (userLen < 200) {
      signals.push(SIMPLICITY_SIGNALS.shortPrompt);
      complexityScore -= 1;
    }
  }

  if (complexityScore >= 5) {
    return { matched: true, tier: "hard", confidence: 0.8, signals };
  }
  if (complexityScore >= 3) {
    return { matched: true, tier: "standard", confidence: 0.7, signals };
  }
  if (complexityScore <= -5) {
    return { matched: true, tier: "trivial", confidence: 0.75, signals };
  }
  if (complexityScore <= -3) {
    return { matched: true, tier: "simple", confidence: 0.6, signals };
  }

  return { matched: false };
}
