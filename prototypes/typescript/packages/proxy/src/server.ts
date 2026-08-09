import { Hono } from "hono";
import { RoutingEngine } from "@miser/router";
import type { MiserConfig } from "@miser/config";

export function createServer(config: MiserConfig): Hono {
  const app = new Hono();
  const engine = new RoutingEngine(config);

  app.use("*", async (c, next) => {
    c.header("X-Miser-Version", "0.1.0");
    await next();
  });

  if (config.server.api_key) {
    app.use("/v1/*", async (c, next) => {
      const auth = c.req.header("Authorization");
      if (!auth || auth !== `Bearer ${config.server.api_key}`) {
        return c.json({ error: "Unauthorized" }, 401);
      }
      await next();
    });
  }

  app.get("/v1/models", async (c) => {
    const models = await engine.listModels();
    return c.json(models);
  });

  app.post("/v1/chat/completions", async (c) => {
    let body: Record<string, unknown>;
    try {
      body = await c.req.json();
    } catch {
      return c.json({ error: "Invalid JSON body" }, 400);
    }

    const messages = body.messages as unknown[];
    if (!messages || !Array.isArray(messages)) {
      return c.json({ error: "messages field required" }, 400);
    }

    const isStreaming = body.stream === true;

    const route = await engine.route({
      messages: messages as ClassifiableRequest["messages"],
      tools: body.tools as unknown[] | undefined,
      tool_choice: body.tool_choice,
      response_format: body.response_format as { type?: string } | undefined,
      max_tokens: body.max_tokens as number | undefined,
      temperature: body.temperature as number | undefined,
      model: body.model as string | undefined,
      stream: isStreaming,
    });

    const result = await route.forward();
    const upstream = result.response;

    c.header("X-Miser-Tier", route.tier);
    c.header("X-Miser-Model", route.model);
    c.header("X-Miser-Stage", route.classification.stage);
    c.header("X-Miser-Confidence", route.classification.confidence.toFixed(3));
    c.header("X-Miser-Signals", route.classification.signals.join("; "));

    if (isStreaming && upstream.body) {
      return new Response(upstream.body, {
        status: upstream.status,
        headers: {
          "Content-Type": "text/event-stream",
          "Cache-Control": "no-cache",
          "Connection": "keep-alive",
          "X-Miser-Tier": route.tier,
          "X-Miser-Model": route.model,
        },
      });
    }

    const data = await upstream.json();
    if (!upstream.ok) {
      return c.json(data, upstream.status as 400 | 401 | 403 | 404 | 429 | 500);
    }

    return c.json(data);
  });

  app.get("/health", (c) => {
    return c.json({ status: "ok", version: "0.1.0" });
  });

  app.get("/stats", (c) => {
    const cacheStats = engine.getCache().stats();
    return c.json({
      cache: cacheStats,
      tiers: engine.getTierOrder(),
    });
  });

  return app;
}

import type { ClassifiableRequest } from "@miser/classifier";
