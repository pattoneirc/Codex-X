import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import ts from "typescript";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { INITIAL_APP_UPDATER_STATE, appUpdaterCheckFailureMessage } from "../src/appUpdaterController.ts";

const desktop = dirname(dirname(fileURLToPath(import.meta.url)));
let component;
async function ui() {
  if (component) return component;
  component = (async () => {
    const scratch = await mkdtemp(join(desktop, "tests/.update-dialog-ui-"));
    try {
      // SSR cannot render a React DOM portal. Adapt only its container transport
      // while keeping the real UpdateDialog, ModalShell and Button components.
      const portal = join(scratch, "portal.mjs");
      await writeFile(portal, "export function createPortal(children) { return children; }\n");
      const emitted = new Map();
      async function compile(sourcePath) {
        if (emitted.has(sourcePath)) return emitted.get(sourcePath);
        const outputPath = join(scratch, relative(desktop, sourcePath).replace(/\.tsx?$/, ".mjs"));
        emitted.set(sourcePath, outputPath);
        let output = ts.transpileModule(await readFile(sourcePath, "utf8"), {
          fileName: sourcePath,
          compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX },
        }).outputText.replace(/import\s*["'][^"']+\.css["'];?/g, "");
        for (const match of [...output.matchAll(/\bfrom\s*["'](\.[^"']+)["']/g)]) {
          const specifier = match[1];
          const base = resolve(dirname(sourcePath), specifier);
          let dependency;
          for (const candidate of [`${base}.ts`, `${base}.tsx`, join(base, "index.ts")]) {
            try { await readFile(candidate, "utf8"); dependency = candidate; break; } catch { /* Normal TypeScript resolution candidates. */ }
          }
          assert.ok(dependency, `could not resolve ${specifier} from ${sourcePath}`);
          let replacement = relative(dirname(outputPath), await compile(dependency)).replaceAll("\\", "/");
          if (!replacement.startsWith(".")) replacement = `./${replacement}`;
          output = output.replaceAll(`"${specifier}"`, `"${replacement}"`).replaceAll(`'${specifier}'`, `'${replacement}'`);
        }
        if (sourcePath.endsWith(join("components", "ui", "ModalShell.tsx"))) {
          const portalImport = relative(dirname(outputPath), portal).replaceAll("\\", "/");
          assert.ok(output.includes('from "react-dom"'), "the only adapted dependency must be ModalShell's portal");
          output = output.replace('from "react-dom"', `from "${portalImport}"`);
        }
        await mkdir(dirname(outputPath), { recursive: true });
        await writeFile(outputPath, output);
        return outputPath;
      }
      const path = await compile(join(desktop, "src/components/AppDialogs.tsx"));
      return (await import(pathToFileURL(path).href)).UpdateDialog;
    } finally { await rm(scratch, { recursive: true, force: true }); }
  })();
  return component;
}

async function render(props) {
  const UpdateDialog = await ui();
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  // Effects, event handlers and Tauri IPC never run in renderToStaticMarkup.
  Object.defineProperty(globalThis, "document", { configurable: true, value: { body: {} } });
  try { return renderToStaticMarkup(createElement(UpdateDialog, props)); }
  finally {
    if (previousDocument) Object.defineProperty(globalThis, "document", previousDocument);
    else delete globalThis.document;
  }
}
function buttons(html) {
  return [...html.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(([button]) => button);
}
const noop = () => {};
const props = (lang) => ({ open: true, lang, currentVersion: "0.3.21", latestVersion: "0.3.24", onClose: noop, onDownload: noop });

for (const lang of ["zh", "en"]) {
  const zh = lang === "zh";
  test(`native check failure offers online retry as primary and manual download as fallback (${lang})`, async () => {
    const state = { ...INITIAL_APP_UPDATER_STATE, phase: "error", failure: "check", checkFailure: "network", errorMessage: appUpdaterCheckFailureMessage("network"), currentVersion: "0.3.21" };
    const html = await render({ ...props(lang), state, onRetry: noop, onUpdate: noop });
    assert.ok(html.includes('role="dialog"'));
    assert.ok(html.includes(zh ? "在线更新检查失败" : "Online update check failed"));
    assert.ok(html.includes(appUpdaterCheckFailureMessage("network", lang)));
    const retry = buttons(html).find((button) => button.includes(zh ? "重试在线检查" : "Retry online check"));
    assert.ok(retry);
    assert.ok(retry.includes("ui-button--primary"));
    const download = buttons(html).find((button) => button.includes(zh ? "打开下载页（备用）" : "Open download page (fallback)"));
    assert.ok(download);
    assert.ok(download.includes("ui-button--secondary"));
    assert.ok(!html.includes(zh ? "立即更新" : "Update now"), "a callback alone cannot expose installation after a failed check");
    assert.ok(!html.includes(zh ? "可前往下载页获取对应平台" : "A new version is available from the download page"));
  });

  test(`check error details remain fixed safe explanations even for an older error state (${lang})`, async () => {
    const state = { ...INITIAL_APP_UPDATER_STATE, phase: "error", failure: "check", checkFailure: undefined, errorMessage: "error at https://private.fixture.test?token=private-secret Authorization: Bearer private-secret" };
    const html = await render({ ...props(lang), state, onRetry: noop });
    assert.ok(html.includes(appUpdaterCheckFailureMessage("unknown", lang)));
    for (const secret of ["private-secret", "private.fixture.test", "Authorization", "Bearer"]) assert.ok(!html.includes(secret));
    assert.ok(html.includes('role="alert"'));
  });

  test(`portable manual-download dialog remains available without native updater state (${lang})`, async () => {
    const html = await render(props(lang));
    assert.ok(html.includes(zh ? "发现新版本" : "New version available"));
    assert.ok(html.includes(zh ? "检测到新版本，可前往下载页" : "A new version is available from the download page"));
    const download = buttons(html).find((button) => button.includes(zh ? "打开下载页" : "Open download page"));
    assert.ok(download?.includes("ui-button--primary"));
    assert.ok(!html.includes(zh ? "重试在线检查" : "Retry online check"));
    assert.ok(!html.includes(zh ? "立即更新" : "Update now"));
    assert.ok(!html.includes(zh ? "备用" : "fallback"));
    assert.ok(html.includes("0.3.21"));
    assert.ok(html.includes("0.3.24"));
  });

  test(`a successfully checked native update still offers installation (${lang})`, async () => {
    const state = { ...INITIAL_APP_UPDATER_STATE, phase: "available", currentVersion: "0.3.21", latestVersion: "0.3.24" };
    const html = await render({ ...props(lang), state, onUpdate: noop, onRetry: noop });
    const install = buttons(html).find((button) => button.includes(zh ? "立即更新" : "Update now"));
    assert.ok(install?.includes("ui-button--primary"));
    assert.ok(!html.includes(zh ? "在线更新检查失败" : "Online update check failed"));
  });

  test(`native retry uses its fresh version and cannot remain a stale fallback download dialog (${lang})`, async () => {
    const state = { ...INITIAL_APP_UPDATER_STATE, phase: "available", currentVersion: "0.3.21", latestVersion: "0.3.24" };
    const html = await render({ ...props(lang), latestVersion: "v0.3.23", state, onUpdate: noop, onRetry: noop });
    assert.ok(html.includes("0.3.24"));
    assert.ok(!html.includes("0.3.23"));
    assert.ok(buttons(html).some((button) => button.includes(zh ? "立即更新" : "Update now")));
  });

  test(`a native retry reporting no update renders up-to-date without installation or download-only actions (${lang})`, async () => {
    const state = { ...INITIAL_APP_UPDATER_STATE, phase: "idle", currentVersion: "0.3.24" };
    const html = await render({ ...props(lang), state, onUpdate: noop, onRetry: noop });
    assert.ok(html.includes(zh ? "当前已是最新版本" : "Codex-X is up to date"));
    assert.ok(!buttons(html).some((button) => button.includes(zh ? "立即更新" : "Update now")));
    assert.ok(!buttons(html).some((button) => button.includes(zh ? "打开下载页" : "Open download page")));
  });

  test(`download, verification, preparation, installation and restart errors keep existing copy (${lang})`, async () => {
    for (const failure of ["download", "verify", "prepare", "install", "restart"]) {
      const state = { ...INITIAL_APP_UPDATER_STATE, phase: "error", failure, errorMessage: "Fixture installation failure", logPath: "/fixtures/update.log" };
      const html = await render({ ...props(lang), state, onRetry: noop, onUpdate: noop });
      assert.ok(html.includes(zh ? "更新没有完成" : "Update did not finish"));
      assert.ok(html.includes("Fixture installation failure"));
      assert.ok(html.includes("/fixtures/update.log"));
      assert.ok(buttons(html).some((button) => button.includes(zh ? "重试" : "Try again")));
      assert.ok(buttons(html).some((button) => button.includes(zh ? "打开下载页" : "Open download page")));
      assert.ok(!html.includes(zh ? "重试在线检查" : "Retry online check"));
      assert.ok(!html.includes(zh ? "（备用）" : "(fallback)"));
      if (failure === "restart") assert.ok(html.includes(zh ? "软件未能重新启动" : "Codex-X could not restart"));
    }
  });
}
