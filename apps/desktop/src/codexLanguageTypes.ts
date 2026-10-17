export type CodexLocale = "auto" | "zh-CN" | "en-US";
export type CodexLanguageStatus = {
  locale: string;
  enabled: boolean;
  pendingEnabled: boolean | null;
  supported: boolean;
  appName: string | null;
  appVersion: string | null;
  phase: "inactive" | "pending" | "starting" | "active" | "failed" | "unsupported";
  message: string;
  readyPages: number;
  restoreAvailable: boolean;
};
