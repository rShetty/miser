export type ComplexityTier =
  | "trivial"
  | "simple"
  | "standard"
  | "hard"
  | "reasoning";

export type ClassifierStageName =
  | "override"
  | "structural"
  | "heuristic"
  | "ml"
  | "local_llm"
  | "cloud_llm";

export type ClassifierMode =
  | "heuristic"
  | "local_llm"
  | "cloud_llm"
  | "hybrid";

export interface LLMClassifierConfig {
  enabled: boolean;
  model: string;
  base_url: string;
  api_key?: string;
  timeout_ms: number;
}

export interface TierConfig {
  model: string;
  max_cost_per_1m?: {
    prompt?: number;
    completion?: number;
  };
  max_tokens?: number;
  temperature?: number;
}

export interface ProviderPreferences {
  sort?: "price" | "throughput" | "latency";
  allow_fallbacks?: boolean;
  order?: string[];
  only?: string[];
  ignore?: string[];
  max_price?: {
    prompt?: number;
    completion?: number;
  };
  quantizations?: string[];
}

export interface OpenRouterConfig {
  api_key: string;
  base_url?: string;
  provider_preferences?: ProviderPreferences;
}

export interface ClassifierConfig {
  mode: ClassifierMode;
  stages: ClassifierStageName[];
  confidence_threshold: number;
  ml_model?: string;
  local_llm: LLMClassifierConfig;
  cloud_llm: LLMClassifierConfig;
}

export interface CacheConfig {
  enabled: boolean;
  max_entries: number;
  similarity_threshold: number;
  embedding_model?: string;
}

export interface MiserConfig {
  server: {
    host: string;
    port: number;
    api_key?: string;
  };
  classifier: ClassifierConfig;
  tiers: Record<ComplexityTier, TierConfig>;
  openrouter: OpenRouterConfig;
  cache: CacheConfig;
  quality_check?: {
    enabled: boolean;
    retry_with_higher_tier: boolean;
  };
}

export const DEFAULT_CONFIG: MiserConfig = {
  server: {
    host: "127.0.0.1",
    port: 8787,
  },
  classifier: {
    mode: "hybrid",
    stages: ["override", "structural", "heuristic", "ml", "local_llm", "cloud_llm"],
    confidence_threshold: 0.55,
    ml_model: "Xenova/all-MiniLM-L6-v2",
    local_llm: {
      enabled: false,
      model: "qwen3:4b",
      base_url: "http://127.0.0.1:11434/v1",
      timeout_ms: 30000,
    },
    cloud_llm: {
      enabled: false,
      model: "openai/gpt-4.1-mini",
      base_url: "https://openrouter.ai/api/v1",
      api_key: "",
      timeout_ms: 30000,
    },
  },
  cache: {
    enabled: true,
    max_entries: 10000,
    similarity_threshold: 0.92,
    embedding_model: "Xenova/all-MiniLM-L6-v2",
  },
  tiers: {
    trivial: {
      model: "meta-llama/llama-3.2-3b-instruct:free",
      max_tokens: 2048,
      temperature: 0,
    },
    simple: {
      model: "deepseek/deepseek-chat",
      max_tokens: 4096,
      temperature: 0,
    },
    standard: {
      model: "anthropic/claude-sonnet-4",
      max_tokens: 8192,
      temperature: 0,
    },
    hard: {
      model: "anthropic/claude-opus-4",
      max_tokens: 16384,
      temperature: 0,
    },
    reasoning: {
      model: "openai/o4-mini",
      max_tokens: 16384,
      temperature: 0,
    },
  },
  openrouter: {
    api_key: "",
    base_url: "https://openrouter.ai/api/v1",
    provider_preferences: {
      sort: "price",
      allow_fallbacks: true,
    },
  },
  quality_check: {
    enabled: true,
    retry_with_higher_tier: true,
  },
};
