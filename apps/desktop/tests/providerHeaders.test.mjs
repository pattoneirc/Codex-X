import assert from "node:assert/strict";
import test from "node:test";
import {
  createProviderHeader,
  findProviderHeader,
  normalizeProviderHeaders,
  removeProviderHeader,
  updateProviderHeader,
  validateProviderHeaders,
} from "../src/providerHeaders.ts";

const header = (name, value = "Codex-X", source = "static") => ({ name, value, source });

test("normalization retains original values and supports static/environment headers", () => {
  const rows = [header(" X-Title ", "  keep whitespace  "), header("X-Project", "PROJECT_KEY", "env")];
  assert.equal(validateProviderHeaders(rows).valid, true);
  assert.deepEqual(normalizeProviderHeaders(rows), [header("X-Title", "  keep whitespace  "), rows[1]]);
  assert.equal(findProviderHeader(rows, "x-title"), rows[0]);
  assert.equal(rows[0].name, " X-Title ");
});

test("duplicate names are rejected across sources and letter case", () => {
  const result = validateProviderHeaders([header("User-Agent"), header("user-agent", "CUSTOM_UA", "env")], "zh");
  assert.equal(result.valid, false);
  assert.ok(result.errors.every((error) => error.name.includes("重复")));
});

test("header names follow HTTP token syntax", () => {
  for (const name of ["", "bad name", "bad:name", "x\r\ninjected", "中文", "x\u007f"]) {
    assert.equal(validateProviderHeaders([header(name)]).valid, false, name);
  }
  assert.equal(validateProviderHeaders([header("!#$%&'*+-.^_`|~09AZaz")]).valid, true);
});

test("values reject CRLF and other control characters without exposing secrets in errors", () => {
  for (const value of ["secret\r\nInjected: yes", "secret\n", "secret\u0000", "secret\u000b", "secret\u007f"]) {
    const result = validateProviderHeaders([header("X-Test", value)]);
    assert.equal(result.valid, false);
    assert.equal(JSON.stringify(result.errors).includes("secret"), false);
  }
  assert.equal(validateProviderHeaders([header("X-Test", ""), header("X-Other", "text\t中文")]).valid, true);
});

test("environment sources store names, rejecting pasted values and invalid identifiers", () => {
  for (const value of ["", "9PROJECT", "PROJECT KEY", "PROJECT=secret", "secret\r\n", " key "]) {
    assert.equal(validateProviderHeaders([header("X-Project", value, "env")]).valid, false);
  }
  assert.equal(validateProviderHeaders([header("X-Project", "_PROJECT_9", "env")]).valid, true);
});

test("incomplete drafts and unsupported source types cannot be silently saved", () => {
  assert.equal(validateProviderHeaders([createProviderHeader()]).valid, false);
  assert.equal(validateProviderHeaders([header("X-Test", "value", "unsupported")]).valid, false);
  assert.throws(() => normalizeProviderHeaders([createProviderHeader()]), /Invalid provider headers/);
  assert.deepEqual(normalizeProviderHeaders([]), []);
});

test("row editing and removal preserve other rows without mutating the input", () => {
  const original = [header("X-Title"), header("X-Project", "PROJECT", "env")];
  const changed = updateProviderHeader(original, 0, { name: "User-Agent", value: "Agent" });
  assert.deepEqual(changed[0], header("User-Agent", "Agent"));
  assert.equal(changed[1], original[1]);
  assert.equal(original[0].name, "X-Title");
  assert.deepEqual(removeProviderHeader(changed, 0), [original[1]]);
  assert.deepEqual(createProviderHeader("env"), header("", "", "env"));
});
