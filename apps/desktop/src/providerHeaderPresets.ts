import type { ProviderHeader } from "./providerHeaders";

export const PROVIDER_HEADER_PRESETS = [
  {
    id: "claude_code",
    label: { zh: "Claude Code", en: "Claude Code" },
    name: "User-Agent",
    value: "claude-cli/2.1.161 (external, cli)",
  },
  {
    id: "kilo_code",
    label: { zh: "Kilo Code", en: "Kilo Code" },
    name: "User-Agent",
    value: "Kilo-Code/1.0",
  },
] as const;

export type ProviderHeaderPresetId = typeof PROVIDER_HEADER_PRESETS[number]["id"];

/** Unknown IDs are a no-op. A preset changes one UA row and preserves drafts. */
export function applyProviderHeaderPreset(rows: readonly ProviderHeader[], id: string): ProviderHeader[] {
  const preset = PROVIDER_HEADER_PRESETS.find((entry) => entry.id === id);
  if (!preset) return [...rows];

  let index = rows.findIndex((row) => row.name.trim().toLowerCase() === "user-agent");
  if (index < 0) index = rows.findIndex((row) => !row.name.trim() && row.value === "");
  const header: ProviderHeader = { name: preset.name, value: preset.value, source: "static" };
  if (index < 0) return [...rows, header];
  return rows.map((row, position) => position === index ? { ...row, ...header } : row);
}

/** Literal examples describe the named header's purpose; environment rows use their own hint. */
export function providerHeaderValuePlaceholder(name: string, lang: "zh" | "en"): string {
  switch (name.trim().toLowerCase()) {
    case "user-agent":
      return lang === "zh" ? "例如 claude-cli/2.1.161 (external, cli)" : "e.g. claude-cli/2.1.161 (external, cli)";
    case "http-referer":
      return lang === "zh" ? "例如 https://your-app.example" : "e.g. https://your-app.example";
    case "x-title":
    case "x-openrouter-title":
      return lang === "zh" ? "例如 你的应用名称" : "e.g. Your app name";
    case "authorization":
      return lang === "zh" ? "例如 Bearer <访问令牌>" : "e.g. Bearer <access token>";
    case "x-api-key":
    case "x-goog-api-key":
      return lang === "zh" ? "例如 供应商提供的 API Key" : "e.g. API key from your provider";
    default:
      return lang === "zh" ? "填写该请求头的值" : "Enter this header's value";
  }
}
