import { useId } from "react";
import { Plus, Trash2 } from "lucide-react";
import {
  createProviderHeader,
  removeProviderHeader,
  updateProviderHeader,
  validateProviderHeaders,
} from "../providerHeaders";
import type { ProviderHeader, ProviderHeaderSource } from "../providerHeaders";
import { applyProviderHeaderPreset, PROVIDER_HEADER_PRESETS, providerHeaderValuePlaceholder } from "../providerHeaderPresets";
import "../styles/provider-headers.css";

export function ProviderHeadersEditor({ lang, rows, disabled = false, onChange }: {
  lang: "zh" | "en";
  rows: readonly ProviderHeader[];
  disabled?: boolean;
  onChange: (rows: ProviderHeader[]) => void;
}) {
  const id = useId();
  const copy = lang === "zh" ? {
    title: "自定义请求头 Headers", optional: "可选", hint: "按供应商要求设置 User-Agent（客户端标识）或其他附加请求头；不填写时使用默认设置。",
    presets: "User-Agent 预设", presetHint: "预设只设置 User-Agent，其他请求头保留；已有 User-Agent 会更新为所选固定值。预设是否适用以供应商要求为准。",
    envHint: "环境变量模式只保存变量名；直连由 Codex 读取，路由由 Codex-X 读取，请在启动对应应用前设置。",
    add: "添加 Header", empty: "尚未配置自定义 Headers", name: "名称", value: "值", source: "值来源", literal: "固定值", env: "环境变量", remove: "删除 Header",
    placeholder: "例如 User-Agent", envPlaceholder: "例如 PROVIDER_HEADER_VALUE", fix: "请先修正标红的 Headers，再保存供应商。",
    row: (index: number) => `第 ${index + 1} 行`,
  } : {
    title: "Custom headers", optional: "Optional", hint: "Set User-Agent (client identification) or other HTTP headers required by your provider. Leave them empty to keep the defaults.",
    presets: "User-Agent presets", presetHint: "Presets set only User-Agent and keep other headers. An existing User-Agent becomes the selected literal value. Check your provider requirements before choosing one.",
    envHint: "Environment mode saves only the variable name. Set it before launching Codex for direct requests, or Codex-X for routed requests.",
    add: "Add header", empty: "No custom headers configured", name: "Name", value: "Value", source: "Value source", literal: "Literal value", env: "Environment variable", remove: "Remove header",
    placeholder: "e.g. User-Agent", envPlaceholder: "e.g. PROVIDER_HEADER_VALUE", fix: "Correct the highlighted headers before saving this provider.",
    row: (index: number) => `Row ${index + 1}`,
  };
  const validation = validateProviderHeaders(rows, lang);
  const update = (index: number, patch: Partial<ProviderHeader>) => {
    if (!disabled) onChange(updateProviderHeader(rows, index, patch));
  };

  return <section className="cx-provider-headers" aria-labelledby={`${id}-title`}>
    <div className="cx-provider-headers-heading">
      <div><strong id={`${id}-title`}>{copy.title}<span>{copy.optional}</span></strong><p>{copy.hint}</p></div>
      <div className="cx-provider-headers-actions">
        <select className="cx-provider-headers-presets" aria-label={copy.presets} aria-describedby={`${id}-preset-hint`} value="" disabled={disabled} onChange={(event) => {
          const selected = event.currentTarget.value;
          if (selected) onChange(applyProviderHeaderPreset(rows, selected));
        }}>
          <option value="">{copy.presets}</option>
          {PROVIDER_HEADER_PRESETS.map((preset) => <option key={preset.id} value={preset.id}>{preset.label[lang]} · {preset.value}</option>)}
        </select>
        <button type="button" className="cx-providers-button cx-providers-button--secondary cx-providers-button--small" disabled={disabled} onClick={() => onChange([...rows, createProviderHeader()])}><Plus size={14} aria-hidden="true" />{copy.add}</button>
      </div>
    </div>
    <p className="cx-provider-headers-hint cx-provider-headers-preset-hint" id={`${id}-preset-hint`}>{copy.presetHint}</p>
    {rows.length === 0 ? <p className="cx-provider-headers-empty">{copy.empty}</p> : <div className="cx-provider-headers-rows">
      {rows.map((row, index) => {
        const error = validation.errors[index];
        return <div key={index} className="cx-provider-header-row" role="group" aria-label={copy.row(index)}>
          <label className="cx-provider-header-field"><span>{copy.name}</span><input value={row.name} placeholder={copy.placeholder} aria-label={`${copy.name} · ${copy.row(index)}`} aria-invalid={Boolean(error.name)} aria-describedby={error.name ? `${id}-${index}-name-error` : undefined} onChange={(event) => update(index, { name: event.currentTarget.value })} disabled={disabled} autoComplete="off" autoCapitalize="none" spellCheck={false} />{error.name && <small id={`${id}-${index}-name-error`}>{error.name}</small>}</label>
          <label className="cx-provider-header-field"><span>{copy.source}</span><select value={row.source} aria-label={`${copy.source} · ${copy.row(index)}`} aria-invalid={Boolean(error.source)} aria-describedby={error.source ? `${id}-${index}-source-error` : undefined} onChange={(event) => update(index, { source: event.currentTarget.value as ProviderHeaderSource })} disabled={disabled}><option value="static">{copy.literal}</option><option value="env">{copy.env}</option></select>{error.source && <small id={`${id}-${index}-source-error`}>{error.source}</small>}</label>
          <label className="cx-provider-header-field"><span>{row.source === "env" ? copy.env : copy.value}</span><input value={row.value} placeholder={row.source === "env" ? copy.envPlaceholder : providerHeaderValuePlaceholder(row.name, lang)} aria-label={`${row.source === "env" ? copy.env : copy.value} · ${copy.row(index)}`} aria-invalid={Boolean(error.value)} aria-describedby={error.value ? `${id}-${index}-value-error` : row.source === "env" ? `${id}-env-hint` : undefined} onChange={(event) => update(index, { value: event.currentTarget.value })} disabled={disabled} autoComplete="off" autoCapitalize="none" spellCheck={false} />{error.value && <small id={`${id}-${index}-value-error`}>{error.value}</small>}</label>
          <button type="button" className="cx-providers-icon-button cx-providers-icon-button--danger cx-provider-header-remove" title={`${copy.remove} · ${copy.row(index)}`} aria-label={`${copy.remove} · ${copy.row(index)}`} disabled={disabled} onClick={() => onChange(removeProviderHeader(rows, index))}><Trash2 size={14} aria-hidden="true" /></button>
        </div>;
      })}
    </div>}
    <p className="cx-provider-headers-hint" id={`${id}-env-hint`}>{copy.envHint}</p>
    {rows.some((row) => row.name.trim().toLowerCase() === "authorization") && <p className="cx-provider-headers-hint">{lang === "zh" ? "直连使用 Authorization Header 认证时，请留空 API Key；填写 API Key 会替换原有 Authorization。" : "For direct Authorization header authentication, leave API Key empty. An explicit API Key replaces the existing Authorization header."}</p>}
    {!validation.valid && <p className="cx-provider-headers-error" role="status">{copy.fix}</p>}
  </section>;
}
