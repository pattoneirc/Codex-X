import assert from "node:assert/strict";
import test from "node:test";
import { AppUpdaterController } from "../src/appUpdaterController.ts";
import { releaseInfoFromFallback, releaseInfoFromCheckFailure } from "../src/updateCheckFlow.ts";

const metadata = {
  latestVersion: "v0.3.24",
  htmlUrl: "https://github.com/yynxxxxx/Codex-X/releases/tag/v0.3.24",
  hasUpdate: true,
};

test("an installed 0.3.21 edition keeps native retry after a failed channel check and a newer release lookup", async () => {
  let recovery = false;
  let installs = 0;
  const update = { rid: 12, currentVersion: "0.3.21", version: "0.3.24", close: async () => {} };
  const controller = new AppUpdaterController({
    check: async () => {
      if (!recovery) throw new Error("request timed out");
      return update;
    },
    install: async (checked) => {
      assert.equal(checked, update);
      installs++;
      return { restartRequired: true };
    },
    restart: async () => {},
    setTimer: () => 1,
    clearTimer: () => {},
  });
  assert.equal(await controller.check(), "error");
  const release = releaseInfoFromFallback(metadata, true);
  assert.equal(release.updateMethod, "native", "a failed check must not turn an installed edition into download-only");
  assert.equal(release.hasUpdate, true);
  assert.equal(release.latestVersion, "v0.3.24");
  assert.equal(controller.getSnapshot().failure, "check");
  assert.equal(await controller.downloadAndInstall(), "error");
  assert.equal(installs, 0, "release metadata is not a checked installer resource");
  recovery = true;
  assert.equal(await controller.retry(), "available");
  assert.equal(await controller.downloadAndInstall(), "ready");
  assert.equal(installs, 1);
});

test("portable and unsupported editions retain their intentional manual download path", () => {
  assert.deepEqual(releaseInfoFromFallback(metadata, false), { status: "ok", ...metadata, updateMethod: "download" });
  assert.equal(releaseInfoFromFallback({ ...metadata, hasUpdate: false }, false).updateMethod, undefined);
});

test("a failed native channel never claims up-to-date from fallback metadata alone", () => {
  const release = releaseInfoFromFallback({ ...metadata, latestVersion: "v0.3.21", hasUpdate: false }, true);
  assert.equal(release.status, "error");
  assert.equal(release.updateMethod, "native");
  assert.equal(release.hasUpdate, false);
});

test("both checks failing still preserves installed-edition retry instead of a download-only dialog", () => {
  assert.deepEqual(releaseInfoFromCheckFailure(true), { status: "error", updateMethod: "native" });
  assert.deepEqual(releaseInfoFromCheckFailure(false), { status: "error", updateMethod: undefined });
});
