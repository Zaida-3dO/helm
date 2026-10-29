import assert from "node:assert/strict";
import { test } from "node:test";
import { startFakeLocalModel } from "./server.ts";

const KEY = "fake-local-key";

async function call(base: string, method: string, path: string, body?: unknown, headers: Record<string, string> = {}) {
  const r = await fetch(`${base}${path}`, {
    method,
    headers: { "content-type": "application/json", ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  return { status: r.status, text, json: () => JSON.parse(text) as any };
}

/** The text and thinking a client would assemble from a streamed message, and the event order. */
function readSse(text: string) {
  const events = text.split("\n\n").filter(Boolean).map((chunk) => JSON.parse(/^data: (.*)$/m.exec(chunk)![1]!) as any);
  let answer = "";
  let thinking = "";
  let toolInput = "";
  for (const e of events) {
    if (e.type !== "content_block_delta") continue;
    if (e.delta.type === "text_delta") answer += e.delta.text;
    if (e.delta.type === "thinking_delta") thinking += e.delta.thinking;
    if (e.delta.type === "input_json_delta") toolInput += e.delta.partial_json;
  }
  return { types: events.map((e) => e.type as string), answer, thinking, toolInput, stop: events.find((e) => e.type === "message_delta")?.delta.stop_reason as string };
}

test("one server answers the Anthropic, Strata and Ollama routes, keeps loaded state, and logs auth headers", async () => {
  const fake = await startFakeLocalModel({ key: KEY, loadDelayMs: 60, models: [{ name: "fake-flash" }, { name: "org/fake:3b", contextTokens: 8192 }], reply: "pong" });
  try {
    const bearer = { authorization: `Bearer ${KEY}` };
    const messages = { model: "fake-flash", max_tokens: 64, stream: true, messages: [{ role: "user", content: "ping" }] };

    // Strata: /health is open and reports not loaded; a keyless control call and a wrong key are 401s.
    assert.deepEqual((await call(fake.url, "GET", "/health")).json(), { status: "ok", loaded: false, model: "fake-flash" });
    assert.equal((await call(fake.url, "POST", "/load")).status, 401);
    assert.equal((await call(fake.url, "POST", "/v1/messages", messages, { "x-api-key": "wrong" })).status, 401);
    assert.deepEqual((await call(fake.url, "GET", "/v1/models", undefined, bearer)).json().data.map((m: any) => m.id), ["fake-flash", "org/fake:3b"]);

    // A Messages request for an unloaded model waits out the load delay, loads it, and streams the reply.
    const started = Date.now();
    const streamed = await call(fake.url, "POST", "/v1/messages", messages, bearer);
    assert.ok(Date.now() - started >= 50, "the load delay was waited out");
    assert.deepEqual(fake.loaded(), ["fake-flash"]);
    assert.equal((await call(fake.url, "GET", "/health")).json().loaded, true);
    const s = readSse(streamed.text);
    assert.equal(s.answer, "pong");
    assert.deepEqual([s.types[0], s.types.at(-2), s.types.at(-1)], ["message_start", "message_delta", "message_stop"]);
    assert.equal(s.stop, "end_turn");

    // Thinking and a tool call stream as their own blocks; a plain (non-streaming) request is one message.
    fake.setReply({ thinking: "hmm", text: "ok", toolUse: { name: "Read", input: { file_path: "a.txt" } } });
    const rich = readSse((await call(fake.url, "POST", "/v1/messages", messages, { "x-api-key": KEY })).text);
    assert.equal(rich.thinking, "hmm");
    assert.equal(rich.answer, "ok");
    assert.deepEqual(JSON.parse(rich.toolInput), { file_path: "a.txt" });
    assert.equal(rich.stop, "tool_use");
    const plain = (await call(fake.url, "POST", "/v1/messages", { ...messages, stream: false }, bearer)).json();
    assert.deepEqual(plain.content.map((b: any) => b.type), ["thinking", "text", "tool_use"]);
    assert.equal((await call(fake.url, "POST", "/v1/messages", { ...messages, model: "nope" }, bearer)).status, 404);

    // Strata unload.
    await call(fake.url, "POST", "/unload", {}, bearer);
    assert.deepEqual(fake.loaded(), []);

    // Ollama: tags, show, ps, load by generate, unload by keep_alive 0, a variant by create, delete.
    const tags = (await call(fake.url, "GET", "/api/tags")).json().models;
    assert.deepEqual(tags.map((m: any) => m.name), ["fake-flash", "org/fake:3b"]);
    assert.equal((await call(fake.url, "POST", "/api/show", { model: "org/fake:3b" })).json().model_info["fake.context_length"], 8192);
    assert.equal((await call(fake.url, "POST", "/api/show", { model: "gone" })).status, 404);
    assert.deepEqual((await call(fake.url, "GET", "/api/ps")).json().models, []);
    const loaded = (await call(fake.url, "POST", "/api/generate", { model: "org/fake:3b", keep_alive: "15m" })).json();
    assert.equal(loaded.done_reason, "load");
    assert.deepEqual((await call(fake.url, "GET", "/api/ps")).json().models.map((m: any) => m.name), ["org/fake:3b"]);
    assert.equal((await call(fake.url, "POST", "/api/generate", { model: "org/fake:3b", keep_alive: 0 })).json().done_reason, "unload");
    assert.deepEqual(fake.loaded(), []);
    const made = await call(fake.url, "POST", "/api/create", { model: "org/fake:3b-helm-64k", from: "org/fake:3b", parameters: { num_ctx: 65536 }, stream: false });
    assert.equal(made.json().status, "success");
    assert.equal(fake.models.find((m) => m.name === "org/fake:3b-helm-64k")?.contextTokens, 65536);
    assert.equal((await call(fake.url, "DELETE", "/api/delete", { model: "org/fake:3b-helm-64k" })).status, 200);
    assert.equal((await call(fake.url, "DELETE", "/api/delete", { model: "org/fake:3b-helm-64k" })).status, 404);

    // The log keeps the auth headers as received, so a test sees which one a client used.
    const sent = fake.messages().map((r) => [r.authorization, r.xApiKey, r.status]);
    assert.deepEqual(sent.slice(0, 3), [[undefined, "wrong", 401], [`Bearer ${KEY}`, undefined, 200], [undefined, KEY, 200]]);
    assert.equal(fake.messages()[1]!.body?.model, "fake-flash");

    // Down: the connection is dropped, as an unreachable server looks; back up it answers again.
    fake.setDown(true);
    await assert.rejects(call(fake.url, "GET", "/health"));
    fake.setDown(false);
    assert.equal((await call(fake.url, "GET", "/health")).status, 200);
  } finally {
    await fake.close();
  }
});
