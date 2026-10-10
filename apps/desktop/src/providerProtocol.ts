export type UpstreamApi = "responses" | "chat_completions" | "anthropic_messages" | "gemini";

export function providerUpstreamApi(provider: { upstreamApi?: string | null; wireApi?: string }): string {
  const configured = provider.upstreamApi?.trim().toLowerCase();
  if (configured === "chat_completions" || configured === "openai_chat") return "chat_completions";
  if (configured === "anthropic_messages" || configured === "anthropic") return "anthropic_messages";
  if (configured === "gemini" || configured === "gemini_native") return "gemini";
  if (configured === "responses") return "responses";
  if (configured) return configured;
  const legacy = provider.wireApi?.trim();
  if (legacy === "chat" || legacy === "chat_completions") return "chat_completions";
  return !legacy || legacy === "official" ? "responses" : legacy;
}

export function providerRequiresConversion(provider: { upstreamApi?: string | null; wireApi?: string }): boolean {
  return providerUpstreamApi(provider) !== "responses";
}
