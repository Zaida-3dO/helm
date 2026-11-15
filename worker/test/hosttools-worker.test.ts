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

test("the duplicate-server hook keeps the redirect for the chat's own agent and lets a sub-agent's native call through", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-dup-mcp-"));
  const s = startWithFixture("duplicate-mcp", { launch: { hostTools: [], chatToolsDirectory: dir } });
  try {
    await s.host.ready();
    // Helm has both of these; Claude Code loaded its own copy of each under the same name.
    s.host.send({
      type: "mcp_config",
      revision: "r1",
      servers: [
        { id: "agent_standup", name: "Agent Standup", transport: "stdio" },
        { id: "memory", name: "Memory", transport: "stdio" },
      ],
    });
    assert.equal((await prompt(s.host, "go")).success, true);
    await s.host.waitForType("agent_settled");
    const answers = s
      .received()
      .filter((r) => r.type === "control_response" && String((r.response as Json)?.request_id ?? "").startsWith("fake-hook-"))
      .map((r) => (((r.response as Json).response as Json) ?? {}) as Json);
    assert.equal(answers.length, 6, "one answer per native MCP call");
    assert.deepEqual(
      answers.map((a) => (a.hookSpecificOutput as Json | undefined)?.permissionDecision ?? "allow"),
      ["deny", "allow", "allow", "deny", "allow", "allow"],
      "only the chat's own agent is refused: a sub-agent's call (by its agent id, and by a tool call id recorded from its stream) goes through, on every duplicated server; a server only Claude Code has is untouched",
    );
    const reason = (answers[0].hookSpecificOutput as Json).permissionDecisionReason as string;
    assert.match(reason, /Agent Standup is also set up in Helm/);
    assert.match(reason, /app_mcp_tools \(server "agent_standup"\).*app_mcp_call/, "the redirect names Helm's copy");
    assert.match((answers[3].hookSpecificOutput as Json).permissionDecisionReason as string, /\(server "memory"\)/, "the rule is not limited to one server");
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

test("a slow root call moves to the background and is fetched later; sub-agents never move; the store forgets", async () => {
  const { HostToolServer } = await import("../src/hosttools/server.ts");
  const { BackgroundStore } = await import("../src/hosttools/background.ts");
  // A fake channel: each call waits for the test to settle it by its "k" argument; a cancel ends it.
  type Settle = (r: { ok: true; result: unknown }) => void;
  const open = new Map<string, { settle: Settle; signal?: AbortSignal }>();
  const sent: string[] = [];
  const channel = {
    call: (op: string, args: Json, opts: { signal?: AbortSignal }) =>
      new Promise((resolve) => {
        sent.push(op);
        open.set(String(args.k), { settle: resolve, signal: opts.signal });
        opts.signal?.addEventListener("abort", () => resolve({ ok: false, error: "cancelled", changed: "maybe" }));
      }),
  };
  const tool = (name: string, mutating: boolean) => ({ name, description: name, inputSchema: { type: "object" }, requestLimitBytes: 8192, timeoutMs: 120_000, children: false, mutating });
  const bridge = {
    enabled: () => true,
    tools: async () => ({}),
    has: (n: string) => n === "mcp_slow",
    call: (n: string, args: Json, signal?: AbortSignal) => channel.call(n, args, { signal }) as Promise<{ ok: true; result: unknown }>,
  };
  const notes: string[] = [];
  const make = (afterMs: number) =>
    new HostToolServer({ hostTools: [tool("app_slow", true), tool("app_background_result", false)], channel: channel as never, bridge, notify: (t) => notes.push(t), backgroundAfterMs: afterMs, resultWaitMs: 50 });
  const server = make(50);
  const call = async (name: string, args: Json, caller: { child: boolean; toolCallId?: string; unattributed?: boolean }, signal?: AbortSignal, on = server) => {
    const r = ((await on.handle({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, caller, signal)) as Json).result as Json;
    return { error: r.isError === true, text: String((r.content as Json[])[0].text), data: (r.structuredContent ?? {}) as Json };
  };
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  // The root agent's call: answered early, and a cancel from the CLI after that no longer reaches it.
  const cli = new AbortController();
  const moved = await call("app_slow", { k: "a" }, { child: false, toolCallId: "tu_a" }, cli.signal);
  assert.deepEqual([moved.data.state, moved.data.callId, moved.data.tool], ["background", "tu_a", "app_slow"]);
  assert.match(String(moved.data.message), /Do not call it again.*app_background_result \{"callId": "tu_a"\}.*may still change things/);
  cli.abort();
  assert.equal(open.get("a")?.signal?.aborted, false, "moving the call detached the CLI's cancel");
  // Still running: the fetch waits a little, then says so.
  const running = await call("app_background_result", { callId: "tu_a" }, { child: false, toolCallId: "tu_f1" });
  assert.equal(running.data.state, "running");
  open.get("a")?.settle({ ok: true, result: { v: 1 } });
  await sleep(0);
  assert.match(notes[0] ?? "", /^app_slow finished in the background after \d+ s\. The agent can read its result with app_background_result\.$/);
  // Through app_mcp_call too; the fetch itself is never sent to the host.
  const done = await call("app_mcp_call", { name: "app_background_result", arguments: { callId: "tu_a" } }, { child: false, toolCallId: "tu_f2" });
  assert.deepEqual(done.data, { state: "done", callId: "tu_a", tool: "app_slow", result: { v: 1 } });
  const list = await call("app_background_result", {}, { child: false, toolCallId: "tu_f3" });
  assert.deepEqual((list.data.calls as Json[]).map((c) => [c.callId, c.state, c.read]), [["tu_a", "done", true]]);
  assert.deepEqual(sent, ["app_slow"]);
  for (const [args, caller, why] of [
    [{ callId: "nope" }, { child: false, toolCallId: "tu_f4" }, /There is no background call nope/],
    [{ callId: "tu_a" }, { child: true, toolCallId: "tu_f5" }, /only available to the parent chat/],
  ] as const) {
    const r = await call("app_background_result", args, caller);
    assert.ok(r.error && why.test(r.text), r.text);
  }

  // A sub-agent's call and an unattributed one wait in the foreground, however long they run.
  for (const [k, caller] of [["c", { child: true, toolCallId: "tu_c" }], ["u", { child: true, unattributed: true }]] as const) {
    let answered = false;
    const p = call("mcp_slow", { k }, caller).then((r) => ((answered = true), r));
    await sleep(120);
    assert.equal(answered, false, `${k}: still in the foreground`);
    open.get(k)?.settle({ ok: true, result: { k } });
    assert.deepEqual((await p).data, { k });
  }
  // A root call to a bridged MCP tool moves like a host tool, and counts as one that may change things.
  const mcpMoved = await call("mcp_slow", { k: "m" }, { child: false, toolCallId: "tu_m" });
  assert.match(String(mcpMoved.data.message), /may still change things/);
  open.get("m")?.settle({ ok: true, result: {} });

  // background(): moves a running call at once (deliver_now), and only one that is running.
  const slow = make(60_000);
  const p = call("app_slow", { k: "b" }, { child: false, toolCallId: "tu_b" }, undefined, slow);
  await sleep(10);
  assert.equal(slow.background("tu_x"), false);
  assert.equal(slow.background("tu_b"), true);
  assert.equal(slow.background("tu_b"), false, "once");
  assert.equal((await p).data.state, "background");
  open.get("b")?.settle({ ok: true, result: {} });

  // The store: at most `max` calls, finished ones make room, running ones never go; results expire.
  let now = 0;
  const store = new BackgroundStore({ max: 2, readKeepMs: 10, doneKeepMs: 30, now: () => now });
  const never = new Promise<never>(() => {});
  const ok = Promise.resolve({ ok: true as const, result: 1 });
  assert.ok(store.add("r1", "t", false, 0, never));
  const d1 = store.add("d1", "t", false, 0, ok);
  await d1?.done;
  assert.ok(store.add("r2", "t", false, 0, never), "the finished result made room");
  assert.equal(store.get("d1"), undefined);
  assert.equal(store.add("r3", "t", false, 0, never), undefined, "full of running calls: the next one stays in the foreground");
  assert.equal(store.add("r1", "t", false, 0, never), undefined, "an id is taken once");
  const wide = new BackgroundStore({ readKeepMs: 10, doneKeepMs: 30, now: () => now });
  for (const id of ["read", "unread"]) await wide.add(id, "t", false, 0, ok)?.done;
  wide.markRead(wide.get("read")!);
  now = 10;
  assert.deepEqual(wide.list().map((c) => c.callId), ["unread"], "dropped 10 after it was read");
  now = 30;
  assert.deepEqual(wide.list(), [], "dropped 30 after it finished");
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
    assert.ok(((init.hooks as Json).PreToolUse as Json[]).some((m) => m.matcher === "Bash|PowerShell|Monitor|mcp__.*" && (m.hookCallbackIds as string[])[0] === "helm-landing-guard"));
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

test("a local chat lists only the core host tools, but every host tool stays callable and searchable", async () => {
  const { HostToolServer } = await import("../src/hosttools/server.ts");
  const core = { name: "app_list_chats", description: "List chats.", inputSchema: { type: "object" }, requestLimitBytes: 8192, timeoutMs: 20000, children: false, mutating: false };
  const other = { name: "app_list_schedules", description: "List the user's schedules.", inputSchema: { type: "object" }, requestLimitBytes: 8192, timeoutMs: 20000, children: false, mutating: false };
  const sent: string[] = [];
  const channel = { call: async (name: string) => (sent.push(name), { ok: true as const, result: { name } }) } as never;
  const mk = (lean: boolean) => new HostToolServer({ hostTools: [core, other], channel, lean });
  const listed = async (s: InstanceType<typeof HostToolServer>) => {
    const r = (await s.handle({ jsonrpc: "2.0", id: 1, method: "tools/list" }, { child: false })) as Json;
    return ((r.result as Json).tools as Json[]).map((t) => t.name);
  };
  assert.deepEqual(await listed(mk(true)), ["app_list_chats", "app_list_tools", "app_mcp_call"], "lean: only the core tool plus the compat tools");
  assert.deepEqual(await listed(mk(false)), ["app_list_chats", "app_list_schedules", "app_list_tools", "app_mcp_call"], "not lean: everything");
  const lean = mk(true);
  const call = await lean.call("app_mcp_call", { name: "app_list_schedules", arguments: {} }, { child: false });
  assert.equal(call.ok, true, "an unlisted host tool still answers through app_mcp_call");
  assert.deepEqual(sent, ["app_list_schedules"], "the call reached the host channel");
  const found = (await lean.handle({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "app_list_tools", arguments: { query: "schedules" } } }, { child: false })) as Json;
  const sc = (found.result as Json).structuredContent as Json;
  assert.deepEqual((sc.tools as Json[]).map((t) => t.name), ["app_list_schedules"], "app_list_tools finds what the list does not show");
  assert.equal(sc.total, 1);
});

test("a local chat is told where the rest of Helm's tools are; a Claude chat is not", async () => {
  const { LOCAL_TOOL_RULE } = await import("../src/drivers/claude/driver.ts");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-lean-"));
  const append = async (claude: Json) => {
    const s = startWithFixture("tool-call", { claude, launch: { hostTools: [], chatToolsDirectory: dir } });
    try {
      await s.host.ready();
      const argv = (s.fakeStart()?.argv ?? []) as string[];
      return argv[argv.indexOf("--append-system-prompt") + 1] ?? "";
    } finally {
      await s.done();
    }
  };
  try {
    assert.ok((await append({ local: { baseUrl: "http://127.0.0.1:9", model: "m" } })).includes(LOCAL_TOOL_RULE), "the local chat gets the line");
    assert.ok(!(await append({})).includes(LOCAL_TOOL_RULE), "a Claude chat does not");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the workflow gate hook asks the host only when the launch says so, and lets an unanswered call through", async () => {
  const hookAnswers = (got: Json[]) =>
    got
      .filter((r) => r.type === "control_response" && String((r.response as Json)?.request_id ?? "").startsWith("fake-hook-"))
      .map((r) => ((r.response as Json).response as Json) ?? {});
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-ht-g-"));
  const s = startWithFixture("workflow-gates", { env: { HELM_WORKFLOW_GATES: "1" }, launch: { hostTools: [LIST_CHATS], chatToolsDirectory: dir } });
  try {
    await s.host.ready();
    // Play the host: refuse the spawn, fail the edit question (a failure lets the call through).
    const asked: Json[] = [];
    const serve = async () => {
      for (let n = 0; n < 2; n++) {
        const r = await s.host.waitFor((e) => e.type === "chat_request" && !asked.includes(e), 10_000, 0);
        asked.push(r);
        const body = n === 0 ? { id: r.id, ok: true, result: { allow: false, reason: "the build gate needs the plan first" } } : { id: r.id, ok: false, error: "no", changed: "no" };
        fs.writeFileSync(path.join(dir, String(r.id), "response.json"), JSON.stringify(body));
      }
    };
    const served = serve();
    const resp = await prompt(s.host, "go");
    assert.equal(resp.success, true);
    await served;
    await s.host.waitForType("agent_settled");
    assert.deepEqual(
      asked.map((r) => [r.operation, r.args]),
      [
        ["app_workflow_gate", { tool: "Agent", input: { subagent_type: "coder" }, child: false }],
        ["app_workflow_gate", { tool: "Write", input: { file_path: "C:/w/a.rs" }, child: false }],
      ],
      "only what the host decides on is sent: no prompt, no file content; a sub-agent's edit is not asked",
    );
    const answers = hookAnswers(s.received());
    assert.equal(answers.length, 3);
    assert.deepEqual(answers[0], { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: "the build gate needs the plan first" } });
    assert.deepEqual(answers.slice(1), [{}, {}]);
  } finally {
    await s.done();
    fs.rmSync(dir, { recursive: true, force: true });
  }
  // Without the launch variable (Orchestrator off), nothing is registered and nothing is asked.
  const dir2 = fs.mkdtempSync(path.join(os.tmpdir(), "helm-ht-g-"));
  const off = startWithFixture("workflow-gates", { launch: { hostTools: [LIST_CHATS], chatToolsDirectory: dir2 } });
  try {
    await off.host.ready();
    assert.equal((await prompt(off.host, "go")).success, true);
    await off.host.waitForType("agent_settled");
    const init = off.received().find((r) => r.type === "control_request" && (r.request as Json).subtype === "initialize")?.request as Json;
    assert.ok(!JSON.stringify(init.hooks).includes("helm-workflow-gates"));
    assert.equal(off.host.ofType("chat_request").length, 0);
  } finally {
    await off.done();
    fs.rmSync(dir2, { recursive: true, force: true });
  }
});
