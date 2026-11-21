// The built-in browser: what the worker does when the broker's answer says a person is needed.
// A needs_owner refusal or a sign-in request reaches the host (flag + Telegram ask), and the
// broker lease is renewed while that hold is open — until the person's "done" (or a release)
// finishes it.

import assert from "node:assert/strict";
import { createHash, createHmac } from "node:crypto";
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

// ── Approve cards over the fake broker's acting gate ────────────────────────────────────────
//
// The fake broker's `page` mode answers browser_act the way an attached browser decides it —
// the site list, the password fields, the consequence words, and the owner grant. These rows
// drive the bridge against it: which refusals ask for an Approve card, which stay refused, and
// that the broker (not the worker) is what rejects a grant that is replayed, forged, or signed
// for another lease or verb. The grant minted below copies the broker's own seven-field payload.

const GRANT_SECRET = "gate-secret";
const BSL = String.fromCharCode(92);
const NL = String.fromCharCode(10);
const gEsc = (s: string) => s.split(BSL).join(BSL + BSL).split("|").join(BSL + "|");
const gDigest = (v: string | undefined) => (!v ? "" : createHash("sha256").update(v, "utf8").digest("hex"));
/** The broker's grant form: seven escaped fields, `<issued>.<base64url tag>`. */
function grantFor(t: { leaseKey: string; ref: string; name: string; host: string; verb: string; value?: string }, issued = Math.floor(Date.now() / 1000)): string {
  const payload = [gEsc(t.leaseKey), gEsc(t.ref), gEsc(t.name), gEsc(t.host), String(issued), gEsc(t.verb), gEsc(gDigest(t.value))].join("|");
  return `${issued}.${createHmac("sha256", GRANT_SECRET).update(payload).digest().toString("base64url")}`;
}

function gateHarness(page: Json) {
  const dir = fs.mkdtempSync(path.join(process.env.TEMP ?? "/tmp", "broker-gate-"));
  const acts = path.join(dir, "acts.txt");
  fs.writeFileSync(acts, "");
  const asks: Ask[] = [];
  let cards = 0;
  const bridge = createMcpBridge(() => {}, {
    cwd: dir,
    browser: { renewMs: 20, capMs: 30_000 },
    hostRequest: async (operation, args) => {
      asks.push({ operation, args });
      if (operation === "app_browser_step") return { ok: true, result: { requestId: `r${++cards}`, state: "pending", summary: "a step on your browser" } };
      return { ok: true };
    },
  });
  bridge.configure({ revision: "r1", servers: [broker({ page, acts }, { env: { BROKER_OWNER_GRANT_SECRET: GRANT_SECRET } })] });
  const rows = () => fs.readFileSync(acts, "utf8").split(NL).filter((l) => l !== "").map((l) => JSON.parse(l) as Json);
  return { bridge, asks, dir, rows };
}

async function until(fn: () => boolean, ms = 3_000): Promise<boolean> {
  const t0 = Date.now();
  while (!fn() && Date.now() - t0 < ms) await new Promise((r) => setTimeout(r, 20));
  return fn();
}

const SHOP_PAGE: Json = {
  host: "shop.example",
  actSites: ["shop.example"],
  controls: { e12: { role: "button", name: "Order now" }, e13: { role: "textbox", name: "Quantity" } },
};

test("the acting gate decides: a card, a refusal, or a pass — a refused step never acts", async () => {
  const rows = [
    { case: "a consequential click on a listed site", page: SHOP_PAGE, act: { action: "click", target: "e12" }, want: "card" },
    { case: "ticking a consequential box", page: { ...SHOP_PAGE, controls: { e12: { role: "checkbox", name: "Order the warranty" } } }, act: { action: "check", target: "e12" }, want: "card" },
    { case: "a site off the act list", page: { ...SHOP_PAGE, actSites: ["other.example"] }, act: { action: "click", target: "e12" }, want: "refused" },
    { case: "typing into a password field", page: { ...SHOP_PAGE, controls: { pw: { role: "textbox", name: "Password", inputType: "password" } } }, act: { action: "type", target: "pw", value: "x" }, want: "refused" },
    { case: "a submit over a page holding a password stays with the person", page: { ...SHOP_PAGE, secretField: true, controls: { sub: { role: "button", name: "Continue", inputType: "submit" }, pw: { role: "textbox", name: "Password" } } }, act: { action: "click", target: "sub" }, want: "secret" },
    { case: "a checkout is always the person's own", page: { ...SHOP_PAGE, controls: { e12: { role: "button", name: "Checkout" } } }, act: { action: "click", target: "e12" }, want: "never" },
  ];
  for (const row of rows) {
    const h = gateHarness(row.page);
    await h.bridge.tools({ server: "browser" });
    await h.bridge.call(generatedName("browser", "browser_navigate"), { lease_key: "L1", url: "https://anything/" });
    const r = await h.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L1", ...row.act });
    const acted = () => h.rows().filter((x) => x.outcome === "acted").length;
    const card = h.asks.find((a) => a.operation === "app_browser_step");
    switch (row.want) {
      case "card": {
        assert.ok(card, `${row.case}: the refusal asked for an Approve card`);
        assert.equal(card!.args.verb, row.act.action, row.case);
        assert.equal(card!.args.ref, row.act.target, row.case);
        assert.equal(card!.args.host, "shop.example", row.case);
        assert.equal(acted(), 0, `${row.case}: nothing acts before an Approve`);
        // While a card waits, the agent's repeat never reaches the broker again.
        const before = h.rows().length;
        const again = await h.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L1", ...row.act });
        assert.ok(again.isError && h.rows().length === before, `${row.case}: a waiting card blocks the retry`);
        break;
      }
      case "refused": {
        assert.ok(r.isError, `${row.case}: the broker's refusal reaches the agent`);
        assert.equal(card, undefined, `${row.case}: no card for a step the broker refused outright`);
        assert.equal(acted(), 0, `${row.case}: nothing acts`);
        break;
      }
      case "secret": {
        assert.equal(card, undefined, `${row.case}: no card for a sign-in or secret step`);
        assert.ok(h.asks.some((a) => a.operation === "app_flag_attention"), `${row.case}: it goes back to the person at their browser`);
        assert.equal(acted(), 0, `${row.case}: nothing acts`);
        break;
      }
      case "never": {
        assert.ok(JSON.stringify(r.content).includes("always does themselves"), `${row.case}: refused for good`);
        assert.equal(card, undefined, `${row.case}: no card is ever asked for it`);
        assert.equal(acted(), 0, `${row.case}: nothing acts`);
        const before = h.rows().length;
        await h.bridge.call(generatedName("browser", "browser_act"), { lease_key: "L1", ...row.act });
        assert.equal(h.rows().length, before, `${row.case}: the retry never reaches the broker again`);
        break;
      }
    }
    h.bridge.close();
  }
});

test("the broker accepts a grant once, for its own lease and verb only", async () => {
  const h = gateHarness(SHOP_PAGE);
  await h.bridge.tools({ server: "browser" });
  await h.bridge.call(generatedName("browser", "browser_navigate"), { lease_key: "L1", url: "https://anything/" });
  const act = { lease_key: "L1", action: "click", target: "e12" };
  assert.ok((await h.bridge.call(generatedName("browser", "browser_act"), act)).isError);
  const results = () => h.asks.filter((a) => a.operation === "app_browser_step_result");
  const actedCount = () => h.rows().filter((x) => x.outcome === "acted").length;

  // Approved: the retry carries the grant and the broker acts once.
  const good = grantFor({ leaseKey: "L1", ref: "e12", name: "Order now", host: "shop.example", verb: "click" });
  assert.ok(h.bridge.deliverOwnerGrant({ requestId: "r1", serverId: "browser", leaseKey: "L1", ref: "e12", grant: good }));
  assert.ok(await until(() => results().length === 1), "the retried step answered back");
  assert.equal(results()[0]!.args.ok, true);
  assert.ok(actedCount() === 1, "the approved step acted exactly once");
  assert.ok(!JSON.stringify(results()[0]!.args).includes(good.split(".")[1]!), "the grant is not in the answer");

  // The same grant answered to a second card: spent, and refused as replayed.
  await h.bridge.call(generatedName("browser", "browser_act"), act);
  assert.ok(h.bridge.deliverOwnerGrant({ requestId: "r2", serverId: "browser", leaseKey: "L1", ref: "e12", grant: good }));
  assert.ok(await until(() => results().length === 2));
  assert.equal(results()[1]!.args.ok, false);
  assert.match(String(results()[1]!.args.text), /grant_invalid/, "the broker itself refused it");
  assert.match(String(results()[1]!.args.text), /replayed/);
  assert.equal(actedCount(), 1, "a replayed grant acts nothing");

  // A grant signed for another lease is forged for this one.
  await h.bridge.call(generatedName("browser", "browser_act"), act);
  const otherLease = grantFor({ leaseKey: "L9", ref: "e12", name: "Order now", host: "shop.example", verb: "click" });
  assert.ok(h.bridge.deliverOwnerGrant({ requestId: "r3", serverId: "browser", leaseKey: "L1", ref: "e12", grant: otherLease }));
  assert.ok(await until(() => results().length === 3));
  assert.equal(results()[2]!.args.ok, false);
  assert.match(String(results()[2]!.args.text), /forged/);

  // A grant signed for another verb and value is forged too.
  await h.bridge.call(generatedName("browser", "browser_act"), act);
  const otherVerb = grantFor({ leaseKey: "L1", ref: "e12", name: "Order now", host: "shop.example", verb: "type", value: "2" });
  assert.ok(h.bridge.deliverOwnerGrant({ requestId: "r4", serverId: "browser", leaseKey: "L1", ref: "e12", grant: otherVerb }));
  assert.ok(await until(() => results().length === 4));
  assert.equal(results()[3]!.args.ok, false);
  assert.match(String(results()[3]!.args.text), /forged/);
  assert.equal(actedCount(), 1, "nothing else acted on this lease");
  h.bridge.close();
});
