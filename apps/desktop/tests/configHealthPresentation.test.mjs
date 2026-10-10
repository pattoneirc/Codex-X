import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";
import ts from "typescript";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { configHealthIssueLocation, primaryConfigHealthIssue, configHealthRepairUnavailableReason } from "../src/configHealthPresentation.ts";
import { configHealthIssueKey } from "../src/configHealthMonitor.ts";

const desktop = dirname(dirname(fileURLToPath(import.meta.url)));
const issue = {
  code: "provider-boolean-text", title: "供应商开关格式不正确", description: "supports_websockets 被写成了字符串。",
  repairable: true, path: "/fixtures/配置目录/config.toml", line: 7, column: 3,
  key: "model_providers.custom.supports_websockets", suggestion: "将 supports_websockets 改为 false（不带引号）。",
};
const report = {
  codexDir: "/fixtures/配置目录", fingerprint: "fixture-only", status: "issues", issues: [issue],
  canRepair: true, repairSummary: ["将写成文字的开关恢复为正确格式"], checkedAt: "2026-10-08T00:00:00Z",
};

let components;
async function ui() {
  if (components) return components;
  components = (async () => {
    const scratch = await mkdtemp(join(desktop, "tests/.config-health-ui-"));
    try {
      const emitted = new Map();
      async function compile(sourcePath) {
        if (emitted.has(sourcePath)) return emitted.get(sourcePath);
        const outputPath = join(scratch, relative(desktop, sourcePath).replace(/\.tsx?$/, ".mjs"));
        emitted.set(sourcePath, outputPath);
        const source = await readFile(sourcePath, "utf8");
        let output = ts.transpileModule(source, {
          fileName: sourcePath,
          compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX },
        }).outputText.replace(/import\s*["'][^"']+\.css["'];?/g, "");
        for (const match of [...output.matchAll(/\bfrom\s*["'](\.[^"']+)["']/g)]) {
          const specifier = match[1];
          const base = resolve(dirname(sourcePath), specifier);
          let dependency;
          for (const candidate of [`${base}.ts`, `${base}.tsx`, join(base, "index.ts")]) {
            try { await readFile(candidate, "utf8"); dependency = candidate; break; } catch { /* Try normal TS module resolution candidates. */ }
          }
          assert.ok(dependency, `could not resolve ${specifier} from ${sourcePath}`);
          const dependencyOutput = await compile(dependency);
          let replacement = relative(dirname(outputPath), dependencyOutput).replaceAll("\\", "/");
          if (!replacement.startsWith(".")) replacement = `./${replacement}`;
          output = output.replaceAll(`"${specifier}"`, `"${replacement}"`).replaceAll(`'${specifier}'`, `'${replacement}'`);
        }
        await mkdir(dirname(outputPath), { recursive: true });
        await writeFile(outputPath, output);
        return outputPath;
      }
      const panel = await compile(join(desktop, "src/components/ConfigHealthPanel.tsx"));
      const toast = await compile(join(desktop, "src/components/ConfigHealthToast.tsx"));
      return { ...await import(pathToFileURL(panel).href), ...await import(pathToFileURL(toast).href) };
    } finally { await rm(scratch, { recursive: true, force: true }); }
  })();
  return components;
}

const panelProps = { lang: "zh", report, checking: false, repairing: false, error: "", onCheck() {}, onRepair() {}, onOpenConfig() {} };
const toastProps = { lang: "zh", report, repairing: false, onRepair() {}, onDismiss() {}, onOpenSettings() {} };

test("exact path, original line and column, parameter, and suggestion render in panel and notice", async () => {
  const { ConfigHealthPanel, ConfigHealthToast } = await ui();
  for (const [Component, props] of [[ConfigHealthPanel, panelProps], [ConfigHealthToast, toastProps]]) {
    const html = renderToStaticMarkup(createElement(Component, props));
    for (const detail of [issue.path, "第 7 行，第 3 列", issue.key, issue.suggestion, "修复配置"]) assert.ok(html.includes(detail), detail);
    const repairButton = [...html.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(([button]) => button).find((button) => button.includes("修复配置"));
    assert.ok(repairButton);
    assert.doesNotMatch(repairButton, /\bdisabled=/);
  }
  assert.equal(configHealthIssueLocation(issue, report, "en").position, "line 7, column 3");
});

test("official history repair is actionable and a missing table never claims an invented source line", async () => {
  const missing = { ...issue, code: "session-provider-definition-missing", title: "旧会话使用的供应商缺少配置", line: null, column: null, key: "model_providers.old_proxy", suggestion: "新增 model_providers.old_proxy，使用当前已确认的 OpenAI 官方登录。" };
  const official = { ...report, issues: [missing], repairSummary: ["让旧会话沿用当前供应商「OpenAI」"] };
  const { ConfigHealthPanel, ConfigHealthToast } = await ui();
  for (const [Component, props] of [[ConfigHealthPanel, panelProps], [ConfigHealthToast, toastProps]]) {
    const html = renderToStaticMarkup(createElement(Component, { ...props, report: official }));
    assert.ok(html.includes("修复配置"));
    assert.ok(html.includes("未定义，尚无对应行"));
    assert.ok(html.includes("model_providers.old_proxy"));
    assert.ok(html.includes("OpenAI"));
    assert.ok(!html.includes("第 7 行"));
  }
});

test("unsupported values display their precise manual action and explain the unavailable repair button", async () => {
  const unsupported = { ...issue, repairable: false, key: "model_providers.custom.wire_api", suggestion: "先向供应商确认 Responses API 支持，再设置 wire_api = responses。" };
  const manual = { ...report, canRepair: false, repairSummary: [], issues: [unsupported] };
  const { ConfigHealthPanel, ConfigHealthToast } = await ui();
  const html = renderToStaticMarkup(createElement(ConfigHealthPanel, { ...panelProps, report: manual }));
  assert.ok(html.includes("需手动修复"));
  assert.ok(html.includes(unsupported.suggestion));
  assert.ok(html.includes(`title="${unsupported.suggestion}"`));
  const manualButton = [...html.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g)].map(([button]) => button).find((button) => button.includes("需手动修复"));
  assert.ok(manualButton);
  assert.match(manualButton, /\bdisabled=""/);
  const toast = renderToStaticMarkup(createElement(ConfigHealthToast, { ...toastProps, report: manual }));
  assert.ok(toast.includes(unsupported.key));
  assert.ok(toast.includes(unsupported.suggestion));
  assert.ok(toast.includes("查看设置"));
  assert.ok(!toast.includes("修复配置"));
});

test("a mixed report advertises a supported issue while the panel retains all manual diagnostics", async () => {
  const manual = { ...issue, repairable: false, title: "必须手动处理", key: "model_providers.custom.base_url" };
  const mixed = { ...report, issues: [manual, issue] };
  assert.equal(primaryConfigHealthIssue(mixed), issue);
  assert.equal(configHealthRepairUnavailableReason({ ...mixed, canRepair: false }, "zh"), manual.suggestion);
  const { ConfigHealthToast, ConfigHealthPanel } = await ui();
  const toast = renderToStaticMarkup(createElement(ConfigHealthToast, { ...toastProps, report: mixed }));
  assert.ok(toast.includes(issue.key));
  assert.ok(!toast.includes(manual.key));
  const panel = renderToStaticMarkup(createElement(ConfigHealthPanel, { ...panelProps, report: mixed }));
  assert.ok(panel.includes(manual.key));
  assert.ok(panel.includes(issue.key));
});

test("notification identity follows the parameter and repairability while ignoring source line shifts", () => {
  assert.equal(configHealthIssueKey(report), configHealthIssueKey({ ...report, issues: [{ ...issue, line: 80, column: 5 }] }));
  assert.notEqual(configHealthIssueKey(report), configHealthIssueKey({ ...report, issues: [{ ...issue, key: "model_providers.other.supports_websockets" }] }));
  assert.notEqual(configHealthIssueKey(report), configHealthIssueKey({ ...report, issues: [{ ...issue, repairable: false }] }));
});

test("file-level diagnostics and legacy reports use a usable path without claiming a line", () => {
  assert.deepEqual(configHealthIssueLocation({ ...issue, key: '"<文件编码>"', line: null, column: null }, report, "zh"), { path: issue.path, position: "文件层面", key: '"<文件编码>"' });
  const legacy = { code: "legacy", title: "Legacy", description: "Review", repairable: false };
  assert.equal(configHealthIssueLocation(legacy, { ...report, codexDir: "/fixtures/codex/" }, "en").path, "/fixtures/codex/config.toml");
});
