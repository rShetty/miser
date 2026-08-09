import type { CacheConfig } from "@miser/config";

export interface CacheEntry {
  promptHash: string;
  embedding: number[];
  response: string;
  model: string;
  tier: string;
  timestamp: number;
  tokensSaved: number;
}

export class SemanticCache {
  private entries: CacheEntry[] = [];
  private config: CacheConfig;

  constructor(config: CacheConfig) {
    this.config = config;
  }

  private hash(text: string): string {
    let h = 0;
    for (let i = 0; i < text.length; i++) {
      h = ((h << 5) - h + text.charCodeAt(i)) | 0;
    }
    return h.toString(36);
  }

  private cosineSim(a: number[], b: number[]): number {
    let dot = 0;
    for (let i = 0; i < a.length; i++) dot += a[i] * b[i];
    return dot;
  }

  lookup(prompt: string, embedding: number[]): CacheEntry | null {
    if (!this.config.enabled) return null;
    const hash = this.hash(prompt);
    const exact = this.entries.find((e) => e.promptHash === hash);
    if (exact) return exact;

    for (const entry of this.entries) {
      const sim = this.cosineSim(embedding, entry.embedding);
      if (sim >= this.config.similarity_threshold) return entry;
    }
    return null;
  }

  store(prompt: string, embedding: number[], response: string, model: string, tier: string, tokensSaved: number): void {
    if (!this.config.enabled) return;
    if (this.entries.length >= this.config.max_entries) {
      this.entries.shift();
    }
    this.entries.push({
      promptHash: this.hash(prompt),
      embedding,
      response,
      model,
      tier,
      timestamp: Date.now(),
      tokensSaved,
    });
  }

  stats(): { entries: number; hitRate: number; tokensSaved: number } {
    return {
      entries: this.entries.length,
      hitRate: 0,
      tokensSaved: this.entries.reduce((s, e) => s + e.tokensSaved, 0),
    };
  }

  clear(): void {
    this.entries = [];
  }
}
