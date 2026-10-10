import assert from "node:assert/strict";
import test from "node:test";
import { AppUpdaterController, appUpdaterCheckFailureMessage, classifyAppUpdaterCheckError, isAppUpdateBusy, UPDATER_SLOW_NOTICE_MS } from "../src/appUpdaterController.ts";

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function fixture() {
  const counts = { checks: 0, installs: 0, restarts: 0, closes: 0 };
  const timers = new Map();
  let timerId = 0;
  let update = { rid: 12, currentVersion: "0.3.20", version: "0.3.21", body: "Update notes", close: async () => { counts.closes++; } };
  let operation;
  let checkOperation;
  let restartOperation;
  const controller = new AppUpdaterController({
    check: async () => { counts.checks++; return checkOperation ? checkOperation.promise : update; },
    install: (value, onEvent, options) => {
      counts.installs++;
      operation = { ...deferred(), value, onEvent, options };
      return operation.promise;
    },
    restart: () => { counts.restarts++; return restartOperation?.promise || Promise.resolve(); },
    setTimer: (callback, delay) => { assert.equal(delay, UPDATER_SLOW_NOTICE_MS); timers.set(++timerId, callback); return timerId; },
    clearTimer: (id) => { timers.delete(id); },
  });
  return {
    controller, counts, timers,
    get operation() { return operation; },
    setCheckOperation(value) { checkOperation = value; },
    setRestartOperation(value) { restartOperation = value; },
    setUpdate(value) { update = value; },
    waitLonger() { for (const [id, callback] of timers) { timers.delete(id); callback(); } },
  };
}

test("download, verification and shutdown preparation are distinct; ready requires installation result", async () => {
  const f = fixture();
  assert.equal(await f.controller.check(), "available");
  const job = f.controller.downloadAndInstall({ timeout: 4000 });
  assert.equal(f.operation.options.timeout, 4000);
  f.operation.onEvent({ event: "Started", data: { contentLength: 100 } });
  f.operation.onEvent({ event: "Progress", data: { chunkLength: 100 } });
  assert.equal(f.controller.getSnapshot().phase, "downloading");
  for (const [event, phase] of [["Verifying", "verifying"], ["Preparing", "preparing"], ["Installing", "installing"]]) {
    f.operation.onEvent({ event });
    assert.equal(f.controller.getSnapshot().phase, phase);
    assert.equal(isAppUpdateBusy(phase), true);
  }
  f.operation.resolve({ restartRequired: true });
  assert.equal(await job, "ready");
  assert.equal(f.controller.getSnapshot().phase, "ready");
  assert.equal(f.timers.size, 0);
});

test("clicking repeatedly and checking during an update cannot launch a second installer", async () => {
  const f = fixture();
  await f.controller.check();
  const job = f.controller.downloadAndInstall();
  assert.equal(f.controller.downloadAndInstall(), job);
  assert.equal(f.controller.retry(), job);
  assert.equal(await f.controller.check({ force: true }), "available");
  assert.equal(f.counts.checks, 1);
  f.operation.onEvent({ event: "Preparing" });
  assert.equal(f.controller.downloadAndInstall(), job);
  f.operation.resolve({ restartRequired: true });
  await job;
  await f.controller.downloadAndInstall();
  await f.controller.check({ force: true });
  assert.equal(f.counts.installs, 1);
  assert.equal(f.counts.checks, 1);
  assert.equal(f.counts.closes, 0);
});

test("preparation failure is not success; shows safe details and allows a deliberate retry", async () => {
  const f = fixture();
  await f.controller.check();
  const job = f.controller.downloadAndInstall();
  f.operation.onEvent({ event: "Preparing" });
  f.operation.reject({ stage: "prepare", message: "无法恢复连接配置，尚未启动安装程序。", logPath: "C:\\Logs\\update.log" });
  assert.equal(await job, "error");
  assert.equal(f.controller.getSnapshot().failure, "prepare");
  assert.equal(f.controller.getSnapshot().errorMessage, "无法恢复连接配置，尚未启动安装程序。");
  assert.equal(f.controller.getSnapshot().logPath, "C:\\Logs\\update.log");
  assert.equal(f.timers.size, 0);
  const retry = f.controller.retry();
  assert.equal(f.counts.installs, 2);
  assert.equal(f.controller.getSnapshot().errorMessage, null);
  f.operation.resolve({ restartRequired: true });
  assert.equal(await retry, "ready");
});

test("signature rejection never displays success or exposes an arbitrary plugin error", async () => {
  const f = fixture();
  await f.controller.check();
  const job = f.controller.downloadAndInstall();
  f.operation.onEvent({ event: "Verifying" });
  f.operation.reject(new Error("signature failed at https://private.example.test?token=secret"));
  assert.equal(await job, "error");
  assert.equal(f.controller.getSnapshot().failure, "verify");
  assert.equal(f.controller.getSnapshot().errorMessage, null);
});

test("successful handoff followed by an IPC disconnect must not offer or start another installation", async () => {
  const f = fixture();
  await f.controller.check();
  const job = f.controller.downloadAndInstall();
  f.operation.onEvent({ event: "HandedOff" });
  f.operation.reject(new Error("IPC disconnected as the app exits"));
  assert.equal(await job, "handed-off");
  assert.equal(f.controller.getSnapshot().failure, null);
  assert.equal(isAppUpdateBusy(f.controller.getSnapshot().phase), true);
  assert.equal(await f.controller.retry(), "handed-off");
  assert.equal(await f.controller.restart(), "handed-off");
  f.waitLonger();
  assert.equal(f.controller.getSnapshot().takingLonger, true);
  assert.equal(f.counts.installs, 1);
  assert.equal(f.counts.restarts, 0);
});

test("handoff result without restart remains pending installer completion, not installed", async () => {
  const f = fixture();
  await f.controller.check();
  const job = f.controller.downloadAndInstall();
  f.operation.resolve({ restartRequired: false });
  assert.equal(await job, "handed-off");
  assert.equal(await f.controller.downloadAndInstall(), "handed-off");
  assert.equal(f.counts.installs, 1);
});

test("slow notices never abort a pending operation and new progress clears the notice", async () => {
  const f = fixture();
  await f.controller.check();
  const job = f.controller.downloadAndInstall();
  f.waitLonger();
  assert.equal(f.controller.getSnapshot().takingLonger, true);
  assert.equal(f.controller.getSnapshot().phase, "downloading");
  assert.equal(f.controller.retry(), job);
  f.operation.onEvent({ event: "Progress", data: { chunkLength: 12 } });
  assert.equal(f.controller.getSnapshot().takingLonger, false);
  f.operation.onEvent({ event: "Preparing" });
  f.waitLonger();
  assert.equal(f.controller.getSnapshot().takingLonger, true);
  assert.equal(f.controller.getSnapshot().phase, "preparing");
  assert.equal(f.counts.installs, 1);
  f.operation.resolve({ restartRequired: true });
  await job;
  assert.equal(f.controller.getSnapshot().takingLonger, false);
});

test("late events from a failed attempt do not corrupt the retried download", async () => {
  const f = fixture();
  await f.controller.check();
  const first = f.controller.downloadAndInstall();
  const old = f.operation;
  old.reject({ stage: "download", message: "Network unavailable" });
  await first;
  const second = f.controller.retry();
  old.onEvent({ event: "HandedOff" });
  old.onEvent({ event: "Progress", data: { chunkLength: 900 } });
  assert.equal(f.controller.getSnapshot().phase, "downloading");
  assert.equal(f.controller.getSnapshot().downloadedBytes, 0);
  f.operation.resolve({ restartRequired: true });
  await second;
});

test("only a confirmed ready update can relaunch, and failed relaunch retries only relaunch", async () => {
  const f = fixture();
  await f.controller.check();
  assert.equal(await f.controller.restart(), "available");
  const job = f.controller.downloadAndInstall();
  f.operation.resolve({ restartRequired: true });
  await job;
  const failed = deferred();
  f.setRestartOperation(failed);
  const restart = f.controller.restart();
  failed.reject(new Error("blocked"));
  assert.equal(await restart, "error");
  assert.equal(f.controller.getSnapshot().failure, "restart");
  f.setRestartOperation(null);
  assert.equal(await f.controller.retry(), "ready");
  assert.equal(f.controller.getSnapshot().phase, "ready");
  assert.equal(f.counts.restarts, 2);
  assert.equal(f.counts.installs, 1);
});

test("a pending forced check cannot install its stale update resource", async () => {
  const f = fixture();
  await f.controller.check();
  const next = deferred();
  f.setCheckOperation(next);
  const check = f.controller.check({ force: true });
  assert.equal(await f.controller.downloadAndInstall(), "checking");
  assert.equal(f.counts.installs, 0);
  next.resolve(null);
  await check;
  assert.equal(f.counts.closes, 1);
});


const checkFailures = [
  ["timeout", new Error("error sending request: operation timed out at https://private.fixture.test/update?token=private-secret")],
  ["network", { code: "NetworkError", message: "connection refused; Authorization: Bearer private-secret at https://private.fixture.test" }],
  ["platform", "the platform `windows-x86_64` was not found in the response `platforms` object"],
  ["platform", new Error("Unsupported application architecture, expected x86 or aarch64")],
  ["invalid-release", "Could not fetch a valid release JSON from the remote"],
  ["invalid-release", { code: "Serialization", message: "missing field `signature` at line 1 column 1: private-secret" }],
  ["public-key", "Could not decode public key or signature: private-secret"],
  ["unknown", { message: "Something unexpected happened: private-secret", url: "https://private.fixture.test", authorization: "Bearer private-secret" }],
];

test("check failures are classified into fixed safe explanations in both languages", () => {
  for (const [kind, cause] of checkFailures) {
    assert.equal(classifyAppUpdaterCheckError(cause), kind);
    for (const lang of ["zh", "en"]) {
      const message = appUpdaterCheckFailureMessage(kind, lang);
      assert.ok(message.length > 20);
      assert.ok(!message.includes("private-secret"));
      assert.ok(!message.includes("https://"));
      assert.ok(!message.includes("Authorization"));
    }
  }
  for (const cause of [null, undefined, 123, false, {}, { message: {} }]) assert.equal(classifyAppUpdaterCheckError(cause), "unknown");
  assert.equal(classifyAppUpdaterCheckError("Unrecognized error https://fixture.test/timeout?publickey=secret"), "unknown", "URL text alone must not invent the reason");
});

test("SDK check rejection retains a safe cause without any raw endpoint or credentials", async () => {
  for (const [kind, cause] of checkFailures) {
    const f = fixture();
    const rejected = deferred();
    f.setCheckOperation(rejected);
    const job = f.controller.check();
    rejected.reject(cause);
    assert.equal(await job, "error");
    const state = f.controller.getSnapshot();
    assert.equal(state.phase, "error");
    assert.equal(state.failure, "check");
    assert.equal(state.checkFailure, kind);
    assert.equal(state.errorMessage, appUpdaterCheckFailureMessage(kind));
    const visible = JSON.stringify(state);
    for (const secret of ["private-secret", "private.fixture.test", "Authorization", "Bearer"]) assert.ok(!visible.includes(secret));
    assert.equal(f.counts.installs, 0);
  }
});

test("failed forced checks release their prior RID and clear stale release details", async () => {
  const f = fixture();
  f.setUpdate({ rid: 21, currentVersion: "0.3.21", version: "0.3.24", body: "Old release notes", date: "2026-10-01", close: async () => { f.counts.closes++; } });
  await f.controller.check();
  assert.equal(f.controller.getSnapshot().latestVersion, "0.3.24");
  const failed = deferred();
  f.setCheckOperation(failed);
  const job = f.controller.check({ force: true });
  assert.equal(f.controller.getSnapshot().latestVersion, null);
  assert.equal(f.controller.getSnapshot().notes, null);
  failed.reject(new Error("NetworkError: connection refused"));
  await job;
  assert.equal(f.counts.closes, 1);
  assert.equal(f.controller.getSnapshot().failure, "check");
  assert.equal(f.controller.getSnapshot().checkFailure, "network");
  for (const field of ["latestVersion", "notes", "publishedAt"]) assert.equal(f.controller.getSnapshot()[field], null);
  assert.equal(await f.controller.downloadAndInstall(), "error");
  assert.equal(f.counts.installs, 0, "failed SDK metadata cannot authorize an installer");
});

test("retry checks again and installation requires a newly obtained legal SDK RID", async () => {
  const f = fixture();
  const failed = deferred();
  f.setCheckOperation(failed);
  const first = f.controller.check();
  failed.reject(new Error("operation timed out"));
  await first;
  const pending = deferred();
  f.setCheckOperation(pending);
  const retry = f.controller.retry();
  assert.equal(f.counts.checks, 2);
  assert.equal(f.controller.getSnapshot().phase, "checking");
  assert.equal(await f.controller.downloadAndInstall(), "checking");
  assert.equal(f.counts.installs, 0);
  pending.resolve({ rid: 24, currentVersion: "0.3.21", version: "0.3.24", body: "New release", close: async () => { f.counts.closes++; } });
  assert.equal(await retry, "available");
  assert.equal(f.controller.getSnapshot().errorMessage, null);
  assert.equal(f.controller.getSnapshot().checkFailure, null);
  assert.equal(f.counts.installs, 0, "retrying the check must not automatically install");
  const installing = f.controller.downloadAndInstall();
  assert.equal(f.operation.value.rid, 24);
  assert.equal(f.counts.installs, 1);
  f.operation.resolve({ restartRequired: true });
  assert.equal(await installing, "ready");
});

test("invalid SDK resource IDs are closed and can never reach installation", async () => {
  for (const rid of [-1, NaN, Infinity, 1.5, 0x1_0000_0000, "24", undefined]) {
    const f = fixture();
    f.setUpdate({ rid, currentVersion: "0.3.21", version: "0.3.24", close: async () => { f.counts.closes++; } });
    assert.equal(await f.controller.check(), "error", String(rid));
    assert.equal(f.controller.getSnapshot().checkFailure, "invalid-release");
    assert.equal(f.counts.closes, 1);
    assert.equal(await f.controller.downloadAndInstall(), "error");
    assert.equal(f.counts.installs, 0);
  }
  const f = fixture();
  f.setUpdate({ rid: 0, currentVersion: "0.3.21", version: "0.3.24", close: async () => { f.counts.closes++; } });
  assert.equal(await f.controller.check(), "available", "Tauri resource IDs may start at zero");
});


test("automatic transport recovery starts a fresh byte count and installs only once", async () => {
  const f = fixture();
  await f.controller.check();
  const job = f.controller.downloadAndInstall();
  f.operation.onEvent({ event: "Started", data: { contentLength: 100 } });
  f.operation.onEvent({ event: "Progress", data: { chunkLength: 45 } });
  assert.equal(f.controller.getSnapshot().downloadedBytes, 45);
  f.operation.onEvent({ event: "Started", data: { contentLength: 100 } });
  assert.equal(f.controller.getSnapshot().downloadedBytes, 0);
  f.operation.onEvent({ event: "Progress", data: { chunkLength: 100 } });
  f.operation.onEvent({ event: "Verifying" });
  f.operation.onEvent({ event: "Preparing" });
  f.operation.onEvent({ event: "HandedOff" });
  f.operation.resolve({ restartRequired: false });
  assert.equal(await job, "handed-off");
  assert.equal(f.counts.installs, 1);
  assert.equal(f.counts.checks, 1);
});
