import { DEFAULT_CONFIG, type MiserConfig } from "@miser/config";

export async function loadConfig(configPath?: string): Promise<MiserConfig> {
  const path = configPath ?? "./miser.yaml";
  let fileConfig: Record<string, unknown> = {};

  try {
    fileConfig = Bun.YAML.parse(await Bun.file(path).text()) as Record<string, unknown>;
  } catch {
    try {
      const jsonPath = path.replace(/\.ya?ml$/, ".json");
      fileConfig = JSON.parse(await Bun.file(jsonPath).text()) as Record<string, unknown>;
    } catch {
      fileConfig = {};
    }
  }

  const merged = deepMerge(
    DEFAULT_CONFIG as unknown as Record<string, unknown>,
    fileConfig
  ) as unknown as MiserConfig;

  merged.openrouter.api_key = resolveEnv(merged.openrouter.api_key) ?? "";
  merged.classifier.local_llm.api_key = resolveEnv(merged.classifier.local_llm.api_key);
  merged.classifier.cloud_llm.api_key = resolveEnv(merged.classifier.cloud_llm.api_key);

  if (!merged.openrouter.api_key) merged.openrouter.api_key = process.env.OPENROUTER_API_KEY ?? "";
  if (!merged.classifier.cloud_llm.api_key) {
    merged.classifier.cloud_llm.api_key = process.env.OPENROUTER_API_KEY ?? "";
  }
  if (!merged.server.api_key) merged.server.api_key = process.env.MISER_API_KEY;

  return merged;
}

function resolveEnv(value?: string): string | undefined {
  if (!value) return value;
  const match = value.match(/^\{env:([A-Z0-9_]+)\}$/i);
  return match ? process.env[match[1]] ?? "" : value;
}

function deepMerge(
  defaults: Record<string, unknown>,
  override: Record<string, unknown>
): Record<string, unknown> {
  const result: Record<string, unknown> = { ...defaults };
  for (const [key, overrideValue] of Object.entries(override)) {
    const defaultValue = defaults[key];
    if (
      overrideValue &&
      typeof overrideValue === "object" &&
      !Array.isArray(overrideValue) &&
      defaultValue &&
      typeof defaultValue === "object" &&
      !Array.isArray(defaultValue)
    ) {
      result[key] = deepMerge(
        defaultValue as Record<string, unknown>,
        overrideValue as Record<string, unknown>
      );
    } else {
      result[key] = overrideValue;
    }
  }
  return result;
}
