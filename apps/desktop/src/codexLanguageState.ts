import type { CodexLanguageStatus } from "./codexLanguageTypes";

// Keep the last detected setting across settings-page mounts. Never invent an
// initial "off" state while the native configuration is still being read.
const statuses = new Map<string, CodexLanguageStatus>();
export function cachedCodexLanguageStatus(configDir: string) { return statuses.get(configDir) ?? null; }
export function rememberCodexLanguageStatus(configDir: string, status: CodexLanguageStatus) {
  statuses.set(configDir, status);
}
