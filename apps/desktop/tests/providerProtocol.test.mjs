import assert from "node:assert/strict";
import test from "node:test";
import { providerUpstreamApi, providerRequiresConversion } from "../src/providerProtocol.ts";

test("all upstream choices are retained independently of the native wire API", () => {
  for (const upstreamApi of ["responses", "chat_completions", "anthropic_messages", "gemini"]) {
    assert.equal(providerUpstreamApi({ upstreamApi, wireApi: "responses" }), upstreamApi);
    assert.equal(providerRequiresConversion({ upstreamApi, wireApi: "responses" }), upstreamApi !== "responses");
  }
});

test("legacy chat and protocol aliases remain editable", () => {
  assert.equal(providerUpstreamApi({ wireApi: "chat" }), "chat_completions");
  assert.equal(providerUpstreamApi({ wireApi: "chat_completions" }), "chat_completions");
  assert.equal(providerUpstreamApi({ upstreamApi: "openai_chat" }), "chat_completions");
  assert.equal(providerUpstreamApi({ upstreamApi: "anthropic" }), "anthropic_messages");
  assert.equal(providerUpstreamApi({ upstreamApi: "gemini_native" }), "gemini");
});

test("an explicit native selection clears a legacy chat choice", () => {
  assert.equal(providerUpstreamApi({ wireApi: "chat", upstreamApi: "responses" }), "responses");
  assert.equal(providerRequiresConversion({ wireApi: "responses", upstreamApi: null }), false);
});

test("unknown metadata is retained so saving cannot silently switch protocols", () => {
  assert.equal(providerUpstreamApi({ upstreamApi: "unsupported", wireApi: "responses" }), "unsupported");
  assert.equal(providerUpstreamApi({ wireApi: "unknown_native" }), "unknown_native");
});
