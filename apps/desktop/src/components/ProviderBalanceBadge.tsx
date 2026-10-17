import { AlertCircle, RefreshCw } from "lucide-react";
import type { ProviderBalance } from "../providerBalance";
import "../styles/provider-balance.css";

function errorLabel(code: string | null, zh: boolean) {
  if (code === "permission_denied") return zh ? "查询权限不足" : "Access denied";
  if (code === "invalid_credential" || code === "authentication_failed") return zh ? "请检查 API Key" : "Check API key";
  if (code === "missing_key") return zh ? "未设置 API Key" : "API key missing";
  if (code === "rate_limited") return zh ? "请求较多，请稍后刷新" : "Rate limited. Try again shortly.";
  if (code === "timeout" || code === "network_error") return zh ? "网络异常，请稍后刷新" : "Check your connection and retry.";
  return zh ? "暂时无法查询" : "Temporarily unavailable";
}

function amountText(amount: string) {
  const match = /^(-?)(\d+)(?:\.(\d+))?$/.exec(amount);
  if (!match) return amount;
  const integer = match[2].replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const fraction = (match[3] || "").replace(/0+$/, "").padEnd(2, "0");
  // Group the exact decimal string; financial values never pass through Number.
  return `${match[1] === "-" ? "−" : ""}${integer}.${fraction}`;
}

export function ProviderBalanceBadge({ balance, loading, refreshing = false, failed, lang, onRefresh }: {
  balance?: ProviderBalance;
  loading: boolean;
  refreshing?: boolean;
  failed: boolean;
  lang: "zh" | "en";
  onRefresh?: () => void;
}) {
  const zh = lang === "zh";
  const hasBalance = Boolean(balance?.balances.length);
  const unavailable = failed || Boolean(balance?.errorCode);
  const busy = loading || refreshing;
  const label = balance?.balanceKind === "credit_limit" ? (zh ? "Key 剩余额度" : "Key credit left") : (zh ? "可用余额" : "Available balance");
  const updated = balance?.checkedAt ? new Date(balance.checkedAt) : null;
  const updateTime = updated && Number.isFinite(updated.getTime())
    ? updated.toLocaleTimeString(zh ? "zh-CN" : "en-GB", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" }) : null;
  const detail = errorLabel(balance?.errorCode ?? null, zh);
  const hint = [balance?.balanceKind === "credit_limit" ? (zh ? "此 API Key 的剩余额度。" : "Remaining credit for this API key.") : "",
    unavailable ? detail : "", updateTime ? `${hasBalance ? (zh ? "余额更新于" : "Balance updated") : (zh ? "最近查询于" : "Last checked")} ${updated!.toLocaleString(zh ? "zh-CN" : "en-GB")}` : "",
    zh ? "后台每 5 分钟自动刷新，也可点击余额旁的按钮立即查询。" : "Refreshes every 5 minutes in the background. Use the refresh button to query now."].filter(Boolean).join("\n");
  return <section className={`cx-provider-balance${unavailable ? " cx-provider-balance--stale" : ""}`} aria-label={label} aria-busy={busy} title={hint}>
    <div className="cx-provider-balance-heading">
      <span>{label}</span>

    </div>
    <div className="cx-provider-balance-reading">
    {hasBalance ? <div className={`cx-provider-balance-amounts${balance!.balances.length > 1 ? " cx-provider-balance-amounts--multiple" : ""}`}>
      {balance!.balances.map(item => <div key={item.currency} className={`cx-provider-balance-value${item.amount.startsWith("-") || /^0(?:\.0+)?$/.test(item.amount) ? " cx-provider-balance-value--empty" : ""}`} title={`${item.amount} ${item.currency}`}>
        <strong>{amountText(item.amount)}</strong><span>{item.currency}</span>
      </div>)}
    </div> : busy ? <div className="cx-provider-balance-placeholder" aria-label={zh ? "正在查询余额" : "Loading balance"}><span /></div>
      : <div className="cx-provider-balance-unavailable"><AlertCircle size={14} aria-hidden="true" /><span>{detail}</span></div>}
      <button type="button" className="cx-provider-balance-refresh" disabled={busy || !onRefresh} onClick={onRefresh}
        title={zh ? "立即刷新余额" : "Refresh balance now"} aria-label={zh ? "刷新余额" : "Refresh balance"}>
        <RefreshCw size={13} strokeWidth={1.8} className={busy ? "cx-provider-balance-spin" : undefined} aria-hidden="true" />
      </button>
    </div>
    <div className={`cx-provider-balance-footer${unavailable && !busy ? " cx-provider-balance-footer--error" : ""}`} aria-live="polite">
      {busy ? (zh ? "正在刷新…" : "Refreshing…") : <>{unavailable ? (zh ? "刷新失败" : "Refresh failed") : !hasBalance ? (zh ? "尚未更新" : "Not updated") : (zh ? "已更新" : "Updated")}{updateTime && <><span aria-hidden="true"> · </span>{unavailable && hasBalance ? (zh ? "上次 " : "Last ") : ""}<time dateTime={balance!.checkedAt!}>{updateTime}</time></>}</>}
    </div>
  </section>;
}
