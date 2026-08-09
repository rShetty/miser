#!/usr/bin/env bun
import { createServer } from "./server.ts";
import { loadConfig } from "./config-loader.ts";

const config = await loadConfig(process.argv[2]);

if (!config.openrouter.api_key) {
  console.error("ERROR: OpenRouter API key not set.");
  console.error("Set OPENROUTER_API_KEY env var or configure openrouter.api_key in miser.yaml");
  process.exit(1);
}

const app = createServer(config);
const port = config.server.port;
const host = config.server.host;

console.log(`
  ┌─────────────────────────────────────────────┐
  │             Miser Router v0.1.0              │
  │   Intelligent LLM routing via OpenRouter     │
  └─────────────────────────────────────────────┘

  Endpoint:  http://${host}:${port}/v1/chat/completions
  Models:    http://${host}:${port}/v1/models
  Health:    http://${host}:${port}/health
  Stats:     http://${host}:${port}/stats

  Tiers:
${Object.entries(config.tiers)
    .map(([tier, cfg]) => `    ${tier.padEnd(12)} -> ${cfg.model}`)
    .join("\n")}

  Classifier stages: ${config.classifier.stages.join(" -> ")}
  Cache: ${config.cache.enabled ? "enabled" : "disabled"}
`);

export default {
  port,
  hostname: host,
  fetch: app.fetch,
};
