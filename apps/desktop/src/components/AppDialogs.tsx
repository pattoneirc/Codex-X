import type { ReactNode } from "react";
import {
  AlertCircle,
  CheckCircle2,
  Download,
  Loader2,
  RefreshCw,
  RotateCcw,
  Settings,
  Sparkles,
} from "lucide-react";

import { INITIAL_APP_UPDATER_STATE, isAppUpdateBusy, type AppUpdaterState } from "../appUpdater";
import { appUpdaterCheckFailureMessage } from "../appUpdaterController";
import type { Lang, StartupDiagnostics } from "../types";
import { Button, ModalShell } from "./ui";

export type AppToastProps = {
  lang: Lang;
  message: string;
  error: string;
  loading?: boolean;
  onDismissMessage: () => void;
  onDismissError: () => void;
};

export function AppToast({
  lang,
  message,
  error,
  loading = false,
  onDismissMessage,
  onDismissError,
}: AppToastProps) {
  const activeText = error || message;
  if (!activeText) return null;

  const isError = Boolean(error);
  const status = loading ? "loading" : isError ? "error" : "success";
  const [firstLine, ...remainingLines] = activeText.split("\n");
  const detail = remainingLines.join("\n").trim();
  const dismiss = isError ? onDismissError : onDismissMessage;

  return (
    <div
      key={`${status}:${activeText}`}
      className={`cx-app-toast cx-app-toast--${status}`}
      role={isError ? "alert" : "status"}
      aria-live={isError ? "assertive" : "polite"}
      onAnimationEnd={(event) => {
        if (event.target !== event.currentTarget || event.animationName !== "cx-app-toast-exit") return;
        dismiss();
      }}
    >
      {loading
        ? <Loader2 className="cx-app-toast-loader" size={18} aria-hidden="true" />
        : <span className="cx-app-toast-dot" aria-hidden="true" />}
      <div className="cx-app-toast-copy">
        <strong>{firstLine || (isError ? (lang === "zh" ? "操作失败" : "Action failed") : "Codex-X")}</strong>
        {detail && <span>{detail}</span>}
      </div>
    </div>
  );
}

export type UpdateDialogProps = {
  open: boolean;
  lang: Lang;
  state?: AppUpdaterState;
  currentVersion?: string | null;
  latestVersion?: string | null;
  onClose: () => void;
  onDownload: () => void;
  onUpdate?: () => void | Promise<unknown>;
  onRetry?: () => void | Promise<unknown>;
  onRestart?: () => void | Promise<unknown>;
};

function formatUpdateBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

export function UpdateDialog({
  open,
  lang,
  state,
  currentVersion,
  latestVersion,
  onClose,
  onDownload,
  onUpdate,
  onRetry,
  onRestart,
}: UpdateDialogProps) {
  const isChinese = lang === "zh";
  const updaterState = state ?? {
    ...INITIAL_APP_UPDATER_STATE,
    phase: "available" as const,
    currentVersion: currentVersion ?? null,
    latestVersion: latestVersion ?? null,
  };
  const phase = updaterState.phase;
  const isBusy = isAppUpdateBusy(phase);
  const checkFailed = phase === "error" && updaterState.failure === "check";
  const failureDetails = checkFailed
    ? appUpdaterCheckFailureMessage(updaterState.checkFailure ?? "unknown", lang)
    : updaterState.errorMessage;
  const totalBytes = updaterState.totalBytes;
  const hasKnownProgress = totalBytes !== null && totalBytes > 0;
  const progress = totalBytes !== null && totalBytes > 0
    ? Math.min(100, Math.round((updaterState.downloadedBytes / totalBytes) * 100))
    : null;

  const copy = isChinese
    ? {
        checkingTitle: "正在检查更新",
        checkingDescription: "正在确认是否有新版本。",
        availableTitle: "发现新版本",
        availableDescription: onUpdate
          ? "可以直接在软件内完成更新，无需重新下载安装包。"
          : "检测到新版本，可前往下载页获取对应平台的安装包。",
        downloadingTitle: "正在下载更新",
        downloadingDescription: "请保持 Codex-X 打开，下载完成后会自动安装。",
        verifyingTitle: "正在验证安装包",
        verifyingDescription: "下载已完成，正在确认安装包完整可靠。",
        preparingTitle: "正在准备更新",
        preparingDescription: "正在保存状态并恢复连接配置，完成后会自动退出并开始安装。",
        installingTitle: "正在启动安装程序",
        installingDescription: "请稍候。如系统询问是否允许安装，请确认授权。",
        handedOffTitle: "安装程序已启动",
        handedOffDescription: "Codex-X 即将退出，请在安装窗口继续。首次升级旧版时，系统可能需要一次管理员授权。",
        readyTitle: "更新已准备好",
        readyDescription: "重新启动 Codex-X 即可使用新版本。",
        errorTitle: checkFailed ? "在线更新检查失败" : "更新没有完成",
        errorDescription: checkFailed ? "在线安装尚未开始。请重试在线检查；下载页可作为备用。" : updaterState.failure === "restart"
          ? "软件未能重新启动，请再试一次。"
          : "请重试；如果仍然失败，也可以前往下载页更新。",
        idleTitle: "当前已是最新版本",
        idleDescription: "暂时没有可用的新版本。",
        current: "当前版本",
        latest: "新版本",
        later: "稍后",
        close: "关闭",
        updateNow: "立即更新",
        downloading: "正在下载",
        verifying: "验证安装包",
        preparing: "准备退出",
        installing: "启动安装程序",
        handedOff: "等待安装程序",
        slowTitle: "这一步比平时慢一些",
        slowDescription: phase === "downloading"
          ? "暂时没有收到新的下载数据，正在等待网络恢复。请勿重复点击更新。"
          : "仍在等待当前步骤完成。请查看是否有系统授权或安装窗口等待操作，不要重复启动安装。",
        detailLabel: "失败原因",
        logLabel: "安装日志",
        restart: "重新启动",
        retry: checkFailed ? "重试在线检查" : "重试",
        downloadPage: "打开下载页",
        backupDownloadPage: "打开下载页（备用）",
        releaseNotes: "本次更新",
      }
    : {
        checkingTitle: "Checking for updates",
        checkingDescription: "Checking whether a new version is available.",
        availableTitle: "New version available",
        availableDescription: onUpdate
          ? "Update directly in the app without downloading the installer again."
          : "A new version is available from the download page for your platform.",
        downloadingTitle: "Downloading update",
        downloadingDescription: "Keep Codex-X open. Installation starts automatically after download.",
        verifyingTitle: "Verifying installer",
        verifyingDescription: "Download complete. Checking the installer before making changes.",
        preparingTitle: "Preparing update",
        preparingDescription: "Saving state and restoring connection settings before exiting to install.",
        installingTitle: "Starting installer",
        installingDescription: "Please wait. Approve the installation if your system asks for permission.",
        handedOffTitle: "Installer started",
        handedOffDescription: "Codex-X will exit. Continue in the installer window. Upgrading an older installation may ask for administrator permission once.",
        readyTitle: "Update is ready",
        readyDescription: "Restart Codex-X to use the new version.",
        errorTitle: checkFailed ? "Online update check failed" : "Update did not finish",
        errorDescription: checkFailed ? "Online installation has not started. Retry the online check; the download page is available as a fallback." : updaterState.failure === "restart"
          ? "Codex-X could not restart. Please try again."
          : "Try again, or use the download page if the problem continues.",
        idleTitle: "Codex-X is up to date",
        idleDescription: "There is no new version available right now.",
        current: "Current",
        latest: "New version",
        later: "Later",
        close: "Close",
        updateNow: "Update now",
        downloading: "Downloading",
        verifying: "Verifying installer",
        preparing: "Preparing to exit",
        installing: "Starting installer",
        handedOff: "Waiting for installer",
        slowTitle: "Taking a little longer",
        slowDescription: phase === "downloading"
          ? "Waiting for more download data. Please do not start another update."
          : "Still waiting for this step to finish. Check for a system permission or installer window requiring your attention. Do not start another installation.",
        detailLabel: "What went wrong",
        logLabel: "Installation log",
        restart: "Restart",
        retry: checkFailed ? "Retry online check" : "Try again",
        downloadPage: "Open download page",
        backupDownloadPage: "Open download page (fallback)",
        releaseNotes: "What's new",
      };

  const phaseCopy = {
    checking: [copy.checkingTitle, copy.checkingDescription],
    available: [copy.availableTitle, copy.availableDescription],
    downloading: [copy.downloadingTitle, copy.downloadingDescription],
    verifying: [copy.verifyingTitle, copy.verifyingDescription],
    preparing: [copy.preparingTitle, copy.preparingDescription],
    installing: [copy.installingTitle, copy.installingDescription],
    "handed-off": [copy.handedOffTitle, copy.handedOffDescription],
    ready: [copy.readyTitle, copy.readyDescription],
    error: [copy.errorTitle, copy.errorDescription],
    idle: [copy.idleTitle, copy.idleDescription],
  };
  const [title, description] = phaseCopy[phase];
  const progressLabel = phase === "downloading" ? copy.downloading
    : phase === "verifying" ? copy.verifying : phase === "preparing" ? copy.preparing
      : phase === "handed-off" ? copy.handedOff : phase === "ready" ? copy.restart : copy.installing;
  const indeterminate = isBusy && (phase !== "downloading" || progress === null);

  const handleClose = () => {
    if (!isBusy) onClose();
  };

  const footer = phase === "available"
    ? (
        <>
          <Button variant="secondary" onClick={handleClose}>{copy.later}</Button>
          <Button
            icon={<Download size={16} />}
            onClick={() => {
              if (onUpdate) void onUpdate();
              else onDownload();
            }}
          >
            {onUpdate ? copy.updateNow : copy.downloadPage}
          </Button>
        </>
      )
    : phase === "ready"
      ? (
          <Button icon={<RefreshCw size={16} />} onClick={() => void onRestart?.()}>
            {copy.restart}
          </Button>
        )
      : phase === "error"
        ? (
            <>
              <Button variant="secondary" icon={<Download size={16} />} onClick={onDownload}>
                {checkFailed ? copy.backupDownloadPage : copy.downloadPage}
              </Button>
              <Button icon={<RotateCcw size={16} />} onClick={() => void onRetry?.()}>
                {copy.retry}
              </Button>
            </>
          )
        : isBusy
          ? (
              <Button disabled icon={<Loader2 className="spin" size={16} />}>
                {progressLabel}
              </Button>
            )
          : <Button variant="secondary" onClick={handleClose}>{copy.close}</Button>;

  return (
    <ModalShell
      open={open}
      onClose={handleClose}
      size="sm"
      title={title}
      description={description}
      closeLabel={copy.close}
      closeOnBackdrop={!isBusy}
      closeOnEscape={!isBusy}
      showCloseButton={!isBusy}
      className="cx-update-dialog"
      footer={footer}
    >
      <div className={`cx-update-dialog-icon cx-update-dialog-icon--${phase}`} aria-hidden="true">
        {phase === "checking" || isBusy
          ? <Loader2 className="spin" size={20} />
          : phase === "ready"
            ? <CheckCircle2 size={20} />
            : phase === "error"
              ? <AlertCircle size={20} />
              : <Sparkles size={20} />}
      </div>
      <dl className="cx-update-version-grid">
        <div><dt>{copy.current}</dt><dd>{updaterState.currentVersion || currentVersion || "-"}</dd></div>
        <div><dt>{copy.latest}</dt><dd>{updaterState.latestVersion || latestVersion || "-"}</dd></div>
      </dl>

      {(isBusy || phase === "ready") && (
        <div className="cx-update-progress" aria-live="polite">
          <div className="cx-update-progress-copy">
            <span>{progressLabel}</span>
            <strong>
              {phase === "downloading"
                ? hasKnownProgress
                  ? `${progress}% · ${formatUpdateBytes(updaterState.downloadedBytes)} / ${formatUpdateBytes(totalBytes)}`
                  : updaterState.downloadedBytes > 0
                    ? formatUpdateBytes(updaterState.downloadedBytes)
                    : "..."
                : phase === "ready"
                  ? "100%"
                  : "..."}
            </strong>
          </div>
          <div
            className={`cx-update-progress-track${indeterminate ? " cx-update-progress-track--indeterminate" : ""}`}
            role="progressbar"
            aria-label={progressLabel}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={phase === "ready" ? 100 : indeterminate ? undefined : progress ?? undefined}
          >
            <span style={{ width: phase === "ready" ? "100%" : indeterminate ? "38%" : `${progress}%` }} />
          </div>
        </div>
      )}

      {updaterState.takingLonger && isBusy && (
        <section className="cx-update-notes" role="status">
          <strong>{copy.slowTitle}</strong>
          <p>{copy.slowDescription}</p>
        </section>
      )}
      {phase === "error" && failureDetails && (
        <section className="cx-update-notes" role="alert">
          <strong>{copy.detailLabel}</strong>
          <p>{failureDetails}</p>
          {updaterState.logPath && <p className="cx-update-log-path">{copy.logLabel}: {updaterState.logPath}</p>}
        </section>
      )}
      {updaterState.notes && phase !== "checking" && (
        <section className="cx-update-notes">
          <strong>{copy.releaseNotes}</strong>
          <p>{updaterState.notes}</p>
        </section>
      )}
    </ModalShell>
  );
}

export type StartupWizardDialogProps = {
  open: boolean;
  closing: boolean;
  mode?: "startup" | "manual";
  lang: Lang;
  diagnostics: StartupDiagnostics | null;
  diagnosticsError?: string;
  configHealthPanel?: ReactNode;
  configDir: string;
  loading: boolean;
  onConfigDirChange: (value: string) => void;
  onRecheck: () => void;
  onSkip: () => void;
  onOpenSettings: () => void;
  onEnter: () => void;
};

export function StartupWizardDialog({
  open,
  closing,
  mode = "startup",
  lang,
  diagnostics,
  diagnosticsError,
  configHealthPanel,
  configDir,
  loading,
  onConfigDirChange,
  onRecheck,
  onSkip,
  onOpenSettings,
  onEnter,
}: StartupWizardDialogProps) {
  const isChinese = lang === "zh";
  const isManual = mode === "manual";
  const recheckButton = (
    <Button
      variant={isManual ? "primary" : "secondary"}
      icon={<RefreshCw size={16} className={loading ? "spin" : undefined} />}
      onClick={onRecheck}
      disabled={loading}
    >
      {loading ? (isChinese ? "正在检查" : "Checking") : (isChinese ? "重新检查" : "Recheck")}
    </Button>
  );

  return (
    <ModalShell
      open={open}
      onClose={onSkip}
      size="lg"
      title={isChinese ? "环境与配置检查" : "Environment & configuration check"}
      description={isManual
        ? (isChinese ? "查看环境与配置状态，发现问题后可选择修复。" : "Review your environment and configuration, and choose whether to repair any issues.")
        : (isChinese ? "首次使用前，检查 Codex 环境与配置是否就绪。" : "Before you get started, check whether your Codex environment and configuration are ready.")}
      showCloseButton={isManual}
      closeLabel={isChinese ? "关闭" : "Close"}
      closeOnBackdrop={isManual}
      closeOnEscape={isManual}
      className={closing ? "cx-startup-dialog cx-startup-dialog--closing" : "cx-startup-dialog"}
      footer={(
        isManual ? (
          <>
            <Button variant="secondary" onClick={onSkip}>{isChinese ? "关闭" : "Close"}</Button>
            {recheckButton}
          </>
        ) : (
          <>
            <Button variant="ghost" onClick={onSkip}>{isChinese ? "跳过" : "Skip"}</Button>
            <Button variant="secondary" icon={<Settings size={16} />} onClick={onOpenSettings}>{isChinese ? "去设置" : "Settings"}</Button>
            <Button icon={<CheckCircle2 size={16} />} onClick={onEnter}>{isChinese ? "进入 Codex-X" : "Enter Codex-X"}</Button>
          </>
        )
      )}
    >
      <div className={`cx-startup-path-control${isManual ? " cx-startup-path-control--manual" : ""}`}>
        <label htmlFor="cx-startup-codex-home">CODEX_HOME</label>
        <input
          id="cx-startup-codex-home"
          value={configDir}
          onChange={(event) => onConfigDirChange(event.target.value)}
          placeholder="~/.codex"
          disabled={loading}
          spellCheck={false}
        />
        {!isManual && recheckButton}
      </div>

      {diagnosticsError && <div className="cx-startup-diagnostics-notice cx-startup-diagnostics-notice--error" role="alert">
        <AlertCircle size={17} aria-hidden="true" />
        <div>
          <strong>{isChinese ? "环境检查未完成" : "Environment check did not finish"}</strong>
          <p>{diagnosticsError}</p>
        </div>
      </div>}

      {!diagnostics && !diagnosticsError && <div className="cx-startup-diagnostics-notice" role="status" aria-live="polite">
        {loading ? <Loader2 className="spin" size={17} aria-hidden="true" /> : <RefreshCw size={17} aria-hidden="true" />}
        <p>{loading
          ? (isChinese ? "正在检查 Codex 环境…" : "Checking your Codex environment…")
          : (isChinese ? "点击重新检查，查看当前环境状态。" : "Choose Recheck to see the current environment status.")}</p>
      </div>}

      {diagnostics && <div className="cx-startup-checks">
        {diagnostics.items.map((item) => {
          const isOk = item.status === "ok";
          const isManual = item.status === "manual";
          const statusText = isChinese
            ? item.message
            : isOk
              ? "Detected"
              : isManual
                ? "Manual selection required"
                : "Not found";
          return (
            <article className={`cx-startup-check${isOk ? " cx-startup-check--ok" : isManual ? " cx-startup-check--manual" : ""}`} key={item.key}>
              <div className="cx-startup-check-icon" aria-hidden="true">
                {isOk ? <CheckCircle2 size={17} /> : <AlertCircle size={17} />}
              </div>
              <div>
                <strong>{item.label}</strong>
                <p>{statusText}</p>
                {item.path && <code title={item.path}>{item.path}</code>}
              </div>
            </article>
          );
        })}
      </div>}

      {configHealthPanel && <div className="cx-startup-config-health">{configHealthPanel}</div>}
    </ModalShell>
  );
}
