// The built-in browser: what the worker does when the broker's answer says a person is needed.
// A needs_owner refusal or a sign-in request reaches the host (flag + Telegram ask), and the
// broker lease is renewed while that hold is open — until the person's "done" (or a release)
// finishes it.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { generatedName } from "../src/mcp/naming.ts";
import { createMcpBridge } from "../src/mcp/index.ts";
import type { Json, ResolvedServer } from "../src/mcp/types.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const BROKER_FAKE = path.resolve(here, "../../fakes/broker-mcp/server.ts");

function broker(cfg: Json, extra: Partial<ResolvedServer> = {}): ResolvedServer {
  return {
    id: "browser",
    name: "My browser",
    preset: "browser_broker",
    transport: "stdio",
    command: process.execPath,
    args: [BROKER_FAKE, "--stdio", JSON.stringify(cfg)],
    allowPrivateHttp: false,
    ...extra,
  };
}

interface Ask {
  operation: string;
  args: Json;
}

function harness(cfg: Json, opts: { renewMs?: number; capMs?: number } = {}, preset: "keep" | "drop" = "keep") {
  const dir = fs.mkdtempSync(path.join(process.env.TEMP ?? "/tmp", "broker-hold-"));
  const statuses = path.join(dir, "statuses.txt");
  fs.writeFileSync(statuses, "");
  const asks: Ask[] = [];
  const bridge = createMcpBridge(() => {}, {
    cwd: dir,
    browser: { renewMs: opts.renewMs ?? 20, capMs: opts.capMs ?? 30_000 },
    hostRequest: async (operation, args) => {
      asks.push({ operation, args });
      return { ok: true };
    },
  });
  const server = broker({ ...cfg, statuses });
  if (preset === "drop") delete server.preset;
  bridge.configure({ revision: "r1", servers: [server] });
  const count = () => fs.readFileSync(statuses, "utf8").split("\n").filter((l) => l !== "").length;
  return { bridge, asks, dir, statuses, count };
}

test("a needs_owner refusal raises the flag and the Telegram ask, and renews until done", async () => {
  const h = harness({});
  await h.bridge.tools({ server: "browser" });
  await h.bridge.call(generatedName("browser", "browser_navigate"), { lease_key: "L1", url: "https://shop.example/groceries/" });
  const act = await h.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L1", action: "click", target: "Basket" });
  assert.ok(act.isError, "the refusal is still an error for the agent");
  assert.deepEqual(
    h.asks.map((a) => a.operation).sort(),
    ["app_flag_attention", "app_telegram_ask"],
  );
  const flag = h.asks.find((a) => a.operation === "app_flag_attention")!.args;
  assert.match(String(flag.reason), /shop\.example/, "the site host names the ask");
  assert.match(String(flag.reason), /attached\.needs_owner/, "the broker's own sentence is relayed");
  assert.equal(flag.priority, "high");
  const tg = h.asks.find((a) => a.operation === "app_telegram_ask")!.args;
  assert.deepEqual(tg.options, ["Done", "Not yet"]);

  // A repeat refusal while the hold is open is not a second ask.
  await h.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L1", action: "click", target: "Basket" });
  assert.equal(h.asks.length, 2, "one flag and one Telegram ask per open hold");

  // While the hold is open the lease is renewed (browser_status is the renew verb).
  await new Promise((r) => setTimeout(r, 60));
  await h.bridge.call(generatedName("browser", "browser_status"), { lease_key: "L1" });
  assert.ok(h.count() >= 3, `expected renewals plus the agent's call, saw ${h.count()}`);

  // The person answered done: the hold ends, renewing stops.
  await h.bridge.call(generatedName("browser", "browser_sign_in"), { lease_key: "L1", action: "done" });
  const before = h.count();
  await new Promise((r) => setTimeout(r, 80));
  assert.equal(h.count(), before, "no renewal after the done");
  h.bridge.close();
});

test("a sign-in request carries the broker's sentence, and only the browser preset reports", async () => {
  const h = harness({ act: "ok" });
  await h.bridge.tools({ server: "browser" });
  const r = await h.bridge.call(generatedName("browser", "browser_sign_in"), { lease_key: "L9", action: "request", what: "the shop" });
  assert.ok(!r.isError);
  assert.deepEqual(h.asks.map((a) => a.operation).sort(), ["app_flag_attention", "app_telegram_ask"]);
  const flag = h.asks.find((a) => a.operation === "app_flag_attention")!.args;
  assert.match(String(flag.reason), /the shop wants you to sign in/, "the relayed sentence, not the JSON shape");
  assert.ok(!String(flag.reason).includes("wait_seconds"));
  h.bridge.close();

  // The same tool names on a server that is not the browser preset: nothing is raised — a
  // configured server that merely names its tools like the broker cannot drive the phone path.
  const p = harness({}, {}, "drop");
  await p.bridge.tools({ server: "browser" });
  await p.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L1" });
  await p.bridge.call(generatedName("browser", "browser_sign_in"), { lease_key: "L1", action: "request", what: "x" });
  assert.deepEqual(p.asks, []);
  p.bridge.close();
});

test("a release ends a hold, and a hold that outlasts the cap renews no more", async () => {
  const h = harness({}, { capMs: 30 });
  await h.bridge.tools({ server: "browser" });
  await h.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L2" });
  assert.equal(h.asks.length, 2, "one flag and one ask per hold");
  await new Promise((r) => setTimeout(r, 80));
  const atCap = h.count();
  await new Promise((r) => setTimeout(r, 60));
  assert.equal(h.count(), atCap, "the cap stopped the renewals");

  // A second refusal starts a fresh hold; a release ends it without a Telegram round trip.
  await h.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L2" });
  await h.bridge.call(generatedName("browser", "browser_release"), { lease_key: "L2" });
  const before = h.count();
  await new Promise((r) => setTimeout(r, 60));
  assert.equal(h.count(), before, "released: no more renewals");
  h.bridge.close();
});
