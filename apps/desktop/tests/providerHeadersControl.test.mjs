import assert from "node:assert/strict";
import test from "node:test";
import { createProviderHeadersControl } from "../src/providerHeadersControl.ts";
import { normalizeProviderHeaders, validateProviderHeaders } from "../src/providerHeaders.ts";

const row = (name = "X-Title", value = "Fixture", source = "static") => ({ name, value, source });
function deferred() {
  let resolve, reject;
  const promise = new Promise((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}
function fixture(text = "source-a") {
  let currentText = text;
  const reads = [], writes = [], events = [], published = [];
  const control = createProviderHeadersControl({
    read(configText) { const task = deferred(); reads.push({ configText, ...task }); return task.promise; },
    update(configText, headers) { const task = deferred(); writes.push({ configText, headers, ...task }); return task.promise; },
    getConfigText: () => currentText,
    isValid: (rows) => validateProviderHeaders(rows).valid,
    normalize: normalizeProviderHeaders,
    onRowsChange: (rows) => events.push({ rows }),
    onConfigChange: (configText) => published.push(configText),
    onStateChange: (state) => events.push({ state }),
  });
  return { control, reads, writes, events, published, setText(text) { currentText = text; }, latestState() { return events.findLast((event) => event.state)?.state; } };
}

test("read requests never replace rows from a newer source or after the source changed before its effect", async () => {
  const f = fixture();
  const first = f.control.load("source-a");
  f.setText("source-b");
  f.reads[0].resolve([row("Old", "old")]);
  await first;
  assert.equal(f.events.some((event) => event.rows), false);
  const second = f.control.load("source-b");
  f.reads[1].resolve([row("New", "new")]);
  await second;
  assert.deepEqual(f.events.filter((event) => event.rows).at(-1).rows, [row("New", "new")]);
  assert.equal(f.latestState().valid, true);
});

test("invalid rows stay editable and prevent saving without starting IPC updates", async () => {
  const f = fixture();
  const loading = f.control.load("source-a");
  await f.control.changeRows([row("", "")]);
  f.reads[0].resolve([row("Stale")]);
  await loading;
  assert.equal(f.writes.length, 0);
  assert.deepEqual(f.events.filter((event) => event.rows).at(-1).rows, [row("", "")]);
  assert.deepEqual(f.latestState(), { busy: false, reading: false, valid: false, error: null });
});

test("rapid valid edits accept only the latest write and normalize names without trimming values", async () => {
  const f = fixture();
  const first = f.control.changeRows([row(" X-Title ", "first")]);
  const second = f.control.changeRows([row(" X-Title ", "  last  ")]);
  assert.deepEqual(f.writes[1].headers, [row("X-Title", "  last  ")]);
  f.writes[1].resolve("patched-last");
  await second;
  f.writes[0].resolve("patched-first");
  await first;
  assert.deepEqual(f.published, ["patched-last"]);
  assert.equal(f.latestState().busy, false);
});

test("a new TOML source rebases edited headers and cannot be overwritten by an old response", async () => {
  const f = fixture();
  const first = f.control.changeRows([row("X-Title", "edited")]);
  f.setText("source-b-with-new-model");
  const rebased = f.control.load("source-b-with-new-model");
  assert.equal(f.writes[1].configText, "source-b-with-new-model");
  f.writes[0].resolve("old-source-result");
  await first;
  assert.deepEqual(f.published, []);
  f.writes[1].resolve("new-source-with-edited-headers");
  await rebased;
  assert.deepEqual(f.published, ["new-source-with-edited-headers"]);
});

test("a local invalid draft survives an unrelated TOML change", async () => {
  const f = fixture();
  await f.control.changeRows([row("invalid name", "keep-editing")]);
  f.setText("changed-model");
  await f.control.load("changed-model");
  assert.equal(f.reads.length, 0);
  assert.equal(f.writes.length, 0);
  assert.equal(f.latestState().valid, false);
  assert.deepEqual(f.events.filter((event) => event.rows).at(-1).rows, [row("invalid name", "keep-editing")]);
});

test("the parent echo of a successful patch does not reread or erase local drafts", async () => {
  const f = fixture();
  const writing = f.control.changeRows([row()]);
  f.writes[0].resolve("patched-a");
  await writing;
  f.setText("patched-a");
  await f.control.load("patched-a");
  assert.equal(f.reads.length, 0);
  assert.equal(f.writes.length, 1);
  assert.equal(f.latestState().valid, true);
});

test("editing before an earlier patch is echoed rebases the latest edit against that patch", async () => {
  const f = fixture();
  const first = f.control.changeRows([row("X-Title", "first")]);
  f.writes[0].resolve("patched-a");
  await first;
  const second = f.control.changeRows([row("X-Title", "second")]);
  f.setText("patched-a");
  const rebase = f.control.load("patched-a");
  f.writes[1].resolve("stale-second");
  await second;
  f.writes[2].resolve("patched-second");
  await rebase;
  assert.equal(f.writes[2].configText, "patched-a");
  assert.deepEqual(f.published, ["patched-a", "patched-second"]);
});

test("unmount invalidates pending reads and writes without emitting events", async () => {
  for (const operation of ["read", "write"]) {
    const f = fixture();
    const task = operation === "read" ? f.control.load("source-a") : f.control.changeRows([row()]);
    const count = f.events.length;
    f.control.dispose();
    if (operation === "read") f.reads[0].resolve([row("Stale")]);
    else f.writes[0].resolve("stale-result");
    await task;
    assert.equal(f.events.length, count);
    assert.deepEqual(f.published, []);
  }
});

test("IPC failures expose only a generic operation marker and can be retried", async () => {
  const f = fixture();
  const loading = f.control.load("source-a");
  f.reads[0].reject(new Error("header-secret\nresolved-env-secret"));
  await loading;
  assert.equal(f.latestState().error, "read");
  const retry = f.control.retry();
  f.reads[1].resolve([row()]);
  await retry;
  const writing = f.control.changeRows([row("X-Title", "changed")]);
  f.writes[0].reject(new Error("header-secret\nresolved-env-secret"));
  await writing;
  assert.equal(f.latestState().error, "write");
  assert.equal(JSON.stringify(f.events).includes("header-secret"), false);
  const writeRetry = f.control.retry();
  f.writes[1].resolve("retry-success");
  await writeRetry;
  assert.deepEqual(f.published, ["retry-success"]);
  assert.equal(f.latestState().valid, true);
});
