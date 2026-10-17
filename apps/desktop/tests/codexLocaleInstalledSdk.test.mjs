import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { closeSync, existsSync, openSync, readFileSync, readSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";

// Optional local compatibility evidence. No installed application is launched,
// and no bundle is executed or copied into this repository. Only the narrow
// SDK registration / getter / compiled memo fragments are extracted at run time.
const archive = "/Applications/ChatGPT.app/Contents/Resources/app.asar";
const available = process.platform === "darwin" && existsSync(archive);
const runtime = readFileSync(new URL("../src-tauri/src/codex_locale_runtime.js", import.meta.url), "utf8");
const owner = "c".repeat(64);
const marker = "__CODEXX_LOCALE_RUNTIME_V1__";

function readInstalledEvidence() {
  const fd = openSync(archive, "r");
  try {
    const prefix = Buffer.alloc(16);
    readSync(fd, prefix, 0, prefix.length, 0);
    assert.equal(prefix.readUInt32LE(0), 4);
    const headerSize = prefix.readUInt32LE(12);
    assert.ok(headerSize > 0 && headerSize < 32 * 1024 * 1024);
    const header = Buffer.alloc(headerSize);
    assert.equal(readSync(fd, header, 0, header.length, 16), header.length);
    const assets = JSON.parse(header.toString()).files.webview.files.assets.files;
    const base = 8 + prefix.readUInt32LE(4);
    function asset(pattern) {
      const matches = Object.entries(assets).filter(([name]) => pattern.test(name));
      assert.equal(matches.length, 1, `installed asset shape changed: ${pattern}`);
      const [name, entry] = matches[0];
      assert.ok(!entry.unpacked && entry.size < 16 * 1024 * 1024);
      const bytes = Buffer.alloc(entry.size);
      assert.equal(readSync(fd, bytes, 0, bytes.length, base + Number(entry.offset)), bytes.length);
      return { name, source: bytes.toString(), sha256: createHash("sha256").update(bytes).digest("hex") };
    }
    const shared = asset(/^app-shared-.*\.js$/);
    // The language consumer's compiled memo is in app-initial, not app-shared.
    const initial = asset(/^app-initial-.*\.js$/);
    const plist = readFileSync("/Applications/ChatGPT.app/Contents/Info.plist", "utf8");
    const plistValue = (key) => plist.match(new RegExp(`<key>${key}</key>\\s*<string>([^<]+)`))?.[1];
    return { shared, initial, version: plistValue("CFBundleShortVersionString"), build: plistValue("CFBundleVersion") };
  } finally { closeSync(fd); }
}

function extractedFragments(evidence) {
  const require = createRequire(new URL("../package.json", import.meta.url));
  const ts = require("typescript");
  const shared = evidence.shared.source;
  function declaration(marker) {
    const index = shared.indexOf(marker);
    assert.ok(index >= 0, `installed SDK fragment changed: ${marker}`);
    // Parse a bounded window; execute only its first complete declaration.
    const parsed = ts.createSourceFile("fragment.js", shared.slice(index, index + 4096), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
    assert.ok(ts.isFunctionDeclaration(parsed.statements[0]));
    return parsed.statements[0].getText(parsed);
  }
  const registration = declaration("function p(e,t){if((0,s._isServerEnv)())return");
  const useLayer = declaration("function Nyn(");
  const readLayer = declaration("function Pyn(");
  const memoIndex = shared.indexOf("_memoize(e,t){");
  assert.ok(memoIndex >= 0);
  const parsedMemo = ts.createSourceFile("memo.js", `class Extracted {${shared.slice(memoIndex, memoIndex + 2048)}`, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const memoize = parsedMemo.statements[0].members[0].getText(parsedMemo);
  const assignmentStart = shared.indexOf("this.getLayer=this._memoize(");
  const assignmentEnd = shared.indexOf(",this.getParameterStore=", assignmentStart);
  assert.ok(assignmentStart >= 0 && assignmentEnd > assignmentStart);
  const assignment = shared.slice(assignmentStart, assignmentEnd);
  const derivedStart = shared.lastIndexOf("constructor(", assignmentStart);
  assert.ok(shared.slice(derivedStart, assignmentStart).includes("super("), "the real derived constructor calls super before assigning getLayer");
  const registerCall = shared.indexOf("p(e,this)");
  const baseStart = shared.lastIndexOf("constructor(", registerCall);
  assert.ok(baseStart > 0 && registerCall > baseStart);
  assert.equal(shared.slice(registerCall, registerCall + 10), "p(e,this)}", "registration is at the end of the real base constructor");
  const getterStart = shared.indexOf("e._getStatsigGlobal=()=>");
  const getterEnd = shared.indexOf(",e._getStatsigGlobalFlag", getterStart);
  const globalsStart = shared.indexOf("var n=`__STATSIG__`,", getterEnd);
  const globalsEnd = shared.indexOf("a[n]=o", globalsStart) + "a[n]=o".length;
  assert.ok(getterStart >= 0 && getterEnd > getterStart && globalsStart > getterEnd && globalsEnd > globalsStart);
  const globalInitialization = `${shared.slice(getterStart, getterEnd)};${shared.slice(globalsStart, globalsEnd)};`;
  const consumerStart = evidence.initial.source.indexOf("function O9s(");
  const expressionStart = evidence.initial.source.indexOf("t[0]===s?c=t[1]", consumerStart);
  const expressionEnd = evidence.initial.source.indexOf(";let l=c", expressionStart);
  assert.ok(consumerStart >= 0 && expressionStart > consumerStart && expressionEnd > expressionStart);
  const consumerMemo = evidence.initial.source.slice(expressionStart, expressionEnd);
  assert.ok(consumerMemo.includes("enable_i18n") && consumerMemo.length < 256);
  return { registration, useLayer, readLayer, memoize, assignment, globalInitialization, consumerMemo };
}

function isolatedSdk(fragments, installEarly) {
  const timers = new Map();
  const sandbox = {
    URL, location: { href: "app://-/index.html" }, document: { documentElement: { lang: "zh-CN" } },
    performance: { getEntriesByType: () => [{ name: "app://-/assets/zh-CN-fixture.js" }] },
    setInterval(callback) { timers.set(1, callback); return 1; }, clearInterval(id) { timers.delete(id); },
  };
  sandbox.window = sandbox; sandbox.top = sandbox;
  const context = vm.createContext(sandbox);
  const install = () => vm.runInContext(runtime.replace("__CODEXX_LANGUAGE_CONFIG__", JSON.stringify({ locale: "zh-CN", owner })), context, { timeout: 500 });
  if (installEarly) install();
  // These are independently supplied stubs, not live SDK stores, React, DOM,
  // networking, or account data. Extracted code has no bundle/module loader.
  vm.runInContext(`
    globalThis.fakeLayer={get:(key,fallback)=>key==='enable_i18n'?false:key==='locale_source'?'IDE':fallback};
    globalThis.originalCalls=0;
    const sdkGlobals=(()=>{const e={_getInstance(){}};${fragments.globalInitialization}return e;})();
    const s={_isServerEnv:()=>false},a={Log:{warn(){throw Error('unexpected duplicate fixture key')}}};
    const n={_getStatsigGlobal:sdkGlobals._getStatsigGlobal,MemoPrefix:{_layer:'fixture-layer'}};
    const o={createMemoKey:(...args)=>JSON.stringify(args)},f=1000;
    ${fragments.registration}
    class Base { constructor(e){this._options={};this._memoCache={};p(e,this)} ${fragments.memoize} }
    const originalMemoize=Base.prototype._memoize;
    Base.prototype._memoize=function(...args){const raw=originalMemoize.apply(this,args);this.fixtureOriginalGetLayer=raw;return raw;};
    class Derived extends Base { constructor(e){super(e);globalThis.registeredBeforeGetter=typeof this.getLayer!=='function';${fragments.assignment}} _getLayerImpl(){originalCalls++;return fakeLayer} }
    const hookCache=Array(8).fill(Symbol.for('fixture.uninitialized'));
    const consumerCache=Array(33).fill(Symbol.for('fixture.uninitialized'));
    const iW={c:()=>hookCache};
    function rW(){return {client:globalThis.client,getLayer:globalThis.client.getLayer}}
    ${fragments.readLayer}
    ${fragments.useLayer}
    globalThis.readCompiledLanguage=()=>{const s=Nyn('72216192');let t=consumerCache,c;${fragments.consumerMemo};return c;};
    globalThis.makeClient=key=>new Derived(key);
    globalThis.client=makeClient('fixture-key');
  `, context, { timeout: 500 });
  return { sandbox, context, timers, install, read: () => vm.runInContext("readCompiledLanguage()", context, { timeout: 500 }) };
}

test("installed SDK registration and compiled first language memo see the early override synchronously", { skip: !available }, (t) => {
  const evidence = readInstalledEvidence();
  t.diagnostic(`Installed build ${evidence.version} (${evidence.build}); ${evidence.shared.name}; SHA256 ${evidence.shared.sha256}`);
  const fragments = extractedFragments(evidence);
  const f = isolatedSdk(fragments, true);
  assert.equal(f.sandbox.registeredBeforeGetter, true);
  assert.equal(f.read(), true, "the first compiled memo sees true without timer ticks or SDK events");
  assert.equal(f.read(), true);
  assert.equal(f.sandbox.originalCalls, 1, "the real SDK and React memo keep the initial layer cached");
  assert.equal(f.sandbox.fakeLayer.get("enable_i18n", false), false);
  assert.equal(f.sandbox.client.getLayer("72216192").get("locale_source", "wrong"), "IDE");
  const registry = f.sandbox.__STATSIG__;
  f.sandbox.__STATSIG__ = registry;
  f.sandbox.second = vm.runInContext("makeClient('second-fixture-key')", f.context);
  assert.equal(f.sandbox.second.getLayer("72216192").get("enable_i18n", false), true, "subsequent SDK registrations remain observed");
  assert.equal(f.sandbox[marker].status().ready, true);
  f.sandbox[marker].restore();
  assert.equal(f.sandbox[marker], undefined);
  assert.equal(f.timers.size, 0);
  assert.equal(f.sandbox.__STATSIG__, registry);
  assert.equal(registry.instances["fixture-key"], f.sandbox.client);
  assert.equal(registry.instances["second-fixture-key"], f.sandbox.second);
  for (const [object, key] of [[f.sandbox, "__STATSIG__"], [registry, "firstInstance"], [registry, "instances"], [registry.instances, "fixture-key"], [registry.instances, "second-fixture-key"], [f.sandbox.client, "getLayer"], [f.sandbox.second, "getLayer"]]) {
    const descriptor = Object.getOwnPropertyDescriptor(object, key);
    assert.ok(descriptor && Object.hasOwn(descriptor, "value"), `restore removes the owned accessor for ${key}`);
    assert.equal(descriptor.writable, true);
    assert.equal(descriptor.configurable, true);
  }
  assert.equal(f.sandbox.client.getLayer, f.sandbox.client.fixtureOriginalGetLayer);
  assert.equal(f.sandbox.second.getLayer, f.sandbox.second.fixtureOriginalGetLayer);
  assert.equal(f.sandbox.client.getLayer("72216192").get("enable_i18n", false), false);
  assert.equal(f.sandbox.second.getLayer("72216192").get("enable_i18n", false), false);
});

test("installed compiled language memo demonstrates why a timer-only late hook cannot repair its first false value", { skip: !available }, () => {
  const f = isolatedSdk(extractedFragments(readInstalledEvidence()), false);
  assert.equal(f.read(), false);
  f.install();
  assert.equal(f.sandbox.client.getLayer("72216192").get("enable_i18n", false), true);
  assert.equal(f.read(), false, "the unchanged React client/layer memo retains the pre-hook false value");
  f.sandbox[marker].restore();
});
