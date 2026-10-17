import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Languages, Loader2 } from "lucide-react";
import type { CodexLanguageStatus } from "../codexLanguageTypes";
import { Toggle } from "./ui";
import { cachedCodexLanguageStatus, rememberCodexLanguageStatus } from "../codexLanguageState";
import "../styles/codex-language.css";

export function CodexLanguageControl({ lang, configDir, active = true, disabled = false, onBusyChange, onPreferencesSaved }: {
  lang: "zh" | "en";
  configDir: string;
  active?: boolean;
  disabled?: boolean;
  onBusyChange?: (busy: boolean) => void;
  onPreferencesSaved?: (status: CodexLanguageStatus) => void;
}) {
  const text = (zh: string, en: string) => lang === "zh" ? zh : en;
  const [detected, setDetected] = useState(() => ({ configDir, value: cachedCodexLanguageStatus(configDir) }));
  const status = detected.configDir === configDir ? detected.value : cachedCodexLanguageStatus(configDir);
  const setStatus = (value: CodexLanguageStatus) => {
    rememberCodexLanguageStatus(configDir, value);
    setDetected({ configDir, value });
  };
  const [busy, setBusy] = useState(false);
  const [readError, setReadError] = useState("");
  const [actionError, setActionError] = useState("");
  const parentBusy = useRef(disabled);
  parentBusy.current = disabled;
  const generation = useRef(0);
  const operationInFlight = useRef(false);
  const statusRequest = useRef(0);

  useEffect(() => {
    generation.current += 1;
    setDetected({ configDir, value: cachedCodexLanguageStatus(configDir) });
    setReadError(""); setActionError(""); setBusy(false);
  }, [configDir]);
  useEffect(() => () => { generation.current += 1; }, []);

  useEffect(() => {
    const current = generation.current;
    let disposed = false;
    let loading = false;
    const load = async () => {
      if (loading || disposed || !active || operationInFlight.current || parentBusy.current) return;
      loading = true;
      const request = ++statusRequest.current;
      try {
        const next = await invoke<CodexLanguageStatus>("get_codex_language_status", { configDir });
        if (!disposed && generation.current === current && request === statusRequest.current && !operationInFlight.current && !parentBusy.current) {
          setStatus(next); setReadError("");
        }
      } catch (cause) {
        if (!disposed && generation.current === current && request === statusRequest.current && !operationInFlight.current && !parentBusy.current) setReadError(String(cause));
      } finally { loading = false; }
    };
    if (active) void load();
    const interval = active ? window.setInterval(() => { void load(); }, 1800) : undefined;
    return () => { disposed = true; if (interval !== undefined) window.clearInterval(interval); };
  }, [configDir, active]);

  const save = async (enabled: boolean) => {
    if (busy || disabled || operationInFlight.current) return;
    operationInFlight.current = true;
    statusRequest.current += 1;
    onBusyChange?.(true);
    const current = generation.current;
    setBusy(true); setActionError("");
    try {
      const next = await invoke<CodexLanguageStatus>("save_codex_language_preferences", { configDir, locale: enabled ? "zh-CN" : "auto", enabled });
      if (current === generation.current) { setStatus(next); setReadError(""); onPreferencesSaved?.(next); }
    } catch (cause) {
      if (current === generation.current) setActionError(String(cause));
    } finally {
      operationInFlight.current = false;
      onBusyChange?.(false);
      if (current === generation.current) setBusy(false);
    }
  };

  const pending = status?.pendingEnabled != null;
  const enabled = status?.pendingEnabled ?? status?.enabled ?? false;
  const starting = status?.phase === "starting";
  const blocked = busy || disabled || starting;
  const error = actionError || readError || (status?.phase === "failed" ? status.message : "");
  const description = error || (status && !status.supported ? status.message : text(
    "为 Codex 应用简体中文，重启生效。自动检测当前配置。",
    "Use Simplified Chinese in Codex after restart. Detects current settings.",
  ));
  const stateLabel = busy ? text("处理中", "Saving")
    : readError ? text("检测失败", "Unavailable")
    : actionError ? text("操作失败", "Failed")
    : !status ? text("检测中", "Checking")
    : status.phase === "failed" ? text("应用失败", "Failed")
    : starting ? text("应用中", "Applying")
    : pending ? text("待重启", "Restart needed")
    : enabled ? text("已开启", "On") : text("已关闭", "Off");

  return <section className="cx-page-setting-row cx-codex-language" aria-labelledby="codex-language-title" aria-busy={busy || starting}>
      <div className="cx-page-setting-icon" aria-hidden="true"><Languages size={18} strokeWidth={1.9} /></div>
      <div className="cx-page-setting-copy">
        <strong id="codex-language-title">{text("Codex 简体中文", "Codex Simplified Chinese")}</strong>
        <p id="codex-language-description" title={description} role={error ? "alert" : undefined} className={error ? "cx-codex-language-error" : undefined}>{description}</p>
      </div>
      <div className="cx-page-setting-action cx-codex-language-action">
        <div className="cx-codex-language-switch">
          <span className={enabled && !pending && !error ? "cx-codex-language-enabled" : undefined} aria-live="polite">{stateLabel}</span>
          {status ? <Toggle checked={enabled} disabled={blocked || !status.supported || Boolean(readError)} aria-labelledby="codex-language-title" aria-describedby="codex-language-description" onCheckedChange={next => void save(next)} />
            : <span className="cx-codex-language-loading" aria-hidden="true"><Loader2 size={15} className="cx-page-spin" /></span>}
        </div>
      </div>
    </section>;
}
