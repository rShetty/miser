import type { ComplexityTier } from "@miser/config";

export interface ClassificationResult {
  tier: ComplexityTier;
  confidence: number;
  stage: string;
  signals: string[];
  overridden: boolean;
}

export interface ClassifiableRequest {
  messages: Array<{
    role: string;
    content: string | Array<Record<string, unknown>>;
  }>;
  tools?: unknown[];
  tool_choice?: unknown;
  response_format?: { type?: string };
  max_tokens?: number;
  temperature?: number;
  model?: string;
  stream?: boolean;
}

export type StageResult =
  | { matched: true; tier: ComplexityTier; confidence: number; signals: string[] }
  | { matched: false };
