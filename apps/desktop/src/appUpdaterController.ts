import type { CheckOptions, DownloadOptions } from "@tauri-apps/plugin-updater";

export type AppUpdaterPhase = "idle" | "checking" | "available" | "downloading" | "verifying" | "preparing" | "installing" | "handed-off" | "ready" | "error";
export type AppUpdaterCheckFailureKind = "timeout" | "network" | "platform" | "invalid-release" | "public-key" | "unknown";
export type AppUpdaterFailure = "check" | "download" | "verify" | "prepare" | "install" | "restart" | null;
export type AppUpdaterState = Readonly<{
  phase: AppUpdaterPhase;
  currentVersion: string | null;
  latestVersion: string | null;
  notes: string | null;
  publishedAt: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  failure: AppUpdaterFailure;
  errorMessage: string | null;
  checkFailure?: AppUpdaterCheckFailureKind | null;
  logPath: string | null;
  takingLonger: boolean;
}>;
export type AppUpdaterCheckResult = "available" | "up-to-date" | "error";
export type AppUpdaterCheckOptions = CheckOptions & { force?: boolean };
export type AppUpdateEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Verifying" | "Preparing" | "Installing" | "HandedOff" };
export type CheckedAppUpdate = {
  rid: number;
  currentVersion: string;
  version: string;
  body?: string;
  date?: string;
  close: () => Promise<void>;
};
export type AppUpdaterDependencies = {
  check: (options: CheckOptions) => Promise<CheckedAppUpdate | null>;
  install: (update: CheckedAppUpdate, onEvent: (event: AppUpdateEvent) => void, options?: DownloadOptions) => Promise<{ restartRequired: boolean }>;
  restart: () => Promise<void>;
  setTimer: (callback: () => void, delay: number) => unknown;
  clearTimer: (timer: unknown) => void;
};
export const UPDATER_SLOW_NOTICE_MS = 30_000;
export const INITIAL_APP_UPDATER_STATE: AppUpdaterState = {
  phase: "idle", currentVersion: null, latestVersion: null, notes: null, publishedAt: null,
  downloadedBytes: 0, totalBytes: null, failure: null, errorMessage: null, checkFailure: null, logPath: null, takingLonger: false,
};
export function isAppUpdateBusy(phase: AppUpdaterPhase): boolean {
  return ["downloading", "verifying", "preparing", "installing", "handed-off"].includes(phase);
}
function normalizeByteCount(value: number | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}
// Plugin errors can contain private endpoints and credentials. Inspect bounded
// text only to select one of these fixed explanations; never return that text.
export function classifyAppUpdaterCheckError(error: unknown): AppUpdaterCheckFailureKind {
  let text = "";
  if (typeof error === "string") text = error;
  else if (error instanceof Error) text = `${error.name} ${error.message}`;
  else if (typeof error === "object" && error !== null) {
    const record = error as Record<string, unknown>;
    text = ["code", "kind", "type", "name", "message"].map((key) => record[key])
      .filter((value): value is string => typeof value === "string").join(" ");
  }
  text = text.slice(0, 4096).replace(/https?:\/\/[^\s<>"']+/gi, "").toLowerCase();
  if (/timeout|timed?\s*out|time out|etimedout|deadline exceeded|超时/.test(text)) return "timeout";
  if (/(?:platform|target)[\s\S]{0,200}(?:not found|missing|unsupported)|unsupported[\s\S]{0,80}(?:architecture|platform|operating system|\bos\b)|unsupportedarch|unsupportedos|targets?notfound|不支持.*(?:平台|系统|架构)/.test(text)) return "platform";
  if (/release json|(?:invalid|malformed)[^\n]{0,80}(?:release|json|manifest|version)|serialization|semver|invalid updater resource|missing field|expected (?:value|ident)[^\n]{0,80}line|eof while parsing|trailing characters|key must be a string|(?:relative url without a base)|更新信息.*(?:无效|格式)/.test(text)) return "invalid-release";
  if (/public.?key|pubkey|minisign|signature|invalid base64|base64[^\n]{0,80}(?:invalid|decode)|公钥|签名/.test(text)) return "public-key";
  if (/network|failed to fetch|error sending request|connection (?:refused|reset|closed)|connect error|dns|tcp|tls|certificate|resolve (?:host|dns)|http (?:error|status)|status code|could not connect|enotfound|econnreset|econnrefused|网络|连接失败/.test(text)) return "network";
  return "unknown";
}

export function appUpdaterCheckFailureMessage(kind: AppUpdaterCheckFailureKind, lang: "zh" | "en" = "zh"): string {
  const messages = lang === "zh" ? {
    timeout: "在线更新检查超时。请检查网络或代理后重试在线检查。",
    network: "无法连接在线更新服务。请检查网络或代理后重试在线检查。",
    platform: "更新服务没有提供适用于当前系统或架构的在线安装包。请重试在线检查；也可使用下载页的对应平台安装包。",
    "invalid-release": "在线更新信息格式无效或未能读取。请重试在线检查；若持续失败，可使用下载页的安装包。",
    "public-key": "在线更新签名或公钥配置无效。请重试在线检查；若持续失败，请从下载页获取官方安装包。",
    unknown: "在线更新检查未完成。请重试在线检查；下载页可作为备用。",
  } : {
    timeout: "The online update check timed out. Check your network or proxy, then retry the online check.",
    network: "The online update service could not be reached. Check your network or proxy, then retry the online check.",
    platform: "The update service did not provide an online installer for this system or architecture. Retry the online check, or use your platform’s installer from the download page.",
    "invalid-release": "Online update information is invalid or could not be read. Retry the online check; if it keeps failing, use the installer from the download page.",
    "public-key": "The online update signature or public key configuration is invalid. Retry the online check; if it keeps failing, get the official installer from the download page.",
    unknown: "The online update check did not finish. Retry the online check; the download page is available as a fallback.",
  };
  return messages[kind] ?? messages.unknown;
}

function installationError(error: unknown, fallback: AppUpdaterFailure) {
  // Only the app's structured installation command supplies user-facing details.
  // Do not render arbitrary plugin/network errors, which can include private URLs.
  if (typeof error === "object" && error !== null && "stage" in error && "message" in error) {
    const data = error as Record<string, unknown>;
    const stage = ["download", "verify", "prepare", "install"].includes(String(data.stage))
      ? data.stage as AppUpdaterFailure : fallback;
    return {
      failure: stage,
      errorMessage: typeof data.message === "string" ? data.message.slice(0, 1000) : null,
      logPath: typeof data.logPath === "string" ? data.logPath : null,
    };
  }
  return { failure: fallback, errorMessage: null, logPath: null };
}

export class AppUpdaterController {
  private state: AppUpdaterState = INITIAL_APP_UPDATER_STATE;
  private readonly listeners = new Set<() => void>();
  private readonly dependencies: AppUpdaterDependencies;
  private update: CheckedAppUpdate | null = null;
  private checkPromise: Promise<AppUpdaterCheckResult> | null = null;
  private updatePromise: Promise<AppUpdaterPhase> | null = null;
  private restartPromise: Promise<AppUpdaterPhase> | null = null;
  private retryAction: "check" | "update" | "restart" = "check";
  private slowTimer: unknown = null;
  private attempt = 0;

  constructor(dependencies: AppUpdaterDependencies) { this.dependencies = dependencies; }
  readonly getSnapshot = (): AppUpdaterState => this.state;
  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };
  readonly check = (options: AppUpdaterCheckOptions = {}): Promise<AppUpdaterCheckResult> => {
    if (this.checkPromise) return this.checkPromise;
    if (isAppUpdateBusy(this.state.phase) || this.state.phase === "ready") {
      return Promise.resolve(this.update ? "available" : "error");
    }
    if (this.update && !options.force) return Promise.resolve("available");
    const { force: _force, ...pluginOptions } = options;
    this.checkPromise = this.performCheck(pluginOptions).finally(() => { this.checkPromise = null; });
    return this.checkPromise;
  };
  readonly downloadAndInstall = (options?: DownloadOptions): Promise<AppUpdaterPhase> => {
    if (this.updatePromise) return this.updatePromise;
    if (this.checkPromise) return Promise.resolve(this.state.phase);
    if (this.state.phase === "ready" || this.state.phase === "handed-off") return Promise.resolve(this.state.phase);
    if (!this.update) {
      this.retryAction = "check";
      this.setState({ phase: "error", failure: "check", checkFailure: "invalid-release", errorMessage: appUpdaterCheckFailureMessage("invalid-release"), latestVersion: null, notes: null, publishedAt: null });
      return Promise.resolve("error");
    }
    this.updatePromise = this.performDownloadAndInstall(this.update, options).finally(() => { this.updatePromise = null; });
    return this.updatePromise;
  };
  readonly retry = (): Promise<AppUpdaterCheckResult | AppUpdaterPhase> => {
    if (this.retryAction === "restart") return this.restart();
    if (this.retryAction === "update" && this.update) return this.downloadAndInstall();
    return this.check({ force: true });
  };
  readonly restart = (): Promise<AppUpdaterPhase> => {
    if (this.restartPromise) return this.restartPromise;
    if (this.state.phase !== "ready" && this.state.failure !== "restart") return Promise.resolve(this.state.phase);
    this.restartPromise = this.performRestart().finally(() => { this.restartPromise = null; });
    return this.restartPromise;
  };

  private async performCheck(options: CheckOptions): Promise<AppUpdaterCheckResult> {
    this.retryAction = "check";
    this.setState({ phase: "checking", downloadedBytes: 0, totalBytes: null, failure: null, errorMessage: null, checkFailure: null, latestVersion: null, notes: null, publishedAt: null, logPath: null, takingLonger: false });
    try {
      const nextUpdate = await this.dependencies.check(options);
      await this.replaceUpdate(nextUpdate);
      if (nextUpdate && (!Number.isInteger(nextUpdate.rid) || nextUpdate.rid < 0 || nextUpdate.rid > 0xffff_ffff)) {
        throw new Error("invalid updater resource");
      }
      if (!nextUpdate) {
        this.setState({ phase: "idle", latestVersion: null, notes: null, publishedAt: null });
        return "up-to-date";
      }
      this.retryAction = "update";
      this.setState({ phase: "available", currentVersion: nextUpdate.currentVersion, latestVersion: nextUpdate.version,
        notes: nextUpdate.body?.trim() || null, publishedAt: nextUpdate.date || null });
      return "available";
    } catch (error) {
      await this.replaceUpdate(null);
      const checkFailure = classifyAppUpdaterCheckError(error);
      this.setState({ phase: "error", failure: "check", checkFailure, errorMessage: appUpdaterCheckFailureMessage(checkFailure), latestVersion: null, notes: null, publishedAt: null });
      return "error";
    }
  }

  private async performDownloadAndInstall(update: CheckedAppUpdate, options?: DownloadOptions): Promise<AppUpdaterPhase> {
    this.retryAction = "update";
    const attempt = ++this.attempt;
    let acceptingEvents = true;
    let handedOff = false;
    this.setState({ phase: "downloading", downloadedBytes: 0, totalBytes: null, failure: null, errorMessage: null, checkFailure: null, logPath: null, takingLonger: false });
    this.armSlowNotice();
    try {
      const result = await this.dependencies.install(update, (event) => {
        if (!acceptingEvents || attempt !== this.attempt || handedOff) return;
        if (event.event === "HandedOff") handedOff = true;
        this.handleInstallEvent(event);
      }, options);
      // Windows can leave this WebView alive briefly after handing off. The
      // installer has started, but its success is not yet known; never relaunch it.
      const phase = handedOff || !result.restartRequired ? "handed-off" : "ready";
      this.setState({ phase, failure: null, takingLonger: false });
      if (phase === "handed-off") this.armSlowNotice();
      return phase;
    } catch (error) {
      if (handedOff) return "handed-off";
      const fallback = this.state.phase === "verifying" ? "verify" : this.state.phase === "preparing" ? "prepare"
        : this.state.phase === "installing" ? "install" : "download";
      this.setState({ phase: "error", ...installationError(error, fallback), takingLonger: false });
      return "error";
    } finally {
      acceptingEvents = false;
      if (this.state.phase !== "handed-off") this.clearSlowNotice();
    }
  }
  private handleInstallEvent(event: AppUpdateEvent): void {
    if (event.event === "Started") {
      this.setState({ phase: "downloading", downloadedBytes: 0, totalBytes: normalizeByteCount(event.data.contentLength) });
    } else if (event.event === "Progress") {
      const bytes = normalizeByteCount(event.data.chunkLength);
      if (bytes === null) return;
      const next = this.state.downloadedBytes + bytes;
      this.setState({ downloadedBytes: this.state.totalBytes === null ? next : Math.min(next, this.state.totalBytes) });
    } else {
      const phase = { Verifying: "verifying", Preparing: "preparing", Installing: "installing", HandedOff: "handed-off" }[event.event] as AppUpdaterPhase;
      this.setState({ phase, downloadedBytes: this.state.totalBytes ?? this.state.downloadedBytes });
    }
    this.armSlowNotice();
  }
  private armSlowNotice(): void {
    this.clearSlowNotice();
    this.setState({ takingLonger: false });
    this.slowTimer = this.dependencies.setTimer(() => {
      this.slowTimer = null;
      if (isAppUpdateBusy(this.state.phase)) this.setState({ takingLonger: true });
    }, UPDATER_SLOW_NOTICE_MS);
  }
  private clearSlowNotice(): void {
    if (this.slowTimer !== null) this.dependencies.clearTimer(this.slowTimer);
    this.slowTimer = null;
  }
  private async performRestart(): Promise<AppUpdaterPhase> {
    this.retryAction = "restart";
    this.setState({ failure: null, errorMessage: null, checkFailure: null, logPath: null });
    try { await this.dependencies.restart(); this.setState({ phase: "ready" }); return "ready"; }
    catch { this.setState({ phase: "error", failure: "restart" }); return "error"; }
  }
  private async replaceUpdate(next: CheckedAppUpdate | null): Promise<void> {
    const previous = this.update;
    this.update = next;
    if (previous && previous !== next) {
      try { await previous.close(); }
      catch { console.warn("Unable to release the previous updater resource"); }
    }
  }
  private setState(patch: Partial<AppUpdaterState>): void {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((listener) => listener());
  }
}
