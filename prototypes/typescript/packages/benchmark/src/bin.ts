import { classify } from "@miser/classifier";
import { DEFAULT_CONFIG, type ClassifierConfig, type ComplexityTier } from "@miser/config";
import { EVAL_CASES, type EvalCase } from "./evals.ts";

const TIERS: ComplexityTier[] = ["trivial", "simple", "standard", "hard", "reasoning"];
type Strategy = "heuristic" | "local_llm" | "cloud_llm" | "openrouter_auto" | "hybrid";

interface Result {
  strategy: Strategy;
  id: string;
  category: string;
  expected: ComplexityTier;
  predicted?: ComplexityTier;
  confidence: number;
  stage: string;
  latencyMs: number;
  promptTokens: number;
  completionTokens: number;
  error?: string;
}

function arg(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function configFor(strategy: Strategy): ClassifierConfig {
  const base = structuredClone(DEFAULT_CONFIG.classifier);
  const localModel = arg("--local-model") ?? process.env.MISER_LOCAL_MODEL ?? "gemma4:26b";
  const localBaseUrl = arg("--local-url") ?? process.env.MISER_LOCAL_URL ?? "http://127.0.0.1:11434/v1";
  const rawKey = process.env.OPENAI_KEY ?? process.env.OPENAI_API_KEY ?? process.env.OPENROUTER_API_KEY ?? "";
  const configuredKey = rawKey.match(/sk-[A-Za-z0-9_-]+/)?.[0] ?? rawKey;
  const usesOpenRouter = configuredKey.startsWith("sk-or-") || Boolean(process.env.OPENROUTER_API_KEY);
  const cloudModel = arg("--cloud-model") ?? process.env.MISER_CLOUD_MODEL ?? (usesOpenRouter ? "openai/gpt-4.1-mini" : "gpt-4.1-mini");
  const cloudBaseUrl = arg("--cloud-url") ?? process.env.MISER_CLOUD_URL ?? (usesOpenRouter ? "https://openrouter.ai/api/v1" : "https://api.openai.com/v1");
  const cloudKey = configuredKey;

  return {
    ...base,
    mode: strategy === "openrouter_auto" ? "cloud_llm" : strategy,
    local_llm: { enabled: true, model: localModel, base_url: localBaseUrl, timeout_ms: 120000 },
    cloud_llm: {
      enabled: true,
      model: strategy === "openrouter_auto" ? "openrouter/auto" : cloudModel,
      base_url: cloudBaseUrl,
      api_key: cloudKey,
      timeout_ms: 120000,
    },
  };
}

function parseTokens(signals: string[]): { prompt: number; completion: number } {
  const match = signals.find((signal) => signal.startsWith("tokens:"))?.match(/tokens:(\d+)\+(\d+)/);
  return { prompt: Number(match?.[1] ?? 0), completion: Number(match?.[2] ?? 0) };
}

async function evaluate(strategy: Strategy, test: EvalCase): Promise<Result> {
  const start = performance.now();
  try {
    const output = await classify(test.request, configFor(strategy));
    const tokens = parseTokens(output.signals);
    const failedLLM = output.stage === "default" && strategy !== "heuristic" && strategy !== "hybrid";
    return {
      strategy,
      id: test.id,
      category: test.category,
      expected: test.expected,
      predicted: failedLLM ? undefined : output.tier,
      confidence: output.confidence,
      stage: output.stage,
      latencyMs: performance.now() - start,
      promptTokens: tokens.prompt,
      completionTokens: tokens.completion,
    };
  } catch (error) {
    return {
      strategy,
      id: test.id,
      category: test.category,
      expected: test.expected,
      confidence: 0,
      stage: "error",
      latencyMs: performance.now() - start,
      promptTokens: 0,
      completionTokens: 0,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

async function mapConcurrent<T, R>(items: T[], concurrency: number, fn: (item: T) => Promise<R>): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  async function worker(): Promise<void> {
    while (next < items.length) {
      const index = next++;
      results[index] = await fn(items[index]);
      process.stdout.write(".");
    }
  }
  await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, worker));
  return results;
}

function percentile(values: number[], p: number): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))];
}

function summarize(strategy: Strategy, results: Result[]): void {
  const valid = results.filter((result) => result.predicted && !result.error);
  const exact = valid.filter((result) => result.predicted === result.expected).length;
  const adjacent = valid.filter((result) => Math.abs(TIERS.indexOf(result.predicted!) - TIERS.indexOf(result.expected)) <= 1).length;
  const latencies = valid.map((result) => result.latencyMs);
  const totalPromptTokens = valid.reduce((sum, result) => sum + result.promptTokens, 0);
  const totalCompletionTokens = valid.reduce((sum, result) => sum + result.completionTokens, 0);

  console.log(`\n\n${strategy}`);
  console.log("=".repeat(strategy.length));
  console.log(`Cases: ${results.length} | Successful: ${valid.length} | Failures: ${results.length - valid.length}`);
  console.log(`Exact accuracy: ${(exact / results.length * 100).toFixed(1)}% (${exact}/${results.length})`);
  console.log(`Adjacent accuracy: ${(adjacent / results.length * 100).toFixed(1)}% (${adjacent}/${results.length})`);
  console.log(`Latency: mean ${(latencies.reduce((a, b) => a + b, 0) / Math.max(1, latencies.length)).toFixed(1)}ms | p50 ${percentile(latencies, 0.5).toFixed(1)}ms | p95 ${percentile(latencies, 0.95).toFixed(1)}ms`);
  console.log(`Classifier tokens: ${totalPromptTokens} input + ${totalCompletionTokens} output`);

  console.log("\nPer-tier exact accuracy:");
  for (const tier of TIERS) {
    const tierResults = results.filter((result) => result.expected === tier);
    const correct = tierResults.filter((result) => result.predicted === tier).length;
    console.log(`  ${tier.padEnd(10)} ${(correct / tierResults.length * 100).toFixed(1).padStart(5)}% (${correct}/${tierResults.length})`);
  }

  console.log("\nConfusion matrix (expected rows, predicted columns):");
  console.log(`  ${"".padEnd(11)}${TIERS.map((tier) => tier.slice(0, 4).padStart(6)).join("")}`);
  for (const expected of TIERS) {
    const row = TIERS.map((predicted) => results.filter((result) => result.expected === expected && result.predicted === predicted).length);
    console.log(`  ${expected.padEnd(11)}${row.map((count) => String(count).padStart(6)).join("")}`);
  }

  const errors = results.filter((result) => result.predicted !== result.expected);
  console.log(`\nMisclassified (${errors.length}):`);
  for (const result of errors) {
    console.log(`  ${result.id} [${result.category}] expected=${result.expected} predicted=${result.predicted ?? "FAIL"} stage=${result.stage}${result.error ? ` error=${result.error}` : ""}`);
  }
}

async function main(): Promise<void> {
  const selected = (arg("--strategies") ?? "heuristic,local_llm,cloud_llm,openrouter_auto,hybrid")
    .split(",") as Strategy[];
  const limit = Number(arg("--limit") ?? EVAL_CASES.length);
  const concurrency = Number(arg("--concurrency") ?? 1);
  const cases = EVAL_CASES.slice(0, limit);
  const runnable = selected.filter((strategy) => {
    if (strategy === "cloud_llm" && !(process.env.OPENAI_KEY || process.env.OPENAI_API_KEY || process.env.OPENROUTER_API_KEY)) {
      console.log("Skipping cloud_llm: no cloud API key is set");
      return false;
    }
    if (strategy === "openrouter_auto") {
      const rawKey = process.env.OPENROUTER_API_KEY ?? process.env.OPENAI_KEY ?? "";
      const key = rawKey.match(/sk-[A-Za-z0-9_-]+/)?.[0] ?? rawKey;
      if (!key.startsWith("sk-or-")) {
        console.log("Skipping openrouter_auto: no OpenRouter-formatted key is set");
        return false;
      }
    }
    return true;
  });

  console.log(`Evaluating ${cases.length} cases across: ${runnable.join(", ")}`);
  console.log(`Concurrency: ${concurrency}`);

  for (const strategy of runnable) {
    process.stdout.write(`\n${strategy} `);
    const results = await mapConcurrent(cases, concurrency, (test) => evaluate(strategy, test));
    summarize(strategy, results);
  }
}

await main();
