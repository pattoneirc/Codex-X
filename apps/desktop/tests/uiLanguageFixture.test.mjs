import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("./fixtures/ui-runtime.js", import.meta.url), "utf8");
const plain = (value) => JSON.parse(JSON.stringify(value));

function memoryStorage() {
  const values = new Map();
  return { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, String(value)), removeItem: (key) => values.delete(key) };
}

function loadFixture() {
  class Element {
    constructor(tag) { this.tagName = tag; this.style = {}; this.children = []; this.listeners = new Map(); this.attributes = new Map(); }
    append(...children) { this.children.push(...children); }
    setAttribute(name, value) { this.attributes.set(name, value); }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    click() { this.listeners.get("click")?.({ target: this }); }
  }
  const ready = [];
  const body = new Element("body");
  const sandbox = {
    structuredClone, URL, URLSearchParams,
    location: { search: "", reload() { assert.fail("fixture language commands must not reload the browser"); } },
    localStorage: memoryStorage(), sessionStorage: memoryStorage(),
    document: { body, createElement: (tag) => new Element(tag), addEventListener(name, callback) { assert.equal(name, "DOMContentLoaded"); ready.push(callback); } },
  };
  sandbox.window = sandbox;
  vm.runInNewContext(source, sandbox, { timeout: 1000 });
  const invoke = async (command, args) => plain(await sandbox.__TAURI_INTERNALS__.invoke(command, args));
  const get = () => invoke("get_codex_language_status", { configDir: "/fixture/codex" });
  const save = (enabled) => invoke("save_codex_language_preferences", { configDir: "/fixture/codex", locale: enabled ? "zh-CN" : "auto", enabled });
  const apply = () => invoke("apply_codex_language_preferences", { configDir: "/fixture/codex", restore: false });
  return { sandbox, body, get, save, apply, invoke, controls: sandbox.__CODEX_X_FIXTURE__, mount() { for (const callback of ready) callback(); } };
}

test("language fixture starts in Chinese and save stages or cancels without changing actual state", async () => {
  const f = loadFixture();
  assert.deepEqual(await f.get(), { locale: "zh-CN", enabled: true, pendingEnabled: null, supported: true, appName: "ChatGPT", appVersion: "fixture", phase: "active", message: "", readyPages: 0, restoreAvailable: false });
  const staged = await f.save(false);
  assert.equal(staged.enabled, true);
  assert.equal(staged.locale, "zh-CN");
  assert.equal(staged.pendingEnabled, false);
  assert.equal((await f.get()).pendingEnabled, false);
  assert.equal((await f.save(true)).pendingEnabled, null, "choosing the actual state cancels the pending change");
});

test("language apply changes only the memory state and clears pending intent", async () => {
  const f = loadFixture();
  await f.save(false);
  assert.deepEqual(await f.apply(), { locale: "auto", enabled: false, pendingEnabled: null, supported: true, appName: "ChatGPT", appVersion: "fixture", phase: "inactive", message: "", readyPages: 0, restoreAvailable: false });
  assert.equal((await f.apply()).enabled, false, "applying without a new choice keeps the actual state");
  await f.save(true);
  assert.equal((await f.apply()).enabled, true);
  await f.save(false); await f.apply();
  assert.equal((await loadFixture().get()).enabled, true, "language state is not persisted outside this fixture instance");
});

test("save and apply failures preserve state and normal mode permits retry", async () => {
  const f = loadFixture();
  await f.save(false);
  const original = await f.get();
  f.controls.setCodexLanguageMode("save-error");
  await assert.rejects(f.save(true), /保存失败/);
  assert.deepEqual(await f.get(), original);
  f.controls.setCodexLanguageMode("apply-error");
  await assert.rejects(f.apply(), /应用失败/);
  assert.deepEqual(await f.get(), original);
  f.controls.setCodexLanguageMode("normal");
  const result = await f.apply();
  assert.equal(result.enabled, false);
  assert.equal(result.pendingEnabled, null);
  assert.equal(f.controls.snapshot().codexLanguageMode, "normal");
});

test("external state controls are visible and change the next status response", async () => {
  const f = loadFixture();
  f.mount();
  const elements = [];
  const visit = (node) => { if (!node || typeof node !== "object") return; elements.push(node); for (const child of node.children ?? []) visit(child); };
  visit(f.body);
  const button = (label) => {
    const found = elements.find((node) => node.tagName === "button" && node.textContent === label);
    assert.ok(found, `missing visible control: ${label}`);
    return found;
  };
  button("Fixture：语言外部改为自动").click();
  assert.equal((await f.get()).locale, "auto");
  await f.save(true);
  button("Fixture：语言外部改为中文").click();
  assert.equal((await f.get()).enabled, true);
  assert.equal((await f.get()).pendingEnabled, null, "external replacement invalidates an old pending choice");
  button("Fixture：语言保存失败").click();
  await assert.rejects(f.save(false), /保存失败/);
  button("Fixture：语言应用失败").click();
  await assert.rejects(f.apply(), /应用失败/);
  button("Fixture：语言恢复正常").click();
  assert.equal((await f.save(false)).pendingEnabled, false);
  assert.equal((await f.apply()).locale, "auto");
});

test("invalid language arguments reject without mutating actual or pending state", async () => {
  const f = loadFixture();
  const original = await f.get();
  for (const args of [{ locale: "en-US", enabled: true }, { locale: "auto", enabled: true }, { locale: "zh-CN", enabled: "true" }]) {
    await assert.rejects(f.invoke("save_codex_language_preferences", args), /参数无效/);
    assert.deepEqual(await f.get(), original);
  }
  await assert.rejects(f.invoke("apply_codex_language_preferences", { restore: true }), /没有可恢复/);
  assert.deepEqual(await f.get(), original);
});
