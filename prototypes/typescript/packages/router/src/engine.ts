import type { MiserConfig, ComplexityTier, TierConfig } from "@miser/config";
import { classify } from "@miser/classifier";
import type { ClassifiableRequest, ClassificationResult } from "@miser/classifier";
import { SemanticCache } from "./cache.ts";
import { forwardToOpenRouter, listModels, type ForwardResult } from "./provider.ts";

const TIER_ORDER: ComplexityTier[] = ["trivial", "simple", "standard", "hard", "reasoning"];

export interface RoutingDecision {
  classification: ClassificationResult;
  tier: ComplexityTier;
  model: string;
  tierConfig: TierConfig;
  cached: boolean;
  cachedResponse?: string;
}

export interface RouteResult extends RoutingDecision {
  forward: () => Promise<ForwardResult>;
}

export class RoutingEngine {
  private config: MiserConfig;
  private cache: SemanticCache;

  constructor(config: MiserConfig) {
    this.config = config;
    this.cache = new SemanticCache(config.cache);
  }

  async route(req: ClassifiableRequest): Promise<RouteResult> {
    const classification = await classify(req, this.config.classifier);

    const tier = classification.tier;
    const tierConfig = this.config.tiers[tier];
    const model = tierConfig.model;

    const decision: RoutingDecision = {
      classification,
      tier,
      model,
      tierConfig,
      cached: false,
    };

    return {
      ...decision,
      forward: async () => {
        const forwardBody = {
          model,
          messages: req.messages,
          tools: req.tools as unknown[] | undefined,
          tool_choice: req.tool_choice,
          response_format: req.response_format,
          max_tokens: tierConfig.max_tokens ?? req.max_tokens,
          temperature: tierConfig.temperature ?? req.temperature,
          stream: req.stream,
          provider: this.config.openrouter.provider_preferences,
        };
        return forwardToOpenRouter(forwardBody, this.config);
      },
    };
  }

  getCache(): SemanticCache {
    return this.cache;
  }

  getTierOrder(): ComplexityTier[] {
    return TIER_ORDER;
  }

  getConfig(): MiserConfig {
    return this.config;
  }

  async listModels(): Promise<unknown> {
    return listModels(this.config);
  }
}
