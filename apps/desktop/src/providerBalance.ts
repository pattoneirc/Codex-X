import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type ProviderBalance = {
  source: "local" | "detected";
  providerId: string;
  vendor: "deepseek" | "openrouter" | "siliconflow" | "moonshot" | null;
  supported: boolean;
  balanceKind: "balance" | "credit_limit" | null;
  balances: { currency: string; amount: string }[];
  checkedAt: string | null;
  errorCode: string | null;
};
export type ProviderBalanceState = {
  balances: ProviderBalance[];
  failed: boolean;
  loading: boolean;
  refreshingKeys: string[];
  refreshBalance: (source: "local" | "detected", providerId: string) => Promise<void>;
};

function keepLastBalance(previous: ProviderBalance | undefined, next: ProviderBalance): ProviderBalance {
  if (next.errorCode && previous?.balances.length) {
    return { ...next, balances: previous.balances, balanceKind: previous.balanceKind, checkedAt: previous.checkedAt };
  }
  if (!next.errorCode && previous?.checkedAt && next.checkedAt && previous.checkedAt > next.checkedAt) return previous;
  return next;
}
const REFRESH_INTERVAL = 5 * 60 * 1000;

// Lives in App, so navigating away from Providers does not stop refreshing.
export function useProviderBalances(configDir: string, revision: string, ready: boolean) {
  const [snapshot, setSnapshot] = useState({ configDir, revision, balances: [] as ProviderBalance[], failed: false, loading: true });
  const scope = JSON.stringify([configDir, revision]);
  const scopeRef = useRef(scope);
  scopeRef.current = scope;
  const manualInFlight = useRef(new Set<string>());
  const manualVersions = useRef(new Map<string, number>());
  const [manualBusy, setManualBusy] = useState<Record<string, boolean>>({});
  const refreshBalance = useCallback(async (source: "local" | "detected", providerId: string) => {
    if (!ready) return;
    const token = JSON.stringify([scope, source, providerId]);
    if (manualInFlight.current.has(token)) return;
    manualInFlight.current.add(token);
    manualVersions.current.set(token, (manualVersions.current.get(token) ?? 0) + 1);
    setManualBusy(previous => ({ ...previous, [token]: true }));
    try {
      const result = await invoke<ProviderBalance>("refresh_provider_balance", { configDir, source, providerId });
      if (scopeRef.current !== scope) return;
      setSnapshot(previous => {
        const matches = (item: ProviderBalance) => item.source === source && item.providerId === providerId;
        const updated = keepLastBalance(previous.balances.find(matches), result);
        return { ...previous, failed: false, balances: [...previous.balances.filter(item => !matches(item)), updated] };
      });
    } catch {
      if (scopeRef.current === scope) setSnapshot(previous => ({ ...previous, balances: previous.balances.some(item => item.source === source && item.providerId === providerId)
        ? previous.balances.map(item => item.source === source && item.providerId === providerId ? { ...item, errorCode: "refresh_failed" } : item)
        : [...previous.balances, { source, providerId, supported: true, vendor: null, balanceKind: null, balances: [], checkedAt: null, errorCode: "refresh_failed" }] }));
    } finally {
      manualVersions.current.set(token, (manualVersions.current.get(token) ?? 0) + 1);
      manualInFlight.current.delete(token);
      setManualBusy(previous => { const next = { ...previous }; delete next[token]; return next; });
    }
  }, [configDir, ready, scope]);
  useEffect(() => {
    if (!ready) return;
    let disposed = false;
    let inFlight = false;
    let lastAttempt = 0;
    let timer: number | undefined;
    setSnapshot(previous => previous.configDir === configDir && previous.revision === revision
      ? { ...previous, loading: previous.balances.length === 0 }
      : { configDir, revision, balances: [], failed: false, loading: true });
    const refresh = async () => {
      if (disposed || inFlight) return;
      inFlight = true;
      if (timer !== undefined) window.clearTimeout(timer);
      lastAttempt = Date.now();
      let nextDelay = REFRESH_INTERVAL;
      const versionsAtStart = new Map(manualVersions.current);
      const shouldPreserveManual = (item: ProviderBalance) => {
        const token = JSON.stringify([scope, item.source, item.providerId]);
        return manualInFlight.current.has(token) || (manualVersions.current.get(token) ?? 0) !== (versionsAtStart.get(token) ?? 0);
      };
      try {
        const balances = await invoke<ProviderBalance[]>("get_provider_balances", { configDir });
        // A manual query can warm the cache between automatic reads. Follow
        // the data's timestamp rather than restarting a full five-minute wait.
        const now = Date.now();
        for (const item of balances.filter(item => item.supported)) {
          const ttl = item.errorCode ? 60_000 : REFRESH_INTERVAL;
          const checked = item.checkedAt ? Date.parse(item.checkedAt) : NaN;
          const age = Number.isFinite(checked) ? Math.max(0, now - checked) : 0;
          nextDelay = Math.min(nextDelay, Math.max(1_000, ttl - age + 1_000));
        }
        if (!disposed) setSnapshot(previous => ({ configDir, revision, balances: balances.map(next => {
          const current = previous.balances.find(item => item.source === next.source && item.providerId === next.providerId);
          return current && shouldPreserveManual(next) ? current : keepLastBalance(current, next);
        }), failed: false, loading: false }));
      } catch {
        nextDelay = 60_000;
        if (!disposed) setSnapshot(previous => ({ ...previous, loading: false, failed: previous.balances.length === 0, balances: previous.balances.map(item => item.supported && !shouldPreserveManual(item) ? { ...item, errorCode: "refresh_failed" } : item) }));
      } finally {
        inFlight = false;
        lastAttempt = Date.now();
        if (!disposed) timer = window.setTimeout(() => { void refresh(); }, nextDelay);
      }
    };
    const onWake = () => { if (Date.now() - lastAttempt >= REFRESH_INTERVAL) void refresh(); };
    void refresh();
    window.addEventListener("focus", onWake);
    document.addEventListener("visibilitychange", onWake);
    return () => { disposed = true; if (timer !== undefined) window.clearTimeout(timer); window.removeEventListener("focus", onWake); document.removeEventListener("visibilitychange", onWake); };
  }, [configDir, revision, ready, scope]);
  const visible = snapshot.configDir === configDir && snapshot.revision === revision
    ? snapshot : { configDir, revision, balances: [] as ProviderBalance[], loading: true, failed: false };
  const refreshingKeys = Object.keys(manualBusy).flatMap(token => {
    const [requestScope, source, id] = JSON.parse(token) as string[];
    return requestScope === scope ? [`${source}:${id}`] : [];
  });
  return { ...visible, refreshingKeys, refreshBalance };
}

export function supportsProviderBalance(baseUrl: string) {
  try {
    const url = new URL(baseUrl);
    return url.protocol === "https:" && ["api.deepseek.com", "openrouter.ai", "api.siliconflow.cn", "api.moonshot.cn"].includes(url.hostname);
  } catch { return false; }
}
