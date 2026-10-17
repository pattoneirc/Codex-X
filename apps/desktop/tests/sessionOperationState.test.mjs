import assert from "node:assert/strict";
import test from "node:test";
import {
  createSessionStatusTracker,
  providerActivationIsBusy,
  readCurrentSessionStatus,
  sessionStatusContextKey,
} from "../src/sessionOperationState.ts";

const context = (targetProvider = "custom", providerRevision = 0, configDir = "/fixture/codex") => ({ configDir, targetProvider, providerRevision });
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
};

test("session sync permits activation while general operations and other actions keep their locks", () => {
  assert.equal(providerActivationIsBusy(["sessionSync"], ["syncSessions"]), false);
  assert.equal(providerActivationIsBusy(["sessionSync", "sessionSync"], ["syncSessions", "syncSessions"]), false);
  assert.equal(providerActivationIsBusy(["sessionSync", "general"], ["syncSessions"]), true);
  assert.equal(providerActivationIsBusy(["general"], []), true);
  assert.equal(providerActivationIsBusy([], ["checkSessions"]), true);
  assert.equal(providerActivationIsBusy([], []), false);
  // A newer sync token must not hide an older unrelated operation.
  assert.equal(providerActivationIsBusy(["sessionSync"], ["importCcSwitch", "syncSessions"]), true);
});

test("captured targets cannot change when the current context is mutated", () => {
  const tracker = createSessionStatusTracker();
  const source = context("openai");
  const request = tracker.begin(source);
  source.targetProvider = "custom";
  assert.equal(request.context.targetProvider, "openai");
  assert.equal(Object.isFrozen(request.context), true);
  assert.equal(tracker.isCurrent(request, source), false);
});

test("an old status cannot overwrite a newly activated provider", async () => {
  const tracker = createSessionStatusTracker();
  let current = context("openai");
  const old = deferred();
  const published = [];
  const oldRead = readCurrentSessionStatus(tracker, () => current, () => old.promise, (status) => published.push(status));
  current = null; // Activation has begun but the new live config is not ready.
  tracker.invalidate();
  old.resolve({ targetProvider: "openai", needsSync: false });
  assert.equal(await oldRead, null);
  current = context("custom", 1);
  await readCurrentSessionStatus(tracker, () => current, async () => ({ targetProvider: "custom", needsSync: true }), (status) => published.push(status));
  assert.deepEqual(published, [{ targetProvider: "custom", needsSync: true }]);
});

test("switching between providers sharing the custom key still invalidates old replies", async () => {
  const tracker = createSessionStatusTracker();
  let current = context("custom", 10);
  const old = deferred();
  const published = [];
  const pending = readCurrentSessionStatus(tracker, () => current, () => old.promise, (status) => published.push(status));
  current = context("custom", 11);
  old.resolve({ targetProvider: "custom", needsSync: false });
  assert.equal(await pending, null);
  assert.deepEqual(published, []);
});

test("completion reads the current provider instead of publishing the completed job's old status", async () => {
  const tracker = createSessionStatusTracker();
  let current = context("openai");
  const capturedTarget = { ...current };
  const job = deferred();
  const reads = [], published = [];
  const completion = job.promise.then(() => readCurrentSessionStatus(tracker, () => current, async (scope) => {
    reads.push(scope);
    return { targetProvider: scope.targetProvider, needsSync: true };
  }, (status) => published.push(status)));
  current = context("custom", 1);
  job.resolve({ status: { targetProvider: "openai", needsSync: false } });
  await completion;
  assert.equal(capturedTarget.targetProvider, "openai");
  assert.equal(reads[0].targetProvider, "custom");
  assert.deepEqual(published, [{ targetProvider: "custom", needsSync: true }]);
});

test("late responses from a different Codex directory are ignored", async () => {
  const tracker = createSessionStatusTracker();
  let current = context("custom", 0, "/fixture/old");
  const task = deferred();
  let applied = false;
  const read = readCurrentSessionStatus(tracker, () => current, () => task.promise, () => { applied = true; });
  current = context("custom", 0, "/fixture/new");
  task.resolve({ targetProvider: "custom" });
  assert.equal(await read, null);
  assert.equal(applied, false);
});

test("the newest status request wins when reads finish out of order", async () => {
  const tracker = createSessionStatusTracker();
  const current = context();
  const old = deferred(), fresh = deferred();
  const published = [];
  const a = readCurrentSessionStatus(tracker, () => current, () => old.promise, (status) => published.push(status));
  const b = readCurrentSessionStatus(tracker, () => current, () => fresh.promise, (status) => published.push(status));
  fresh.resolve({ targetProvider: "custom", needsSync: true });
  await b;
  old.resolve({ targetProvider: "custom", needsSync: false });
  assert.equal(await a, null);
  assert.deepEqual(published, [{ targetProvider: "custom", needsSync: true }]);
});

test("late failures are ignored and a current failure remains visible", async () => {
  const tracker = createSessionStatusTracker();
  let current = context();
  const task = deferred();
  const old = readCurrentSessionStatus(tracker, () => current, () => task.promise, () => assert.fail("stale read applied"));
  current = context("openai", 1);
  task.reject(new Error("old provider read failed"));
  assert.equal(await old, null);
  await assert.rejects(readCurrentSessionStatus(tracker, () => current, async () => { throw new Error("current read failed"); }, () => {}), /current read failed/);
});

test("provider mismatches and suspended contexts never publish a misleading synced status", async () => {
  const tracker = createSessionStatusTracker();
  let reads = 0, applied = false;
  assert.equal(await readCurrentSessionStatus(tracker, () => null, async () => { reads += 1; return { targetProvider: "old" }; }, () => { applied = true; }), null);
  assert.equal(reads, 0);
  assert.equal(await readCurrentSessionStatus(tracker, () => context("custom"), async () => ({ targetProvider: "openai", needsSync: false }), () => { applied = true; }), null);
  assert.equal(applied, false);
});

test("context keys distinguish target, directory and activation generation", () => {
  const variants = [context(), context("openai"), context("custom", 1), context("custom", 0, "/fixture/other")];
  assert.equal(new Set(variants.map(sessionStatusContextKey)).size, variants.length);
});
