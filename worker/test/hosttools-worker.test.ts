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

test("very long results of Helm's tools are cut to a head and a tail; short ones, discovery and an off cap pass whole", async () => {
  const { HostToolServer } = await import("../src/hosttools/server.ts");
  const { capText, fileSaver } = await import("../src/hosttools/cap.ts");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-cap-"));
  try {
    const bridge = {
      enabled: () => true,
      tools: async () => ({ text: "t".repeat(9000) }),
      has: (n: string) => n === "big" || n === "small",
      call: async (n: string) => ({ ok: true as const, result: { text: n === "big" ? `HEAD${"x".repeat(9000)}TAIL` : "ok" } }),
    };
    const make = (cap: boolean) =>
      new HostToolServer({ hostTools: [], channel: {} as never, bridge, outputCap: cap ? { maxChars: 5000, save: fileSaver("c1", dir) } : undefined });
    const text = async (s: InstanceType<typeof HostToolServer>, name: string, args: Json = {}) => {
      const r = (await s.handle({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, { child: false })) as Json;
      return (r.result as Json) ?? {};
    };
    const big = await text(make(true), "big");
    const cut = ((big.content as Json[])[0].text as string);
    assert.ok(cut.length < 5600 && cut.startsWith('{"text":"HEAD') && cut.endsWith('TAIL"}'), "head and tail kept");
    assert.match(cut, /Helm cut [\d,]+ characters from the middle of this output \([\d,]+ in all\)/);
    assert.equal(big.structuredContent, undefined, "the structured copy would defeat the cut");
    const saved = /saved at (.+?); read that file/.exec(cut)?.[1] ?? "";
    const onDisk = fs.readFileSync(saved, "utf8");
    assert.ok(onDisk.includes("x".repeat(9000)), "the whole output is in the file");
    assert.ok(onDisk.startsWith('{' + String.fromCharCode(10) + '  "text": "HEAD'), "JSON is indented so a line-limited reader can page through it");
    if (process.platform !== "win32") assert.equal(fs.statSync(saved).mode & 0o777, 0o600);
    assert.ok(((await text(make(true), "small")).content as Json[])[0].text === '{"text":"ok"}', "short results pass whole");
    assert.equal(((await text(make(true), "app_mcp_tools")).content as Json[])[0].text, JSON.stringify({ text: "t".repeat(9000) }), "tool discovery is exempt");
    assert.ok((((await text(make(false), "big")).content as Json[])[0].text as string).length > 9000, "cap off: whole");
    // Without a place to save, the marker says to ask for less; a surrogate pair is never split.
    assert.match(capText("a".repeat(100), 20), /Ask again for less/);
    assert.doesNotMatch(capText("😀".repeat(50), 21), /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("sub-agent limits register one hook and add a rule to the chat's instructions, only when the launch sets them", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-ht-w-"));
  const run = async (claude: Json) => {
    const s = startWithFixture("tool-call", { claude, launch: { hostTools: [], chatToolsDirectory: dir } });
    try {
      await s.host.ready();
      const argv = (s.fakeStart()?.argv ?? []) as string[];
      const init = s.received().find((r) => r.type === "control_request" && (r.request as Json).subtype === "initialize")?.request as Json;
      return { hooks: JSON.stringify(init.hooks), append: argv[argv.indexOf("--append-system-prompt") + 1] ?? "" };
    } finally {
      await s.done();
    }
  };
  try {
    const on = await run({ subagentSoftTurns: 60 });
    assert.ok(on.hooks.includes("helm-subagent-budget"));
    assert.match(on.append, /about 60 tool calls/);
    assert.match(on.append, /short final reports/);
    for (const claude of [{ subagentSoftTurns: 0 }, {}]) {
      const off = await run(claude);
      assert.ok(!off.hooks.includes("helm-subagent-budget") && !off.append.includes("Sub-agents:"), JSON.stringify(claude));
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the landing guard asks the host before Bash and MCP calls, and fails closed only where it is in force", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-guard-w-"));
  const s = startWithFixture("landing-guard", { launch: { hostTools: [LIST_CHATS], chatToolsDirectory: dir }, limits: { guardTimeoutMs: 300 } });
  try {
    await s.host.ready();
    const init = s.received().find((r) => r.type === "control_request" && (r.request as Json).subtype === "initialize")?.request as Json;
    assert.ok(((init.hooks as Json).PreToolUse as Json[]).some((m) => m.matcher === "Bash|PowerShell|mcp__.*" && (m.hookCallbackIds as string[])[0] === "helm-landing-guard"));
    await prompt(s.host, "land it");
    // Play the host: the first three checks get answers, the fourth none (the guard was in force),
    // the fifth says the guard is off for this chat, the sixth again none.
    const answers: (Json | undefined)[] = [
      { allow: false, reason: "Helm refused: landing on main.", active: true },
      { allow: true, context: "Helm recorded this override.", active: true },
      { allow: true, active: true },
      undefined,
      { allow: true, active: false },
      undefined,
    ];
    const checks: Json[] = [];
    for (const a of answers) {
      const c = await s.host.waitFor((e) => e.type === "guard_check" && !checks.includes(e), 5000, 0);
      checks.push(c);
      if (a) s.host.send({ type: "guard_verdict", id: c.id, ...a });
    }
    await s.host.waitForType("agent_settled", 8000, 0);
    assert.deepEqual(
      checks.map((c) => [c.kind, c.root, c.command ?? c.tool]),
      [
        ["bash", true, "git push origin HEAD:main"],
        ["bash", false, "git push origin HEAD:main"],
        ["mcp", true, "mcp__github__merge_pull_request"],
        ["bash", true, "git status"],
        ["bash", true, "git status"],
        ["bash", true, "git status"],
      ],
    );
    assert.equal(checks[0].cwd, "C:/w");
    assert.deepEqual(checks[2].input, { owner: "o", repo: "r", pullNumber: 7 });
    assert.ok(checks.every((c) => c.turn === checks[0].turn), "one turn");
    const replies = s
      .received()
      .filter((r) => r.type === "control_response" && String((r.response as Json)?.request_id ?? "").startsWith("fake-hook-"))
      .map((r) => (((r.response as Json).response as Json).hookSpecificOutput as Json | undefined) ?? {});
    assert.deepEqual(
      replies.map((h) => h.permissionDecision ?? (h.additionalContext ? "note" : "allow")),
      ["deny", "note", "allow", "deny", "allow", "allow"],
    );
    assert.equal(replies[0].permissionDecisionReason, "Helm refused: landing on main.");
    assert.match(String(replies[3].permissionDecisionReason), /did not answer the landing guard in time/);
  } finally {
    await s.done();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
