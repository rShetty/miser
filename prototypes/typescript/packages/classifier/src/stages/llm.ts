import type { LLMClassifierConfig } from "@miser/config";
import type { ClassifiableRequest, StageResult } from "../types.ts";
import type { ComplexityTier } from "@miser/config";

const TIER_MAP: Record<string, ComplexityTier> = {
  trivial: "trivial",
  simple: "simple",
  standard: "standard",
  hard: "hard",
  reasoning: "reasoning",
};

const CLASSIFIER_PROMPT = `Classify the request by the minimum model capability needed to complete it reliably.
Return only JSON: {"tier":"trivial|simple|standard|hard|reasoning","confidence":0.0,"reason":"brief"}.

Rubric:
- trivial: greetings, confirmations, basic facts/math, listing files, git status, tiny mechanical edits
- simple: explanations, summaries, one small function, straightforward formatting or conversion
- standard: normal feature implementation, debugging, tests, module refactoring, API or database work
- hard: architecture, security review, production incidents, broad migrations, complex concurrency or multi-file changes
- reasoning: proofs, novel algorithms, formal logic, constraint optimization, consensus correctness

Judge required capability, not keyword presence. A request mentioning a hard topic may still be simple if it only asks for a definition. Tool use and strict schemas increase instruction-following requirements but do not automatically make a request hard.`;

interface LLMClassResult {
  tier: string;
  confidence: number;
  reason: string;
}

export interface LLMStageMetrics {
  model: string;
  promptTokens?: number;
  completionTokens?: number;
}

function serializeRequest(req: ClassifiableRequest): string {
  return JSON.stringify({
    messages: req.messages,
    tools: req.tools?.map((tool) => {
      if (!tool || typeof tool !== "object") return tool;
      const value = tool as Record<string, unknown>;
      const fn = value.function as Record<string, unknown> | undefined;
      return { type: value.type, name: fn?.name, description: fn?.description };
    }),
    tool_choice: req.tool_choice,
    response_format: req.response_format,
    max_tokens: req.max_tokens,
  }).slice(0, 12000);
}

export async function classify(
  req: ClassifiableRequest,
  options: LLMClassifierConfig,
  stageName: "local_llm" | "cloud_llm"
): Promise<StageResult> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), options.timeout_ms);

  try {
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (options.api_key) headers.Authorization = `Bearer ${options.api_key}`;

    const response = await fetch(`${options.base_url.replace(/\/$/, "")}/chat/completions`, {
      method: "POST",
      headers,
      signal: controller.signal,
      body: JSON.stringify({
        model: options.model,
        messages: [
          { role: "system", content: CLASSIFIER_PROMPT },
          { role: "user", content: serializeRequest(req) },
        ],
        max_tokens: 300,
        temperature: 0,
        think: false,
        response_format: { type: "json_object" },
      }),
    });

    if (!response.ok) return { matched: false };
    const data = (await response.json()) as {
      model?: string;
      choices?: Array<{ message?: { content?: string } }>;
      usage?: { prompt_tokens?: number; completion_tokens?: number };
    };
    const content = data.choices?.[0]?.message?.content;
    if (!content) return { matched: false };

    const json = content.match(/\{[\s\S]*\}/)?.[0];
    if (!json) return { matched: false };
    const parsed = JSON.parse(json) as LLMClassResult;
    const tier = TIER_MAP[String(parsed.tier).toLowerCase().trim()];
    if (!tier) return { matched: false };
    const confidence = Number(parsed.confidence);

    return {
      matched: true,
      tier,
      confidence: Number.isFinite(confidence) ? Math.max(0, Math.min(confidence, 0.99)) : 0.7,
      signals: [
        `${stageName}:${data.model ?? options.model}`,
        `reason:${String(parsed.reason ?? "").slice(0, 100)}`,
        `tokens:${data.usage?.prompt_tokens ?? 0}+${data.usage?.completion_tokens ?? 0}`,
      ],
    };
  } catch {
    return { matched: false };
  } finally {
    clearTimeout(timeout);
  }
}
