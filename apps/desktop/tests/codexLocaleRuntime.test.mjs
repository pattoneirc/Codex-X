import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../src-tauri/src/codex_locale_runtime.js", import.meta.url), "utf8");
const marker = "__CODEXX_LOCALE_RUNTIME_V1__";
const layerName = "72216192";
const owner = "a".repeat(64);
const languageResource = "app://-/assets/zh-CN-fixture.js";

class FixtureLayer {
  #values;
  constructor(values = {}) { this.#values = { enable_i18n: false, locale_source: "fixture-original", ...values }; this.name = "fixture layer"; }
  get(parameter, fallback) { return Object.hasOwn(this.#values, parameter) ? this.#values[parameter] : fallback; }
  readPrivate() { return this.#values.locale_source; }
  get privateValue() { return this.#values.locale_source; }
}
class FixtureClient {
  #layers;
  constructor(layer = new FixtureLayer()) {
    this.#layers = { [layerName]: layer, other_layer: new FixtureLayer({ enable_i18n: false }) };
    this.calls = [];
    this.events = [];
    this.gateCalls = [];
  }
  getLayer(name, ...options) { this.calls.push({ name, options, receiver: this }); return this.#layers[name]; }
  checkGate(name) { this.gateCalls.push(name); return this.#layers.other_layer.get("enable_i18n", false); }
  $emt(event) { this.events.push(event.name); this.onValuesUpdated?.(event); }
}

function fixture({ url = "app://-/index.html", language = "zh-CN", resources = [languageResource], client = new FixtureClient(), registry, iframe = false, observer = true, documentElement = true } = {}) {
  const parsed = new URL(url);
  // Location deliberately has no username/password fields: browsers expose
  // those through new URL(location.href), not through the Location interface.
  const location = { href: parsed.href, protocol: parsed.protocol, host: parsed.host, hostname: parsed.hostname, port: parsed.port, pathname: parsed.pathname, search: parsed.search, hash: parsed.hash, origin: parsed.origin };
  const timers = new Map(), scheduled = [], observers = [];
  let entries = resources.map((name) => ({ name, initiatorType: "script" }));
  const sandbox = {
    location, URL,
    document: { documentElement: documentElement ? { lang: language } : null },
    performance: { getEntriesByType(type) { assert.equal(type, "resource"); return entries; } },
    setInterval(callback, interval) { assert.equal(interval, 100); const id = scheduled.length + 1; scheduled.push(callback); timers.set(id, callback); return id; },
    clearInterval(id) { timers.delete(id); },
  };
  if (observer) sandbox.PerformanceObserver = class {
    constructor(callback) { this.callback = callback; this.active = false; observers.push(this); }
    observe(options) { assert.equal(options.type, "resource"); this.active = true; if (options.buffered) this.callback({ getEntries: () => entries }); }
    disconnect() { this.active = false; }
  };
  sandbox.window = sandbox;
  sandbox.top = iframe ? {} : sandbox;
  sandbox.__STATSIG__ = registry ?? { firstInstance: client, instances: { fixture: client } };
  const context = vm.createContext(sandbox);
  return {
    sandbox, client, timers, observers, scheduled,
    install(options = { locale: "zh-CN", owner }) { return vm.runInContext(source.replace("__CODEXX_LANGUAGE_CONFIG__", JSON.stringify(options)), context, { timeout: 500 }); },
    state() { return sandbox[marker]; },
    tick() { for (const callback of [...timers.values()]) callback(); },
    emitResources(names) { const delivered = names.map((name) => ({ name, initiatorType: "script" })); for (const current of observers) if (current.active) current.callback({ getEntries: () => delivered }); },
    clearResources() { entries = []; },
  };
}

test("only the i18n parameter of the named layer changes; other gates, fields and receivers remain original", () => {
  const client = new FixtureClient();
  const layer = client.getLayer(layerName);
  const other = client.getLayer("other_layer");
  client.calls.length = 0;
  const checkGate = client.checkGate;
  const f = fixture({ client });
  assert.equal(f.install().ready, true);
  assert.ok(client.calls.every((call) => call.name === layerName && call.receiver === client));
  assert.deepEqual(client.gateCalls, [], "the runtime never evaluates unrelated feature gates");
  assert.equal(client.checkGate, checkGate);
  assert.equal(client.checkGate("unrelated-account-gate"), false);
  assert.equal(client.getLayer("other_layer"), other);
  const view = client.getLayer(layerName);
  assert.equal(view.get("enable_i18n", false), true);
  assert.equal(view.get("locale_source", "wrong"), "fixture-original");
  assert.equal(view.get("unrelated-field", "original fallback"), "original fallback");
  assert.equal(view.readPrivate(), "fixture-original");
  assert.equal(view.privateValue, "fixture-original");
  assert.equal(view.get, view.get, "the i18n getter has a stable reference for React dependencies");
  assert.equal(view.readPrivate, view.readPrivate, "other bound methods have stable references");
  const previousBoundMethod = view.readPrivate;
  layer.readPrivate = function () { return `${this.privateValue}-updated`; };
  assert.notEqual(view.readPrivate, previousBoundMethod, "a later SDK method replacement is observed");
  assert.equal(view.readPrivate(), "fixture-original-updated");
  assert.equal(layer.get("enable_i18n", false), false, "the actual SDK layer data is not mutated");
  assert.equal(Object.getPrototypeOf(view), Object.getPrototypeOf(layer));
  f.state().restore();
});

test("frozen layers with immutable own get methods do not violate Proxy invariants", () => {
  const layer = new FixtureLayer();
  Object.defineProperty(layer, "get", { value: layer.get, configurable: false, writable: false, enumerable: true });
  Object.freeze(layer);
  const f = fixture({ client: new FixtureClient(layer) });
  assert.equal(f.install().ready, true);
  const view = f.client.getLayer(layerName);
  assert.equal(view.get("enable_i18n", false), true);
  assert.equal(view.readPrivate(), "fixture-original");
  assert.equal(view.privateValue, "fixture-original");
  assert.equal("name" in view, true);
  assert.deepEqual(Object.keys(view), Object.keys(layer));
  f.state().restore();
  assert.equal(view.get("enable_i18n", false), false, "held views stop overriding after restoration");
});

test("writable nonconfigurable methods and configurable accessors restore their exact descriptors", () => {
  for (const accessor of [false, true]) {
    const client = new FixtureClient();
    const original = client.getLayer;
    Object.defineProperty(client, "getLayer", accessor
      ? { configurable: true, enumerable: true, get() { return original; } }
      : { configurable: false, enumerable: true, writable: true, value: original });
    const descriptor = Object.getOwnPropertyDescriptor(client, "getLayer");
    const f = fixture({ client });
    assert.equal(f.install().ready, true);
    f.state().restore();
    assert.deepEqual(Object.getOwnPropertyDescriptor(client, "getLayer"), descriptor);
  }
});

test("unpatchable client methods fail closed without reporting the compatibility layer ready", () => {
  const client = new FixtureClient();
  const original = client.getLayer;
  Object.defineProperty(client, "getLayer", { configurable: false, writable: false, value: original });
  const f = fixture({ client });
  assert.equal(f.install().ready, false);
  assert.equal(client.getLayer, original);
  assert.deepEqual(client.events, []);
  f.state().restore();
});

test("only the top-level official app origin and supported page paths are accepted", () => {
  for (const url of ["https://chatgpt.com/", "http://127.0.0.1/index.html", "app://other/index.html", "app://-/browser.html", "app://user@-/index.html", "app://user:password@-/index.html", "app://-:123/index.html"]) {
    const f = fixture({ url });
    assert.equal(f.install().reason, "unsupported_origin", url);
    assert.equal(f.state(), undefined);
    assert.equal(f.timers.size, 0);
    assert.deepEqual(f.client.events, []);
  }
  assert.equal(fixture({ iframe: true }).install().reason, "unsupported_origin");
  for (const url of ["app://-/", "app://-/index.html?initialRoute=%2Fsettings#fixture"]) {
    const f = fixture({ url });
    assert.equal(f.install().installed, true);
    f.state().restore();
  }
});

test("unsupported locales and untrusted option shapes cause no hooks", () => {
  for (const options of [null, {}, { locale: "fr-FR", owner }, { locale: "zh-CN", owner: "not-a-session-id" }, { locale: "zh-CN", owner: '";globalThis.fixtureInjected=true;//' }]) {
    const f = fixture();
    assert.equal(f.install(options).reason, "invalid_options");
    assert.equal(f.state(), undefined);
    assert.equal(f.sandbox.fixtureInjected, undefined);
    assert.deepEqual(f.client.events, []);
  }
});

test("a late SDK singleton notifies subscribers after the narrow hook becomes available", () => {
  const f = fixture({ registry: {} });
  assert.equal(f.install().ready, false);
  const observed = [];
  f.client.onValuesUpdated = () => observed.push(f.client.getLayer(layerName).get("enable_i18n", false));
  f.sandbox.__STATSIG__ = { firstInstance: f.client, instances: { same: f.client } };
  f.tick();
  assert.equal(f.state().status().ready, true);
  assert.deepEqual(observed, [true]);
  assert.deepEqual(f.client.events, ["values_updated"]);
  f.state().restore();
});

test("early registry and base-constructor registration intercept a later getter assignment before the first read", () => {
  const f = fixture({ registry: {} });
  delete f.sandbox.__STATSIG__;
  assert.equal(f.install().ready, false);
  const registry = {};
  f.sandbox.__STATSIG__ = registry;
  const rawLayer = new FixtureLayer();
  const original = function () { assert.equal(this, client); return rawLayer; };
  let missingDuringBase = false;
  class Base {
    constructor() {
      const instances = registry.instances ?? {};
      instances.fixture = this;
      registry.firstInstance ||= this;
      registry.instances = instances;
      f.sandbox.__STATSIG__ = registry;
      missingDuringBase = typeof this.getLayer !== "function";
    }
  }
  class Derived extends Base {
    constructor() { super(); this.getLayer = original; }
  }
  const client = new Derived();
  assert.equal(missingDuringBase, true);
  assert.equal(client.getLayer(layerName).get("enable_i18n", false), true, "no timer tick or SDK event is needed");
  assert.equal(client.getLayer(layerName).get("locale_source", "wrong"), "fixture-original");
  assert.equal(rawLayer.get("enable_i18n", false), false);
  const laterSdkGetter = function () { return rawLayer; };
  client.getLayer = laterSdkGetter;
  assert.equal(client.getLayer(layerName).get("enable_i18n", false), true);
  f.state().restore();
  assert.equal(client.getLayer, laterSdkGetter, "restoration preserves the latest SDK-assigned method exactly");
  assert.equal(f.sandbox.__STATSIG__, registry);
  assert.equal(registry.instances.fixture, client);
  for (const [object, key] of [[f.sandbox, "__STATSIG__"], [registry, "firstInstance"], [registry, "instances"], [registry.instances, "fixture"], [client, "getLayer"]]) {
    assert.ok(Object.hasOwn(Object.getOwnPropertyDescriptor(object, key), "value"), `${key} is no longer an owned accessor`);
  }
});

test("repeated injection and duplicate registry references never stack hooks or pollers", () => {
  const f = fixture();
  f.install();
  const replacement = f.client.getLayer;
  f.install(); f.tick(); f.install();
  assert.equal(f.client.getLayer, replacement);
  assert.equal(f.timers.size, 1);
  assert.equal(f.observers.length, 1);
  assert.deepEqual(f.client.events, ["values_updated"]);
  assert.equal(f.install({ locale: "zh-CN", owner: "b".repeat(64) }).reason, "another_owner");
  assert.equal(f.client.getLayer, replacement);
  f.state().restore();
});

test("restoration removes inherited overrides and old handles or queued scans cannot rehook", () => {
  const f = fixture();
  const original = f.client.getLayer;
  f.install();
  const state = f.state();
  const heldView = f.client.getLayer(layerName);
  const queuedScan = f.scheduled[0];
  state.restore();
  assert.equal(f.state(), undefined);
  assert.equal(f.timers.size, 0);
  assert.ok(f.observers.every((observer) => !observer.active));
  assert.equal(Object.hasOwn(f.client, "getLayer"), false);
  assert.equal(f.client.getLayer, original);
  assert.equal(heldView.get("enable_i18n", false), false);
  const events = f.client.events.length;
  assert.equal(state.status().installed, false);
  queuedScan(); state.restore();
  assert.equal(f.client.getLayer, original);
  assert.equal(Object.hasOwn(f.client, "getLayer"), false);
  assert.equal(f.client.events.length, events);
});

test("restoration preserves later SDK method replacements", () => {
  const f = fixture();
  const original = f.client.getLayer;
  f.install();
  const external = function (name, ...args) { return original.call(this, name, ...args); };
  f.client.getLayer = external;
  f.tick(); // The SDK replaced its method while compatibility was active.
  f.state().restore();
  assert.equal(f.client.getLayer, external);
});

test("readiness follows only current SDK registry clients, not past account instances", () => {
  const f = fixture();
  const old = f.client;
  const original = old.getLayer;
  f.install();
  const next = new FixtureClient();
  f.sandbox.__STATSIG__ = { firstInstance: next, instances: { same: next } };
  f.tick();
  assert.equal(f.state().status().clients, 1);
  assert.equal(f.state().status().ready, true);
  f.sandbox.__STATSIG__ = {};
  assert.equal(f.state().status().ready, false);
  assert.equal(f.state().status().clients, 0);
  f.state().restore();
  assert.equal(old.getLayer, original);
  assert.equal(Object.hasOwn(next, "getLayer"), false);
});

test("readiness requires the selected language and the official bundled translation resource", () => {
  for (const resources of [[], ["https://other.example/assets/zh-CN-fixture.js"], ["app://other/assets/zh-CN-fixture.js"], ["app://-/different/zh-CN-fixture.js"], ["app://user@-/assets/zh-CN-fixture.js"]]) {
    const f = fixture({ resources });
    assert.equal(f.install().ready, false);
    f.state().restore();
  }
  for (const language of ["zh-TW", "zh-HK", "en-US"]) {
    const f = fixture({ language });
    assert.equal(f.install().ready, false);
    f.state().restore();
  }
  const english = fixture({ language: "en-US", resources: [] });
  assert.equal(english.install({ locale: "en-US", owner }).ready, true);
  english.state().restore();
});

test("observed language resources survive resource-timing buffer eviction", () => {
  const f = fixture({ resources: [] });
  assert.equal(f.install().ready, false);
  f.emitResources([languageResource]);
  f.clearResources();
  assert.equal(f.state().status().ready, true);
  f.state().restore();
});

test("early documents without a documentElement remain unready until the language is present", () => {
  const f = fixture({ documentElement: false });
  assert.equal(f.install().ready, false);
  f.sandbox.document.documentElement = { lang: "zh-CN" };
  assert.equal(f.state().status().ready, true);
  f.state().restore();
});

test("the runtime does not access account data, messages, credential storage or network APIs", () => {
  const f = fixture();
  for (const [object, names] of [[f.sandbox, ["fetch", "XMLHttpRequest", "localStorage", "sessionStorage"]], [f.client, ["user", "apiKey", "messages", "account"]], [f.sandbox.document, ["body", "cookie"]]]) {
    for (const name of names) Object.defineProperty(object, name, { configurable: true, get() { assert.fail(`runtime accessed forbidden fixture field ${name}`); } });
  }
  assert.equal(f.install().ready, true);
  assert.equal(f.state().status().ready, true);
  f.tick();
  f.state().restore();
});
