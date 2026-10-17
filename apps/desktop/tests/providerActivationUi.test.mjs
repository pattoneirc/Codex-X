import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import ts from "typescript";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const desktop = dirname(dirname(fileURLToPath(import.meta.url)));
let page;
async function loadPage() {
  if (page) return page;
  page = (async () => {
    const scratch = await mkdtemp(join(desktop, "tests/.provider-activation-ui-"));
    try {
      const emitted = new Map();
      async function compile(sourcePath) {
        if (emitted.has(sourcePath)) return emitted.get(sourcePath);
        const outputPath = join(scratch, relative(desktop, sourcePath).replace(/\.tsx?$/, ".mjs"));
        emitted.set(sourcePath, outputPath);
        const source = await readFile(sourcePath, "utf8");
        // Animation is unrelated to action locks and has browser layout effects.
        let output = sourcePath.endsWith("/PageTransition.tsx") ? "export function PageTransition({ children }) { return children; }" : ts.transpileModule(source, {
          fileName: sourcePath,
          compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX },
        }).outputText.replace(/import\s*["'][^"']+\.css["'];?/g, "");
        for (const match of [...output.matchAll(/\bfrom\s*["'](\.[^"']+)["']/g)]) {
          const specifier = match[1];
          const base = resolve(dirname(sourcePath), specifier);
          let dependency;
          for (const candidate of [`${base}.ts`, `${base}.tsx`, join(base, "index.ts")]) {
            try { await readFile(candidate, "utf8"); dependency = candidate; break; } catch { /* Try the next TS module candidate. */ }
          }
          assert.ok(dependency, `could not resolve ${specifier}`);
          const dependencyOutput = await compile(dependency);
          let replacement = relative(dirname(outputPath), dependencyOutput).replaceAll("\\", "/");
          if (!replacement.startsWith(".")) replacement = `./${replacement}`;
          output = output.replaceAll(`"${specifier}"`, `"${replacement}"`).replaceAll(`'${specifier}'`, `'${replacement}'`);
        }
        await mkdir(dirname(outputPath), { recursive: true });
        await writeFile(outputPath, output);
        return outputPath;
      }
      return (await import(pathToFileURL(await compile(join(desktop, "src/pages/ProvidersPage.tsx"))).href)).ProvidersPage;
    } finally { await rm(scratch, { recursive: true, force: true }); }
  })();
  return page;
}

const baseRow = { baseUrl: "https://fixture.example.test/v1", model: "fixture-model", wireApi: "responses", requiresOpenaiAuth: false, editable: true, duplicable: true, deletable: true, testable: true };
const copy = new Proxy({ enableLabel: "Enable fixture", addLabel: "Add fixture", importLabel: "Import fixture" }, { get: (target, key) => target[key] ?? String(key) });
const props = {
  mode: "list", lang: "en", configDir: "/fixture/codex", copy, loading: true, actionBusy: "syncSessions",
  providerRows: [
    { ...baseRow, id: "current", source: "local", providerName: "Current fixture", isCurrent: true },
    { ...baseRow, id: "next", source: "local", providerName: "Next fixture", isCurrent: false },
    { ...baseRow, id: "official-next", source: "official", providerName: "Official fixture", isCurrent: false },
  ],
  onImportCcSwitch() {}, onAddProvider() {}, onEnableProvider() {}, onTestProvider() {}, onEditProvider() {}, onDuplicateProvider() {}, onDeleteProvider() {}, onReorderProviders() {},
};
const buttons = (html, label) => [...html.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(([button]) => button).filter((button) => button.includes(label));

test("sync permits switching local and official providers while other provider actions stay disabled", async () => {
  const ProvidersPage = await loadPage();
  const html = renderToStaticMarkup(createElement(ProvidersPage, { ...props, activationBusy: false }));
  const enable = buttons(html, "Enable fixture");
  assert.equal(enable.length, 3);
  assert.match(enable[0], /\bdisabled=/, "the current provider is never activated again");
  assert.doesNotMatch(enable[1], /\bdisabled=/, "another API provider can be enabled during sync");
  assert.doesNotMatch(enable[2], /\bdisabled=/, "an official profile can be enabled during sync");
  for (const label of ["Add fixture", "Import fixture"]) assert.match(buttons(html, label)[0], /\bdisabled=/);
  assert.match(buttons(html, "editLabel")[0], /\bdisabled=/);
  assert.match(buttons(html, "duplicateLabel")[0], /\bdisabled=/);
});

test("a real activation or unrelated operation keeps every activation button locked", async () => {
  const ProvidersPage = await loadPage();
  for (const extra of [{ activationBusy: true }, {}]) {
    const html = renderToStaticMarkup(createElement(ProvidersPage, { ...props, ...extra }));
    assert.ok(buttons(html, "Enable fixture").every((button) => /\bdisabled=/.test(button)));
  }
});
