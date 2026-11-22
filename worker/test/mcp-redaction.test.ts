// MC2's redaction test: a fake server echoes a configured secret in its description, schema,
// result and error, and none of it reaches the model or the UI.

import assert from "node:assert/strict";
import path from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { startHttpFake } from "../../fakes/mcp-server/server.ts";
import { generatedName, Redactor } from "../src/mcp/index.ts";
import type { Json, ResolvedServer } from "../src/mcp/types.ts";
import { harness, httpServer, stdioServer, text } from "./mcp-helpers.ts";

const SECRET = "sk-fake-S3CRET-value-12345";
/** 43 characters of the broker's minting shape (32 random bytes in base64url). */
const LEASE = "a".repeat(32) + "b".repeat(11);
const BROKER_FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../fakes/broker-mcp/server.ts");
const open: { close(): void }[] = [];
after(() => open.forEach((b) => b.close()));

test("stdio: description, schema key and value, result, structured content and error are scrubbed", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("leak", { secret: SECRET }, { env: { TOKEN: SECRET }, secrets: [SECRET] })] });
  const d = await h.bridge.tools({ server: "leak" });
  const discovery = JSON.stringify(d);
  assert.ok(!discovery.includes(SECRET), "discovery reveals the secret");
  const st = d.loaded.find((t) => t.label.endsWith("secret_echo"))!;
  assert.ok(st.description.includes("[redacted]"));
  const props = (st.inputSchema.properties ?? {}) as Record<string, unknown>;
  assert.deepEqual(Object.keys(props), ["ok"], "a schema key carrying the secret is dropped");

  const r = await h.bridge.call(generatedName("leak", "secret_echo"), {});
  const out = JSON.stringify(r);
  assert.ok(!out.includes(SECRET), "the result reveals the secret");
  assert.match(text(r), /the secret is \[redacted\]/);
  assert.match(text(r), /"token":"\[redacted\]"/, "structured content is appended, scrubbed");

  await assert.rejects(h.bridge.call(generatedName("leak", "fail"), {}), (e: Error) => {
    assert.ok(!e.message.includes(SECRET), "the error reveals the secret");
    assert.match(e.message, /\[redacted\]/);
    return true;
  });
  assert.ok(!JSON.stringify(h.lines).includes(SECRET), "a status line reveals the secret");
});

test("a tool whose name carries the secret is not offered", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("named", { toolNames: [`get_${SECRET}`, "plain"] }, { secrets: [SECRET] })] });
  const d = await h.bridge.tools({ server: "named" });
  assert.ok(!JSON.stringify(d).includes(SECRET));
  assert.ok(d.loaded.some((t) => t.label.endsWith("plain")));
  assert.equal(d.loaded.length, 10, "9 fixed tools plus plain; the secret-named one is dropped");
});

test("a server that dies printing the secret: the status error is scrubbed", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("dies", { secret: SECRET, failStart: "exit" }, { secrets: [SECRET] })] });
  await assert.rejects(h.bridge.tools({ server: "dies" }), (e: Error) => !e.message.includes(SECRET));
  const status = JSON.stringify(h.lines);
  assert.ok(status.includes("[redacted]"), "the stderr tail was reported");
  assert.ok(!status.includes(SECRET));
});

test("http: the bearer header reaches the server and never comes back", async () => {
  const f = await startHttpFake({ secret: SECRET, requireHeader: { name: "Authorization", value: `Bearer ${SECRET}` } });
  const h = harness();
  open.push(h.bridge, { close: () => void f.close() });
  h.bridge.configure({
    revision: "r1",
    servers: [httpServer("hub", f.url, { headers: { Authorization: `Bearer ${SECRET}` }, secrets: [SECRET] })],
  });
  const d = await h.bridge.tools({ server: "hub" });
  const r = await h.bridge.call(generatedName("hub", "secret_echo"), {});
  assert.equal(f.requests[0]!.headers.authorization, `Bearer ${SECRET}`);
  assert.ok(!JSON.stringify([d, r, h.lines]).includes(SECRET));
});

test("encoded forms of a secret are scrubbed too; very short values are left alone", () => {
  const red = new Redactor(['a"b\\c-secret', "xy"]);
  assert.equal(red.text('{"v":"a\\"b\\\\c-secret"}'), '{"v":"[redacted]"}');
  assert.equal(red.text("a%22b%5Cc-secret"), "[redacted]");
  assert.equal(red.text("xy stays"), "xy stays");
});

test("secret-looking runs are redacted by shape; git ids and prose are not", () => {
  const sha = "3b18e512dba79e4c8300dd08aeb37f8e93a1c2d4";
  const sha256 = "a".repeat(64);
  const jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r";
  const rows: Array<[string, string]> = [
    [`commit ${sha} on main`, `commit ${sha} on main`],
    [`sha256 ${sha256}`, `sha256 ${sha256}`],
    [`token: ${sha}`, "token: [redacted]"],
    [`{"api_key":"${"Ab3".repeat(12)}"}`, '{"api_key":"[redacted]"}'],
    [`SECRET=${sha256}`, "SECRET=[redacted]"],
    [`Authorization: Bearer ${"Zy9".repeat(12)}`, "Authorization: Bearer [redacted]"],
    [`jwt ${jwt} end`, "jwt [redacted] end"],
    ["key sk-abcdefghijklmnopqrstuvwx", "key [redacted]"],
    [`ghp_${"a1".repeat(18)} and github_pat_${"B2".repeat(20)}`, "[redacted] and [redacted]"],
    ["xoxb-123456789012-abcdef and AKIAABCDEFGHIJKLMNOP", "[redacted] and [redacted]"],
    ["the key to a good token is a short one", "the key to a good token is a short one"],
    ["Passwords must be rotated every ninety days.", "Passwords must be rotated every ninety days."],
    [`{"nextPageToken":"${sha}"}`, `{"nextPageToken":"${sha}"}`],
    [`{"page_token":"${sha}"}`, `{"page_token":"${sha}"}`],
    [`monkey ${sha}`, `monkey ${sha}`],
    [
      "key src/components/settings/runtime/UpgradeCommand",
      "key src/components/settings/runtime/UpgradeCommand",
    ],
    [`sync token: ${sha}`, `sync token: ${sha}`],
    [`access_token: ${sha}`, "access_token: [redacted]"],
    [`x-api-key: ${sha}`, "x-api-key: [redacted]"],
    // A broker lease key (32 random bytes in base64url is 43 characters) is redacted like any
    // other keyed run when nothing asks to keep it.
    [`{"key": "${LEASE}"}`, `{"key": "[redacted]"}`],
    [`{"api_key": "${"c".repeat(40)}"}`, `{"api_key": "[redacted]"}`],
  ];
  const r = new Redactor([]);
  for (const [input, want] of rows) assert.equal(r.text(input), want, input);
});

test("the broker's lease key survives redaction; every other server's is redacted as shaped", async () => {
  const broker = (cfg: Json, extra: Partial<ResolvedServer> = {}): ResolvedServer => ({
    id: "browser",
    name: "My browser",
    preset: "browser_broker",
    transport: "stdio",
    command: process.execPath,
    args: [BROKER_FAKE, "--stdio", JSON.stringify(cfg)],
    allowPrivateHttp: false,
    ...extra,
  });

  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [broker({ leaseKey: LEASE }, { secrets: [SECRET] })] });
  await h.bridge.tools({ server: "browser" });
  const r = await h.bridge.call(generatedName("browser", "browser_claim"), {});
  assert.ok(text(r).includes(`"key": "${LEASE}"`), "the broker hands its lease key to the model whole");

  // The same answer from a server that merely names its tools like the broker: no keeping.
  const plain = broker({ leaseKey: LEASE });
  delete plain.preset;
  const p = harness();
  open.push(p.bridge);
  p.bridge.configure({ revision: "r1", servers: [plain] });
  await p.bridge.tools({ server: "browser" });
  const pr = await p.bridge.call(generatedName("browser", "browser_claim"), {});
  assert.ok(!text(pr).includes(LEASE), "a non-broker server's key stays shape-redacted");

  // A hand-saved broker entry: the broker's own id, stdio, no preset field. The host fills the
  // preset in when it reads the config; this is the worker's second net for an older line.
  const hand = broker({ leaseKey: LEASE }, { id: "browser_broker" });
  delete hand.preset;
  const b = harness();
  open.push(b.bridge);
  b.bridge.configure({ revision: "r1", servers: [hand] });
  await b.bridge.tools({ server: "browser_broker" });
  const br = await b.bridge.call(generatedName("browser_broker", "browser_claim"), {});
  assert.ok(text(br).includes(`"key": "${LEASE}"`), "a broker-id entry without a preset keeps its lease key whole");

  // Keeping the key costs nothing else: everything around it is still redacted.
  const red = new Redactor([SECRET]);
  const kept = red.text(`lease ${LEASE} token ${"z".repeat(40)} {"api_key": "${"q".repeat(40)}"} ${SECRET}`, [LEASE]);
  assert.ok(kept.includes(LEASE), "the kept handle passes");
  for (const gone of [SECRET, "z".repeat(40), "q".repeat(40)]) {
    assert.ok(!kept.includes(gone), `${gone.slice(0, 8)}… still redacted next to a kept key`);
  }
  // A kept string equal to a resolved secret is never kept: the secret list outranks the keeps.
  const red2 = new Redactor([LEASE]);
  assert.ok(!red2.text(`lease ${LEASE}`, [LEASE]).includes(LEASE), "a keep that equals a secret is redacted");
  // Nor is a kept string that is only part of a secret.
  const red3 = new Redactor([`prefix-${LEASE}-suffix`]);
  assert.ok(!red3.text(`token ${LEASE}`, [LEASE]).includes(LEASE), "a keep inside a secret is shape-redacted like any other run");
});
