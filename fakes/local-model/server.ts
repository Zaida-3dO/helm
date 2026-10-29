// A fake local model server. One HTTP server answers every route a local model server offers, so
// a test (or a Helm run with every configured server pointed at it) needs a single base URL:
//
//   Anthropic Messages, what Claude Code talks to:
//     POST /v1/messages                 streaming (SSE) or plain JSON, scripted reply
//     POST /v1/messages/count_tokens    {input_tokens}
//   Strata-style control routes:
//     GET  /health                      {status: "ok", loaded: boolean, model}
//     POST /load                        marks the Strata model loaded after the load delay
//     POST /unload                      marks it unloaded
//     GET  /v1/models                   {object: "list", data: [{id}]} (the model list path is an option)
//   Ollama-style routes:
//     GET  /api/tags                    {models: [{name, model, size, details}]}
//     POST /api/show                    {model_info: {"general.architecture", "<arch>.context_length"}}
//     GET  /api/ps                      {models: [loaded ones, with size and context_length]}
//     POST /api/generate                no prompt: loads the model (after the load delay) or, with
//                                       keep_alive 0, unloads it; {done: true, done_reason: "load"|"unload"}
//     POST /api/create                  {model, from, parameters}: adds the model as a copy (stream:false
//                                       answers {status: "success"}, otherwise newline-delimited JSON)
//     DELETE /api/delete                {model}: removes it (404 when unknown)
//
// Loaded state is per model. A Messages request for a model that is not loaded waits out the load
// delay first, then loads it, as the real servers do. Test hooks, not part of any real server:
//
//   GET  /__fake/requests             every request so far: method, path, auth headers, parsed body
//   POST /__fake/state                {loaded?: {name: boolean}, down?: boolean, reply?: string}
//
// Keys here are made-up test values. When `key` is set, /v1/* and the Strata control routes need it
// as `Authorization: Bearer <key>` or `x-api-key: <key>` (a wrong or missing one is a 401 in the
// Anthropic error shape); /health and the Ollama routes are open. The request log keeps the auth
// headers exactly as received so a test can see which one a client used.
//
// CLI: `node fakes/local-model/server.ts [--port N] [--key K] [--load-delay MS] [--reply TEXT]
//   [--models name[@contextTokens[@sizeBytes]],...] [--models-path /v1/models|/models|none]`
// prints `fake-local-model listening http://127.0.0.1:<port>`. The same options can come from the
// environment: FAKE_LOCAL_MODEL_PORT, _KEY, _LOAD_DELAY_MS, _REPLY, _MODELS, _MODELS_PATH.

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { fileURLToPath } from "node:url";

type Json = Record<string, unknown>;

export interface FakeModel {
  name: string;
  contextTokens: number;
  sizeBytes: number;
  loaded: boolean;
}

/** What a scripted reply says; a plain string is `{text}`. */
export interface ReplySpec {
  /** Streamed in a few deltas. */
  text?: string;
  /** A thinking block before the text (what Claude Code shows as reasoning). */
  thinking?: string;
  /** A tool call after the text; ends the message with stop_reason "tool_use". */
  toolUse?: { name: string; input: Json };
  inputTokens?: number;
  outputTokens?: number;
}

export interface LoggedRequest {
  method: string;
  path: string;
  /** `Authorization` and `x-api-key` as received (made-up test values). */
  authorization?: string;
  xApiKey?: string;
  /** The parsed JSON body, when there was one. */
  body?: Json;
  /** The status the fake answered. */
  status: number;
}

export interface FakeLocalModelOptions {
  port?: number;
  key?: string;
  /** How long loading a model takes (default 0). */
  loadDelayMs?: number;
  models?: Partial<FakeModel>[];
  /** Answer to every Messages request unless `reply` is a function. */
  reply?: string | ReplySpec | ((request: Json) => string | ReplySpec);
  /** Where the Strata model list lives: "/v1/models" (default), "/models", or "none" (404 both). */
  modelsPath?: "/v1/models" | "/models" | "none";
}

export interface FakeLocalModel {
  url: string;
  models: FakeModel[];
  requests: LoggedRequest[];
  /** Only the Messages requests, with their bodies. */
  messages(): LoggedRequest[];
  /** Names of the models loaded now. */
  loaded(): string[];
  setLoaded(name: string, loaded: boolean): void;
  /** While down, every request is answered by dropping the connection (an unreachable server). */
  setDown(down: boolean): void;
  setReply(reply: FakeLocalModelOptions["reply"]): void;
  close(): Promise<void>;
}

const DEFAULT_MODELS: FakeModel[] = [
  { name: "fake-flash", contextTokens: 262_144, sizeBytes: 12_000_000_000, loaded: false },
  { name: "fake-small:3b", contextTokens: 131_072, sizeBytes: 2_000_000_000, loaded: false },
];

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

function readBody(req: IncomingMessage): Promise<Json | undefined> {
  return new Promise((resolve) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => {
      const text = Buffer.concat(chunks).toString("utf8");
      if (!text) return resolve(undefined);
      try {
        const v = JSON.parse(text) as unknown;
        resolve(v && typeof v === "object" ? (v as Json) : undefined);
      } catch {
        resolve(undefined);
      }
    });
    req.on("error", () => resolve(undefined));
  });
}

const header = (req: IncomingMessage, name: string): string | undefined => {
  const v = req.headers[name];
  return Array.isArray(v) ? v[0] : v;
};

/** The model name a body names (`model`, or Ollama's older `name`). */
const modelOf = (body: Json | undefined): string => String(body?.model ?? body?.name ?? "");

export async function startFakeLocalModel(opts: FakeLocalModelOptions = {}): Promise<FakeLocalModel> {
  const models: FakeModel[] = (opts.models ?? DEFAULT_MODELS).map((m, i) => ({ ...DEFAULT_MODELS[i % DEFAULT_MODELS.length]!, ...m }) as FakeModel);
  const requests: LoggedRequest[] = [];
  const loadDelayMs = opts.loadDelayMs ?? 0;
  const modelsPath = opts.modelsPath ?? "/v1/models";
  let reply = opts.reply ?? "Hello from the fake local model.";
  let down = false;
  let msgSeq = 0;

  const find = (name: string) => models.find((m) => m.name === name);
  const load = async (m: FakeModel) => {
    if (m.loaded) return;
    if (loadDelayMs > 0) await sleep(loadDelayMs);
    m.loaded = true;
  };

  const send = (res: ServerResponse, entry: LoggedRequest, status: number, body?: unknown) => {
    entry.status = status;
    res.writeHead(status, { "content-type": "application/json" });
    res.end(body === undefined ? "" : JSON.stringify(body));
  };
  const anthropicError = (res: ServerResponse, entry: LoggedRequest, status: number, type: string, message: string) =>
    send(res, entry, status, { type: "error", error: { type, message } });

  const authorized = (req: IncomingMessage): boolean => {
    if (!opts.key) return true;
    const bearer = /^Bearer (.+)$/i.exec(header(req, "authorization") ?? "")?.[1];
    return bearer === opts.key || header(req, "x-api-key") === opts.key;
  };

  const server = createServer((req, res) => {
    void (async () => {
      const url = new URL(req.url ?? "/", "http://127.0.0.1");
      const path = url.pathname;
      const method = req.method ?? "GET";
      if (down) return void req.socket.destroy();
      const body = await readBody(req);
      const entry: LoggedRequest = { method, path, authorization: header(req, "authorization"), xApiKey: header(req, "x-api-key"), status: 0 };
      if (body) entry.body = body;
      if (!path.startsWith("/__fake/")) requests.push(entry);

      // ---- test hooks
      if (path === "/__fake/requests") return send(res, entry, 200, requests);
      if (path === "/__fake/state" && method === "POST") {
        for (const [name, v] of Object.entries((body?.loaded ?? {}) as Json)) {
          const m = find(name);
          if (m) m.loaded = v === true;
        }
        if (typeof body?.down === "boolean") down = body.down;
        if (typeof body?.reply === "string") reply = body.reply;
        return send(res, entry, 200, {});
      }

      const control = path === "/load" || path === "/unload" || path === "/v1/models" || path === "/models";
      if ((path.startsWith("/v1/") || control) && !authorized(req)) {
        return anthropicError(res, entry, 401, "authentication_error", "invalid x-api-key");
      }

      // ---- Anthropic Messages
      if (path === "/v1/messages/count_tokens" && method === "POST") {
        return send(res, entry, 200, { input_tokens: Math.max(1, Math.ceil(JSON.stringify(body?.messages ?? "").length / 4)) });
      }
      if (path === "/v1/messages" && method === "POST") {
        const name = modelOf(body);
        const m = find(name);
        if (!m) return anthropicError(res, entry, 404, "not_found_error", `model: ${name}`);
        await load(m);
        const spec = (() => {
          const r = typeof reply === "function" ? reply(body ?? {}) : reply;
          return typeof r === "string" ? { text: r } : r;
        })();
        return body?.stream === true ? streamMessage(res, entry, name, spec) : send(res, entry, 200, plainMessage(name, spec));
      }

      // ---- Strata
      if (path === "/health" && method === "GET") {
        const strata = models[0];
        return send(res, entry, 200, { status: "ok", loaded: strata?.loaded === true, model: strata?.name ?? null });
      }
      if (path === "/load" && method === "POST") {
        const m = find(modelOf(body)) ?? models[0];
        if (!m) return send(res, entry, 404, { error: "no model" });
        await load(m);
        return send(res, entry, 200, { status: "loaded", model: m.name });
      }
      if (path === "/unload" && method === "POST") {
        for (const m of models) m.loaded = false;
        return send(res, entry, 200, { status: "unloaded" });
      }
      if (path === modelsPath && method === "GET") {
        return send(res, entry, 200, { object: "list", data: models.map((m) => ({ id: m.name, object: "model", context_length: m.contextTokens })) });
      }

      // ---- Ollama
      if (path === "/api/tags" && method === "GET") {
        return send(res, entry, 200, {
          models: models.map((m) => ({ name: m.name, model: m.name, size: m.sizeBytes, digest: `sha-${m.name}`, details: { family: "fake", parameter_size: "?" } })),
        });
      }
      if (path === "/api/show" && method === "POST") {
        const m = find(modelOf(body));
        if (!m) return send(res, entry, 404, { error: `model '${modelOf(body)}' not found` });
        return send(res, entry, 200, { details: { family: "fake" }, model_info: { "general.architecture": "fake", "fake.context_length": m.contextTokens }, parameters: "" });
      }
      if (path === "/api/ps" && method === "GET") {
        const live = models.filter((m) => m.loaded);
        return send(res, entry, 200, {
          models: live.map((m) => ({ name: m.name, model: m.name, size: m.sizeBytes, size_vram: m.sizeBytes, context_length: m.contextTokens, expires_at: "2099-01-01T00:00:00Z" })),
        });
      }
      if (path === "/api/generate" && method === "POST") {
        const m = find(modelOf(body));
        if (!m) return send(res, entry, 404, { error: `model '${modelOf(body)}' not found` });
        const unload = body?.keep_alive === 0 || body?.keep_alive === "0" || body?.keep_alive === "0s";
        if (unload) m.loaded = false;
        else await load(m);
        return send(res, entry, 200, { model: m.name, created_at: new Date().toISOString(), response: "", done: true, done_reason: unload ? "unload" : "load" });
      }
      if (path === "/api/create" && method === "POST") {
        const from = find(String(body?.from ?? ""));
        const name = String(body?.model ?? body?.name ?? "");
        if (!from || !name) return send(res, entry, 400, { error: "create needs model and an existing from" });
        const ctx = Number((body?.parameters as Json | undefined)?.num_ctx);
        const existing = find(name);
        if (existing) Object.assign(existing, { contextTokens: Number.isFinite(ctx) && ctx > 0 ? ctx : existing.contextTokens });
        else models.push({ name, contextTokens: Number.isFinite(ctx) && ctx > 0 ? ctx : from.contextTokens, sizeBytes: from.sizeBytes, loaded: false });
        if (body?.stream === false) return send(res, entry, 200, { status: "success" });
        entry.status = 200;
        res.writeHead(200, { "content-type": "application/x-ndjson" });
        return void res.end(`${JSON.stringify({ status: "using existing layer" })}\n${JSON.stringify({ status: "success" })}\n`);
      }
      if (path === "/api/delete" && method === "DELETE") {
        const i = models.findIndex((m) => m.name === modelOf(body));
        if (i === -1) return send(res, entry, 404, { error: `model '${modelOf(body)}' not found` });
        models.splice(i, 1);
        return send(res, entry, 200);
      }
      send(res, entry, 404, { error: `no route ${method} ${path}` });
    })().catch(() => {
      if (!res.headersSent) res.writeHead(500);
      res.end();
    });
  });

  // ---- message bodies
  const usageOf = (spec: ReplySpec) => ({ input_tokens: spec.inputTokens ?? 12, output_tokens: spec.outputTokens ?? 7 });
  function blocks(spec: ReplySpec): Json[] {
    const out: Json[] = [];
    if (spec.thinking) out.push({ type: "thinking", thinking: spec.thinking, signature: "fake-signature" });
    if (spec.text) out.push({ type: "text", text: spec.text });
    if (spec.toolUse) out.push({ type: "tool_use", id: `toolu_fake_${++msgSeq}`, name: spec.toolUse.name, input: spec.toolUse.input });
    return out;
  }
  const plainMessage = (model: string, spec: ReplySpec): Json => ({
    id: `msg_fake_${++msgSeq}`,
    type: "message",
    role: "assistant",
    model,
    content: blocks(spec),
    stop_reason: spec.toolUse ? "tool_use" : "end_turn",
    stop_sequence: null,
    usage: usageOf(spec),
  });

  function streamMessage(res: ServerResponse, entry: LoggedRequest, model: string, spec: ReplySpec): void {
    entry.status = 200;
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    const ev = (event: string, data: Json) => res.write(`event: ${event}\ndata: ${JSON.stringify({ type: event, ...data })}\n\n`);
    const usage = usageOf(spec);
    ev("message_start", { message: { id: `msg_fake_${++msgSeq}`, type: "message", role: "assistant", model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: usage.input_tokens, output_tokens: 1 } } });
    const list = blocks(spec);
    list.forEach((b, index) => {
      if (b.type === "thinking") {
        ev("content_block_start", { index, content_block: { type: "thinking", thinking: "" } });
        ev("content_block_delta", { index, delta: { type: "thinking_delta", thinking: b.thinking } });
        ev("content_block_delta", { index, delta: { type: "signature_delta", signature: b.signature } });
      } else if (b.type === "text") {
        ev("content_block_start", { index, content_block: { type: "text", text: "" } });
        // Split in two so a client that joins deltas is exercised.
        const t = String(b.text);
        const cut = Math.ceil(t.length / 2);
        for (const piece of [t.slice(0, cut), t.slice(cut)]) if (piece) ev("content_block_delta", { index, delta: { type: "text_delta", text: piece } });
      } else {
        ev("content_block_start", { index, content_block: { type: "tool_use", id: b.id, name: b.name, input: {} } });
        ev("content_block_delta", { index, delta: { type: "input_json_delta", partial_json: JSON.stringify(b.input) } });
      }
      ev("content_block_stop", { index });
    });
    ev("message_delta", { delta: { stop_reason: spec.toolUse ? "tool_use" : "end_turn", stop_sequence: null }, usage: { output_tokens: usage.output_tokens } });
    ev("message_stop", {});
    res.end();
  }

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(opts.port ?? 0, "127.0.0.1", () => resolve());
  });
  const port = (server.address() as AddressInfo).port;
  return {
    url: `http://127.0.0.1:${port}`,
    models,
    requests,
    messages: () => requests.filter((r) => r.path === "/v1/messages"),
    loaded: () => models.filter((m) => m.loaded).map((m) => m.name),
    setLoaded: (name, loaded) => {
      const m = find(name);
      if (m) m.loaded = loaded;
    },
    setDown: (d) => {
      down = d;
    },
    setReply: (r) => {
      reply = r ?? "";
    },
    close: () =>
      new Promise<void>((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

// ---- command line

function parseModels(spec: string): Partial<FakeModel>[] {
  return spec.split(",").filter(Boolean).map((part) => {
    // "@" separates the fields because Ollama names hold colons and slashes ("name:3b").
    const [name, ctx, size] = part.split("@");
    return { name, ...(ctx ? { contextTokens: Number(ctx) } : {}), ...(size ? { sizeBytes: Number(size) } : {}) };
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const arg = (name: string, envName: string): string | undefined => {
    const i = process.argv.indexOf(`--${name}`);
    return i !== -1 ? process.argv[i + 1] : process.env[`FAKE_LOCAL_MODEL_${envName}`];
  };
  const port = arg("port", "PORT");
  const delay = arg("load-delay", "LOAD_DELAY_MS");
  const models = arg("models", "MODELS");
  const modelsPath = arg("models-path", "MODELS_PATH");
  const fake = await startFakeLocalModel({
    port: port ? Number(port) : undefined,
    key: arg("key", "KEY"),
    loadDelayMs: delay ? Number(delay) : undefined,
    reply: arg("reply", "REPLY"),
    models: models ? parseModels(models) : undefined,
    modelsPath: modelsPath as FakeLocalModelOptions["modelsPath"],
  });
  process.stdout.write(`fake-local-model listening ${fake.url}\n`);
}
