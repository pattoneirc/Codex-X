/* Independently implemented locale compatibility. No application files change. */
(function (options) {
  "use strict";
  let page;
  try { page = new URL(location.href); } catch { return { installed: false, reason: "unsupported_origin" }; }
  if (page.protocol !== "app:" || page.hostname !== "-" || page.port || page.username || page.password ||
      !["/", "/index.html"].includes(page.pathname) || window !== window.top) {
    return { installed: false, reason: "unsupported_origin" };
  }
  if (!options || !["zh-CN", "en-US"].includes(options.locale) ||
      typeof options.owner !== "string" || !/^[a-f0-9]{64}$/.test(options.owner)) {
    return { installed: false, reason: "invalid_options" };
  }
  const marker = "__CODEXX_LOCALE_RUNTIME_V1__";
  if (window[marker]?.owner === options.owner) return window[marker].status();
  if (window[marker]) return { installed: false, reason: "another_owner" };
  const layerName = "72216192";
  const changes = [];
  const clients = new Map();
  const views = new WeakMap();
  const registries = new WeakSet();
  const observed = new WeakMap();
  let enabled = true;
  let timer;
  let messagesSeen = false;
  function isMessages(name) {
    try { const url = new URL(name); return url.protocol === "app:" && url.host === "-" && !url.username && !url.password && /^\/assets\/zh-CN-[a-zA-Z0-9_-]+\.js$/.test(url.pathname); }
    catch { return false; }
  }
  const resourceObserver = typeof PerformanceObserver === "function" ? new PerformanceObserver(list => {
    messagesSeen ||= list.getEntries().some(entry => isMessages(entry.name));
  }) : null;
  try { resourceObserver?.observe({ type: "resource", buffered: true }); } catch { /* optional API */ }

  function change(object, name, value) {
    const descriptor = Object.getOwnPropertyDescriptor(object, name);
    if (descriptor && !descriptor.configurable && !descriptor.writable) return false;
    try {
      Object.defineProperty(object, name, descriptor && !descriptor.configurable ? { value } : { configurable: true, writable: true, value });
      changes.push(() => {
        if (object[name] !== value) return;
        if (descriptor) Object.defineProperty(object, name, descriptor);
        else delete object[name];
      });
      return true;
    } catch { return false; }
  }
  function translatedLayer(layer) {
    if (!layer || typeof layer.get !== "function") return layer;
    if (views.has(layer)) return views.get(layer);
    const read = layer.get.bind(layer);
    const translatedRead = (parameter, ...args) =>
      enabled && parameter === "enable_i18n" ? true : read(parameter, ...args);
    const methods = new Map();
    // A forwarding facade also works for frozen SDK results. A proxy directly
    // wrapping a frozen own `get` would violate JavaScript proxy invariants.
    const view = new Proxy(Object.create(Object.getPrototypeOf(layer)), {
      get(_target, key) {
        if (key === "get") return translatedRead;
        const result = Reflect.get(layer, key, layer);
        if (typeof result !== "function") return result;
        if (methods.get(key)?.original !== result) methods.set(key, { original: result, bound: result.bind(layer) });
        return methods.get(key).bound;
      },
      has(_target, key) { return key in layer; },
      ownKeys() { return Reflect.ownKeys(layer); },
      getOwnPropertyDescriptor(_target, key) {
        const descriptor = Object.getOwnPropertyDescriptor(layer, key);
        return descriptor ? { ...descriptor, configurable: true } : undefined;
      },
    });
    views.set(layer, view);
    return view;
  }
  function hook(client) {
    if (!enabled) return;
    if (!client || typeof client !== "object") return;
    const previous = clients.get(client);
    if (previous?.waiting) return;
    if (previous && previous.replacement === client.getLayer) return;
    // The SDK registers its client from the base constructor, before the
    // derived constructor assigns getLayer. Intercept that later assignment so
    // React's first memoized language evaluation already sees the override.
    if (typeof client.getLayer !== "function") {
      const descriptor = Object.getOwnPropertyDescriptor(client, "getLayer");
      if (descriptor && !descriptor.configurable) return;
      let raw;
      let wrapped;
      const getter = () => wrapped;
      const setter = next => {
        raw = next;
        wrapped = typeof next === "function" ? wrap(client, next) : next;
        clients.set(client, { replacement: wrapped, waiting: false });
      };
      Object.defineProperty(client, "getLayer", { configurable: true, enumerable: descriptor?.enumerable ?? true, get: getter, set: setter });
      clients.set(client, { waiting: true });
      changes.push(() => {
        if (Object.getOwnPropertyDescriptor(client, "getLayer")?.get !== getter) return;
        if (descriptor && Object.hasOwn(descriptor, "value")) Object.defineProperty(client, "getLayer", { ...descriptor, value: raw });
        else if (descriptor) Object.defineProperty(client, "getLayer", descriptor);
        else if (raw === undefined) delete client.getLayer;
        else Object.defineProperty(client, "getLayer", { configurable: true, enumerable: true, writable: true, value: raw });
      });
      return;
    }
    const original = client.getLayer;
    const replacement = wrap(client, original);
    if (change(client, "getLayer", replacement)) {
      clients.set(client, { replacement });
      try { client.$emt?.({ name: "values_updated" }); } catch { /* optional SDK hook */ }
    }
  }
  function wrap(_client, original) {
    return function (name, ...args) {
      const result = original.call(this, name, ...args);
      return enabled && name === layerName ? translatedLayer(result) : result;
    };
  }
  function observe(object, key, receive) {
    let keys = observed.get(object);
    if (keys?.has(key)) { receive(object[key]); return; }
    const descriptor = Object.getOwnPropertyDescriptor(object, key);
    if (descriptor && !descriptor.configurable) { receive(object[key]); return; }
    if (!keys) { keys = new Set(); observed.set(object, keys); }
    keys.add(key);
    let current = object[key];
    const getter = () => current;
    const setter = next => {
      if (descriptor?.set) descriptor.set.call(object, next);
      current = descriptor?.get ? descriptor.get.call(object) : next;
      if (enabled) receive(current);
    };
    Object.defineProperty(object, key, { configurable: true, enumerable: descriptor?.enumerable ?? true, get: getter, set: setter });
    changes.push(() => {
      if (Object.getOwnPropertyDescriptor(object, key)?.get !== getter) return;
      if (descriptor && !Object.hasOwn(descriptor, "value")) Object.defineProperty(object, key, descriptor);
      else if (descriptor) Object.defineProperty(object, key, { ...descriptor, value: current });
      else if (current === undefined) delete object[key];
      else Object.defineProperty(object, key, { configurable: true, enumerable: true, writable: true, value: current });
    });
    receive(current);
  }
  function registryAppeared(registry) {
    if (!enabled || !registry || typeof registry !== "object" || registries.has(registry)) return;
    registries.add(registry);
    observe(registry, "firstInstance", hook);
    observe(registry, "instances", instances => {
      if (!instances || typeof instances !== "object") return;
      // Observe SDK registration without changing the dictionary's identity.
      for (const [key, client] of Object.entries(instances).slice(0, 16)) {
        hook(client);
        observe(instances, key, hook);
      }
    });
  }
  function scan() {
    const current = new Set();
    if (!enabled) return current;
    const registry = window.__STATSIG__;
    if (!registry || typeof registry !== "object") return current;
    if (registry.firstInstance) { hook(registry.firstInstance); current.add(registry.firstInstance); }
    if (registry.instances && typeof registry.instances === "object") {
      for (const client of Object.values(registry.instances).slice(0, 16)) { hook(client); current.add(client); }
    }
    return current;
  }
  const state = {
    owner: options.owner,
    status() {
      const current = scan();
      let gate = false;
      for (const client of current) {
        try { gate ||= client.getLayer(layerName, { disableExposureLog: true }).get("enable_i18n", false) === true; }
        catch { /* unsupported client instance */ }
      }
      const language = document.documentElement?.lang || "";
      const normalized = String(language).replace(/_/g, "-").toLowerCase();
      const locale = options.locale === "zh-CN" ? ["zh-cn", "zh-hans", "zh-hans-cn"].includes(normalized) : /^en(?:-|$)/i.test(normalized);
      const messages = options.locale === "en-US" || messagesSeen || performance.getEntriesByType("resource").some(entry => isMessages(entry.name));
      return { installed: enabled, ready: enabled && gate && locale && messages,
        locale: options.locale, clients: current.size, gate, languageMatches: locale, messagesLoaded: messages };
    },
    restore() {
      enabled = false;
      clearInterval(timer);
      resourceObserver?.disconnect();
      for (const undo of changes.reverse()) { try { undo(); } catch { /* preserve later changes */ } }
      if (window[marker] === state) delete window[marker];
      return { restored: true };
    },
  };
  Object.defineProperty(window, marker, { configurable: true, value: state });
  observe(window, "__STATSIG__", registryAppeared);
  scan();
  timer = setInterval(scan, 100);
  return state.status();
})(__CODEXX_LANGUAGE_CONFIG__)
