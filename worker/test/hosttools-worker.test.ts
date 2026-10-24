// A fake-claude round-trip through the worker's helm MCP server and the request channel.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import type { Json } from "../../fakes/host/fake-host.ts";
import { prompt, startWithFixture } from "./helpers.ts";

const LIST_CHATS = { name: "app_list_chats", description: "List chats.", inputSchema: { type: "object" }, requestLimitBytes: 8192, timeoutMs: 20000, children: false, mutating: false };

test("Claude Code reaches host tools through the helm MCP server, attributed per call", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-ht-w-"));
  const s = startWithFixture("host-tools", { launch: { hostTools: [LIST_CHATS], chatToolsDirectory: dir } });
  try {
    await s.host.ready();
    // Play the host: answer each chat_request the way helm-hosttools' gate would.
    const answered: Json[] = [];
    const serve = async () => {
      for (let n = 0; n < 2; n++) {
        const r = await s.host.waitFor((e) => e.type === "chat_request" && !answered.includes(e), 10_000, 0);
        answered.push(r);
        const child = (r.caller as Json | undefined)?.child === true;
        const body = child ? { id: r.id, ok: false, error: "app_list_chats is only available to the parent chat. Nothing changed.", changed: "no" } : { id: r.id, ok: true, result: { chats: [{ id: "c1" }] } };
        fs.writeFileSync(path.join(dir, String(r.id), "response.json"), JSON.stringify(body));
      }
    };
    const served = serve();
    // The host's mcp_config line reaches Helm's MCP client: with a server configured, the helm
    // server lists app_mcp_tools.
    s.host.send({ type: "mcp_config", revision: "r1", servers: [{ id: "s1", name: "notes", transport: "stdio", allowPrivateHttp: false, error: "not resolved in this test" }] });
    const resp = await prompt(s.host, "use the tools");
    assert.equal(resp.success, true);
    await served;
    await s.host.waitForType("agent_settled");

    const argv = (s.fakeStart()?.argv ?? []) as string[];
    const at = argv.indexOf("--mcp-config");
    assert.ok(at >= 0, "--mcp-config is passed");
    assert.deepEqual(JSON.parse(argv[at + 1]), { mcpServers: { helm: { type: "sdk", name: "helm" } } });
    assert.equal(argv[argv.indexOf("--allowedTools") + 1], "mcp__helm");
    assert.ok(!argv.includes("--strict-mcp-config"), "native MCP stays on by default");

    const got = s.received();
    const init = got.find((r) => r.type === "control_request" && (r.request as Json).subtype === "initialize")?.request as Json;
    assert.deepEqual(init.sdkMcpServers, ["helm"]);
    assert.ok(JSON.stringify(init.hooks).includes("mcp__helm__.*"), "a PreToolUse hook on the helm tools");
    assert.ok(JSON.stringify(init.hooks).includes("helm-duplicate-mcp"), "a PreToolUse hook on every MCP tool, for duplicates of Helm's servers");

    const mcp = (id: number) => {
      const r = got.find((x) => x.type === "control_response" && ((x.response as Json).response as Json | undefined)?.mcp_response && (((x.response as Json).response as Json).mcp_response as Json).id === id);
      return ((r?.response as Json).response as Json).mcp_response as Json;
    };
    assert.equal(((mcp(0).result as Json).serverInfo as Json).name, "helm");
    const names = (((mcp(1).result as Json).tools as Json[]) ?? []).map((t) => t.name);
    assert.deepEqual(names, ["app_list_chats", "app_list_tools", "app_mcp_call", "app_mcp_tools"]);
    assert.deepEqual((mcp(2).result as Json).structuredContent, { chats: [{ id: "c1" }] });
    assert.equal((mcp(3).result as Json).isError, true);

    // The request lines carry the binding the hook gave each call.
    assert.deepEqual(answered[0].caller, { child: false, toolCallId: "tu_root" });
    assert.deepEqual(answered[0].args, { query: "x" });
    assert.deepEqual(answered[1].caller, { child: true, toolCallId: "tu_child" });
    // Each call ends with chat_cancel and its folder removed.
    assert.equal(s.host.ofType("chat_cancel").length, 2);
    assert.deepEqual(fs.readdirSync(dir), []);
  } finally {
    await s.done();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("without hostTools the launch is unchanged: no helm server, no hook", async () => {
  const s = startWithFixture("tool-call");
  try {
    await s.host.ready();
    const argv = (s.fakeStart()?.argv ?? []) as string[];
    assert.ok(!argv.includes("--mcp-config"));
    assert.ok(!argv.includes("--allowedTools"));
    const init = s.received().find((r) => r.type === "control_request" && (r.request as Json).subtype === "initialize")?.request as Json;
    assert.equal(init.sdkMcpServers, undefined);
  } finally {
    await s.done();
  }
});

test("native MCP off adds --strict-mcp-config; held servers are disallowed", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-ht-w-"));
  const s = startWithFixture("tool-call", { claude: { strictMcpConfig: true, disabledMcpServers: ["notes", "bad name", "helm"] }, launch: { hostTools: [], chatToolsDirectory: dir } });
  try {
    await s.host.ready();
    const argv = (s.fakeStart()?.argv ?? []) as string[];
    assert.ok(argv.includes("--strict-mcp-config"));
    const dis = argv[argv.indexOf("--disallowedTools") + 1].split(",");
    assert.ok(dis.includes("mcp__notes"));
    assert.ok(!dis.some((d) => d.includes("bad name") || d === "mcp__helm"), "only safe names, never Helm's own server");
    const settings = JSON.parse(argv[argv.indexOf("--settings") + 1]);
    assert.deepEqual(settings.disabledMcpjsonServers, ["notes"], "held repository servers are off for the .mcp.json loader too");
  } finally {
    await s.done();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
