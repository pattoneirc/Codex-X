import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ProviderHeadersEditor } from "./ProviderHeadersEditor";
import { normalizeProviderHeaders, validateProviderHeaders } from "../providerHeaders";
import type { ProviderHeader } from "../providerHeaders";
import { createProviderHeadersControl } from "../providerHeadersControl";
import type { ProviderHeadersControlState } from "../providerHeadersControl";

export function ProviderHeadersControl({ lang, configText, disabled = false, onConfigChange, onBusyChange, onValidityChange }: {
  lang: "zh" | "en";
  configText: string;
  disabled?: boolean;
  onConfigChange: (configText: string) => void;
  onBusyChange: (busy: boolean) => void;
  onValidityChange: (valid: boolean) => void;
}) {
  const runtime = useRef({ configText, onConfigChange, onBusyChange, onValidityChange });
  runtime.current = { configText, onConfigChange, onBusyChange, onValidityChange };
  const control = useRef<ReturnType<typeof createProviderHeadersControl> | null>(null);
  const [rows, setRows] = useState<ProviderHeader[]>([]);
  const [state, setState] = useState<ProviderHeadersControlState>({ busy: true, reading: true, valid: false, error: null });

  useEffect(() => {
    const instance = createProviderHeadersControl({
      read: (text) => invoke<ProviderHeader[]>("read_provider_headers", { configText: text }),
      update: (text, headers) => invoke<string>("update_provider_headers", { configText: text, headers }),
      getConfigText: () => runtime.current.configText,
      isValid: (next) => validateProviderHeaders(next).valid,
      normalize: normalizeProviderHeaders,
      onRowsChange: setRows,
      onConfigChange: (text) => runtime.current.onConfigChange(text),
      onStateChange: (next) => {
        setState(next);
        runtime.current.onBusyChange(next.busy);
        runtime.current.onValidityChange(next.valid);
      },
    });
    control.current = instance;
    void instance.load(runtime.current.configText);
    return () => {
      instance.dispose();
      if (control.current === instance) control.current = null;
      runtime.current.onBusyChange(false);
      runtime.current.onValidityChange(true);
    };
  }, []);

  useEffect(() => { void control.current?.load(configText); }, [configText]);

  const errorText = state.error === "read"
    ? lang === "zh" ? "无法读取 Headers，请检查供应商 TOML 后重试。" : "Could not read headers. Check the provider TOML and retry."
    : lang === "zh" ? "无法更新 Headers，请重试或检查供应商 TOML。" : "Could not update headers. Retry or check the provider TOML.";

  return <div className="cx-provider-headers-control">
    <ProviderHeadersEditor lang={lang} rows={rows} disabled={disabled || state.reading} onChange={(next) => { void control.current?.changeRows(next); }} />
    {state.busy && <p className="cx-provider-headers-hint" role="status">{state.reading
      ? lang === "zh" ? "正在读取 Headers…" : "Reading headers…"
      : lang === "zh" ? "正在更新配置…" : "Updating configuration…"}</p>}
    {state.error && <div className="cx-provider-headers-feedback"><p className="cx-provider-headers-error" role="status">{errorText}</p><button type="button" className="cx-providers-button cx-providers-button--secondary cx-providers-button--small" disabled={disabled || state.busy} onClick={() => { void control.current?.retry(); }}>{lang === "zh" ? "重试" : "Retry"}</button></div>}
  </div>;
}
