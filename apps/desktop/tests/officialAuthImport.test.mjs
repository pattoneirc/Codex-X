import assert from "node:assert/strict";
import test from "node:test";
import { MAX_AUTH_IMPORT_BYTES, parseOfficialAuthImport, readOfficialAuthImport } from "../src/officialAuthImport.ts";

const encoded = text => new TextEncoder().encode(text);
const auth = { auth_mode: "chatgpt", tokens: { access_token: "fixture-access", refresh_token: "fixture-refresh", account_id: "fixture-account" }, last_refresh: "2026-10-16", extra: { preserve: true } };

test("reads a standard auth file into a draft while preserving unknown fields", async () => {
  const original = JSON.stringify(auth, null, 2);
  const bytes = encoded(original);
  const result = await readOfficialAuthImport({ size: bytes.length, arrayBuffer: async () => bytes.buffer });
  assert.equal(result, original);
  assert.deepEqual(JSON.parse(result), auth);
});
test("accepts UTF-8 BOM and a standard OpenAI API key auth file", () => {
  assert.deepEqual(JSON.parse(parseOfficialAuthImport(encoded("\uFEFF" + JSON.stringify(auth)))), auth);
  assert.deepEqual(JSON.parse(parseOfficialAuthImport(encoded('{"OPENAI_API_KEY":"fixture-key"}'))), { OPENAI_API_KEY: "fixture-key" });
});
test("rejects oversized files before reading their contents", async () => {
  let reads = 0;
  await assert.rejects(readOfficialAuthImport({ size: MAX_AUTH_IMPORT_BYTES + 1, arrayBuffer: async () => { reads++; return new ArrayBuffer(0); } }), { code: "too_large" });
  assert.equal(reads, 0);
});
test("rejects invalid UTF-8 and JSON without echoing credential text", () => {
  assert.throws(() => parseOfficialAuthImport(new Uint8Array([0xff, 0xfe])), { code: "invalid_text" });
  assert.throws(() => parseOfficialAuthImport(encoded('{"tokens":"private-fixture-token')), error => error.code === "invalid_json" && !error.message.includes("private-fixture-token"));
});
test("does not treat arrays, third party configurations, or arbitrary JSON as official auth", () => {
  for (const value of [[], [auth], null, "x", {}, { accounts: [auth] }, { tokens: {} }, { OPENAI_API_KEY: " " }, { ...auth, base_url: "https://fixture.invalid/v1" }]) {
    assert.throws(() => parseOfficialAuthImport(encoded(JSON.stringify(value))), { code: "unsupported_shape" });
  }
});
test("checks actual decoded byte size even when a file supplies an inaccurate size", async () => {
  const bytes = new Uint8Array(MAX_AUTH_IMPORT_BYTES + 1);
  await assert.rejects(readOfficialAuthImport({ size: 1, arrayBuffer: async () => bytes.buffer }), { code: "too_large" });
});
