import type { MiserConfig, ProviderPreferences } from "@miser/config";

export interface ForwardRequest {
  model: string;
  messages: unknown[];
  tools?: unknown[];
  tool_choice?: unknown;
  response_format?: unknown;
  max_tokens?: number;
  temperature?: number;
  stream?: boolean;
  provider?: ProviderPreferences;
}

export interface ForwardResult {
  response: Response;
  modelUsed: string;
}

export async function forwardToOpenRouter(
  body: ForwardRequest,
  config: MiserConfig
): Promise<ForwardResult> {
  const baseUrl = config.openrouter.base_url ?? "https://openrouter.ai/api/v1";
  const providerPrefs = config.openrouter.provider_preferences;

  const payload: Record<string, unknown> = {
    model: body.model,
    messages: body.messages,
    stream: body.stream ?? false,
  };

  if (body.tools) payload.tools = body.tools;
  if (body.tool_choice) payload.tool_choice = body.tool_choice;
  if (body.response_format) payload.response_format = body.response_format;
  if (body.max_tokens) payload.max_tokens = body.max_tokens;
  if (body.temperature !== undefined) payload.temperature = body.temperature;

  const provider: ProviderPreferences = {
    ...providerPrefs,
    ...body.provider,
  };
  if (Object.keys(provider).length > 0) {
    payload.provider = provider;
  }

  const response = await fetch(`${baseUrl}/chat/completions`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${config.openrouter.api_key}`,
      "HTTP-Referer": "https://github.com/rshetty/miser",
      "X-Title": "Miser Router",
    },
    body: JSON.stringify(payload),
  });

  return { response, modelUsed: body.model };
}

export async function listModels(config: MiserConfig): Promise<unknown> {
  const baseUrl = config.openrouter.base_url ?? "https://openrouter.ai/api/v1";
  const response = await fetch(`${baseUrl}/models`, {
    headers: {
      Authorization: `Bearer ${config.openrouter.api_key}`,
    },
  });
  return response.json();
}
