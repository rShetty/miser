import type { ClassifiableRequest, ClassificationResult, StageResult } from "./types.ts";
import type { ClassifierConfig, ClassifierStageName, ComplexityTier } from "@miser/config";

import { classify as overrideClassify } from "./stages/override.ts";
import { classify as structuralClassify } from "./stages/structural.ts";
import { classify as heuristicClassify } from "./stages/heuristic.ts";
import { classify as mlClassify } from "./stages/ml.ts";
import { classify as llmClassify } from "./stages/llm.ts";

const DEFAULT_TIER: ComplexityTier = "standard";

function stagesFor(config: ClassifierConfig): ClassifierStageName[] {
  if (config.mode === "heuristic") return ["override", "structural", "heuristic", "ml"];
  if (config.mode === "local_llm") return ["override", "local_llm"];
  if (config.mode === "cloud_llm") return ["override", "cloud_llm"];
  return config.stages;
}

export async function classify(
  req: ClassifiableRequest,
  config: ClassifierConfig
): Promise<ClassificationResult> {
  const allSignals: string[] = [];

  for (const stageName of stagesFor(config)) {
    let result: StageResult | Promise<StageResult>;

    switch (stageName) {
      case "override":
        result = overrideClassify(req);
        break;
      case "structural":
        result = structuralClassify(req);
        break;
      case "heuristic":
        result = heuristicClassify(req);
        break;
      case "ml":
        result = mlClassify(req);
        break;
      case "local_llm":
        if (!config.local_llm.enabled) continue;
        result = llmClassify(req, config.local_llm, "local_llm");
        break;
      case "cloud_llm":
        if (!config.cloud_llm.enabled || !config.cloud_llm.api_key) continue;
        result = llmClassify(req, config.cloud_llm, "cloud_llm");
        break;
    }

    const resolved = await result;
    if (resolved.matched) {
      allSignals.push(...resolved.signals);
      if (
        resolved.confidence >= config.confidence_threshold ||
        config.mode === "local_llm" ||
        config.mode === "cloud_llm"
      ) {
        return {
          tier: resolved.tier,
          confidence: resolved.confidence,
          stage: stageName,
          signals: allSignals,
          overridden: stageName === "override",
        };
      }
    }
  }

  return {
    tier: DEFAULT_TIER,
    confidence: 0,
    stage: "default",
    signals: allSignals,
    overridden: false,
  };
}

export type { ClassificationResult, ClassifiableRequest } from "./types.ts";
