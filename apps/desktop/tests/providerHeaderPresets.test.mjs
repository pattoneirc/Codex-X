import assert from "node:assert/strict";
import test from "node:test";
import {
  PROVIDER_HEADER_PRESETS,
  applyProviderHeaderPreset,
  providerHeaderValuePlaceholder,
} from "../src/providerHeaderPresets.ts";
import { validateProviderHeaders } from "../src/providerHeaders.ts";

const row = (name, value = "", source = "static") => ({ name, value, source });

test("two explicit UA presets retain their reference values and bilingual labels", () => {
  assert.deepEqual(PROVIDER_HEADER_PRESETS.map(({ id, name, value, label }) => ({ id, name, value, label })), [
    { id: "claude_code", name: "User-Agent", value: "claude-cli/2.1.161 (external, cli)", label: { zh: "Claude Code", en: "Claude Code" } },
    { id: "kilo_code", name: "User-Agent", value: "Kilo-Code/1.0", label: { zh: "Kilo Code", en: "Kilo Code" } },
  ]);
});

test("selecting a preset on an empty configuration creates only one static UA header", () => {
  for (const preset of PROVIDER_HEADER_PRESETS) {
    const configured = applyProviderHeaderPreset([], preset.id);
    assert.deepEqual(configured, [row("User-Agent", preset.value)]);
    assert.equal(validateProviderHeaders(configured).valid, true);
  }
});

test("selecting the same preset repeatedly is idempotent and switching presets updates that row", () => {
  const first = applyProviderHeaderPreset([], "claude_code");
  const second = applyProviderHeaderPreset(first, "claude_code");
  assert.deepEqual(second, first);
  assert.deepEqual(applyProviderHeaderPreset(second, "kilo_code"), [row("User-Agent", "Kilo-Code/1.0")]);
  assert.equal(first[0].value, "claude-cli/2.1.161 (external, cli)");
});

test("an existing environment UA is updated case insensitively without changing other sensitive values", () => {
  const original = [
    row("Authorization", "  Bearer fixture-token  "),
    row(" uSeR-aGeNt ", "PROVIDER_UA", "env"),
    row("x-api-key", "  fixture-key\t"),
    row("X-Project", "PROJECT_ENV", "env"),
  ];
  const selected = applyProviderHeaderPreset(original, "claude_code");
  assert.deepEqual(selected, [original[0], row("User-Agent", "claude-cli/2.1.161 (external, cli)"), original[2], original[3]]);
  assert.equal(selected[0], original[0]);
  assert.equal(selected[2], original[2]);
  assert.equal(selected[3], original[3]);
  assert.deepEqual(original[1], row(" uSeR-aGeNt ", "PROVIDER_UA", "env"));
});

test("only the first UA is updated and duplicate names remain visible to validation", () => {
  const original = [row("", ""), row("USER-AGENT", "old-first"), row("user-agent", "SECOND_UA", "env"), row("X-Title", "App")];
  const selected = applyProviderHeaderPreset(original, "kilo_code");
  assert.equal(selected.length, original.length);
  assert.equal(selected[0], original[0]);
  assert.deepEqual(selected[1], row("User-Agent", "Kilo-Code/1.0"));
  assert.equal(selected[2], original[2]);
  assert.equal(selected[3], original[3]);
  const validation = validateProviderHeaders(selected);
  assert.equal(validation.valid, false);
  assert.ok(validation.errors[1].name.includes("Duplicate"));
  assert.ok(validation.errors[2].name.includes("Duplicate"));
});

test("a completely empty draft row is reused before appending a UA", () => {
  const original = [row("X-Title", "App"), row(" \t ", "", "env"), row("", "")];
  const selected = applyProviderHeaderPreset(original, "claude_code");
  assert.equal(selected.length, 3);
  assert.equal(selected[0], original[0]);
  assert.deepEqual(selected[1], row("User-Agent", "claude-cli/2.1.161 (external, cli)"));
  assert.equal(selected[2], original[2]);
  assert.equal(original[1].source, "env");
});

test("unfinished drafts containing values are preserved and a UA is appended", () => {
  const original = [row("", "unfinished secret"), row("  ", " "), row("X-Title", "App")];
  const selected = applyProviderHeaderPreset(original, "kilo_code");
  assert.deepEqual(selected, [...original, row("User-Agent", "Kilo-Code/1.0")]);
  for (let index = 0; index < original.length; index++) assert.equal(selected[index], original[index]);
});

test("unknown preset IDs are a no-op and never overwrite a blank draft", () => {
  const original = [row("Authorization", "  fixture-token  "), row("", "")];
  assert.deepEqual(applyProviderHeaderPreset(original, "unknown"), original);
  assert.equal(applyProviderHeaderPreset(original, "unknown")[0], original[0]);
  assert.deepEqual(applyProviderHeaderPreset([], ""), []);
});

test("literal placeholders explain each header's purpose without a generic Codex-X value", () => {
  for (const lang of ["zh", "en"]) {
    const names = ["User-Agent", "HTTP-Referer", "X-Title", "Authorization", "x-api-key", "X-Custom"];
    const hints = names.map((name) => providerHeaderValuePlaceholder(name, lang));
    assert.equal(new Set(hints).size, names.length);
    assert.ok(hints.every((hint) => hint && !hint.includes("Codex-X")));
    assert.ok(hints[0].includes("claude-cli/"));
    assert.ok(hints[1].includes("https://"));
    assert.ok(hints[3].includes("Bearer"));
    assert.ok(hints[4].includes("API key") || hints[4].includes("API Key"));
    assert.equal(providerHeaderValuePlaceholder("x-OpenRouter-Title", lang), hints[2]);
    assert.equal(providerHeaderValuePlaceholder("X-GOOG-API-KEY", lang), hints[4]);
    assert.equal(providerHeaderValuePlaceholder(" user-agent ", lang), hints[0]);
  }
});
