import type { AppUpdateInfo, ReleaseInfo } from "./types";

/** Release metadata can identify a version, but does not authorize installation.
 * An installed edition keeps its native check/retry controls after a failed
 * signed-channel check; only editions without a native updater use download-only.
 */
export function releaseInfoFromFallback(update: AppUpdateInfo, nativeCheckAttempted: boolean): ReleaseInfo {
  return {
    status: nativeCheckAttempted && !update.hasUpdate ? "error" : "ok",
    latestVersion: update.latestVersion,
    htmlUrl: update.htmlUrl,
    hasUpdate: update.hasUpdate,
    updateMethod: nativeCheckAttempted ? "native" : update.hasUpdate ? "download" : undefined,
  };
}

export function releaseInfoFromCheckFailure(nativeCheckAttempted: boolean): ReleaseInfo {
  return { status: "error", updateMethod: nativeCheckAttempted ? "native" : undefined };
}
