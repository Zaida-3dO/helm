// The fixture language (fake-claude.ts header): lifecycle acknowledgements, reading at a
// tool boundary, hooks, background moves, withdraw, cancel-queued, stop-task, resume, loops.
// Each test drives the fake directly, as the worker would, and checks what it writes back.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import path from "node:path";
import { describe, test } from "node:test";
import { fileURLToPath } from "node:url";

type Json = Record<string, unknown>;
const here = path.dirname(fileURLToPath(import.meta.url));
const FAKE_CLAUDE = path.resolve(here, "../claude/fake-claude.ts");
const FIXTURES = path.resolve(here, "../claude/fixtures");

class Fake {
  readonly lines: Json[] = [];
  private readonly child: ChildProcess;
  private n = 0;
  readonly closed: Promise<number | null>;

  constructor(fixture: string, flags: string[] = ["--replay-user-messages"]) {
    this.child = spawn(process.execPath, [FAKE_CLAUDE, path.join(FIXTURES, `${fixture}.json`), ...flags], { stdio: ["pipe", "pipe", "pipe"] });
    let buf = "";
    this.child.stdout!.setEncoding("utf8");
    this.child.stdout!.on("data", (d: string) => {
      buf += d;
      let i: number;
      while ((i = buf.indexOf("\n")) !== -1) {
        const l = buf.slice(0, i);
        buf = buf.slice(i + 1);
        if (l.trim()) this.lines.push(JSON.parse(l) as Json);
      }
    });
    this.closed = new Promise((r) => this.child.on("close", (c) => r(c)));
  }

  send(rec: Json): void {
    this.child.stdin!.write(JSON.stringify(rec) + "\n");
  }

  control(request: Json): string {
    const request_id = `t-${++this.n}`;
    this.send({ type: "control_request", request_id, request });
    return request_id;
  }

  init(hooks?: Json): void {
    this.control({ subtype: "initialize", ...(hooks ? { hooks } : {}) });
  }

  user(text: string, uuid: string = randomUUID()): string {
    this.send({ type: "user", message: { role: "user", content: [{ type: "text", text }] }, parent_tool_use_id: null, session_id: "", uuid });
    return uuid;
  }

  async waitFor(pred: (l: Json) => boolean, timeoutMs = 5000, from = 0): Promise<Json> {
    const end = Date.now() + timeoutMs;
    for (;;) {
      const hit = this.lines.slice(from).find(pred);
      if (hit) return hit;
      if (Date.now() > end) throw new Error(`timed out; saw ${JSON.stringify(this.lines.map((l) => l.type + (l.subtype ? "/" + String(l.subtype) : "")))}`);
      await new Promise((r) => setTimeout(r, 15));
    }
  }

  lifecycle(uuid: string): string[] {
    return this.lines.filter((l) => l.type === "command_lifecycle" && l.command_uuid === uuid).map((l) => String(l.state));
  }

  echoed(uuid: string): boolean {
    return this.lines.some((l) => l.type === "user" && l.uuid === uuid && l.isReplay === true);
  }

  async end(): Promise<void> {
    this.child.stdin!.end();
    await this.closed;
  }
}

const isResult = (l: Json) => l.type === "result";
const hookCall = (l: Json) => l.type === "control_request" && (l.request as Json)?.subtype === "hook_callback";
const answer = (id: string) => (l: Json) => l.type === "control_response" && (l.response as Json)?.request_id === id;
const textOf = (f: Fake) =>
  f.lines
    .filter((l) => l.type === "assistant")
    .flatMap((l) => ((l.message as Json).content as Json[]).filter((b) => b.type === "text").map((b) => String(b.text)))
    .join("\n");

function hookReply(f: Fake, req: Json, response: Json): void {
  f.send({ type: "control_response", response: { subtype: "success", request_id: req.request_id, response } });
}

const deny = { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: "run it in the background" } };

describe("fake claude lifecycle fixtures", () => {
  test("lifecycle: an idle message is queued, started and echoed at once; one sent mid-turn waits for the tool boundary", async () => {
    const f = new Fake("steer-read");
    try {
      f.init();
      const p = f.user("do the thing");
      await f.waitFor((l) => l.type === "assistant");
      assert.deepEqual(f.lifecycle(p), ["queued", "started"]);
      assert.ok(f.echoed(p));
      const s = f.user("also mention bananas");
      await f.waitFor((l) => l.type === "command_lifecycle" && l.command_uuid === s);
      assert.deepEqual(f.lifecycle(s), ["queued"]);
      assert.equal(f.echoed(s), false, "not read before the boundary");
      await f.waitFor(isResult);
      await f.waitFor((l) => l.type === "command_lifecycle" && l.command_uuid === p && l.state === "completed");
      assert.deepEqual(f.lifecycle(s), ["queued", "started", "completed"]);
      assert.ok(f.echoed(s));
      const resultAt = f.lines.findIndex(isResult);
      const readAt = f.lines.findIndex((l) => l.type === "user" && l.uuid === s);
      const toolResultAt = f.lines.findIndex((l) => l.type === "user" && JSON.stringify(l).includes("tool_result"));
      assert.ok(toolResultAt < readAt && readAt < resultAt, "read after the tool result, before the turn ends");
      assert.match(textOf(f), /Noted your message: also mention bananas/);
    } finally {
      await f.end();
    }
  });

  test("long-command: a denied hook re-runs the command in the background and reports the background task", async () => {
    const f = new Fake("long-command");
    try {
      f.init({ PreToolUse: [{ matcher: "Bash", hookCallbackIds: ["hk-1"] }] });
      f.user("run the tests");
      const h1 = await f.waitFor(hookCall);
      assert.equal(((h1.request as Json).callback_id), "hk-1");
      assert.deepEqual(((h1.request as Json).input as Json).tool_input, { command: "npm test" });
      hookReply(f, h1, deny);
      const h2 = await f.waitFor(hookCall, 5000, f.lines.indexOf(h1) + 1);
      hookReply(f, h2, {});
      await f.waitFor(isResult);
      const denied = f.lines.find((l) => l.type === "user" && JSON.stringify(l).includes("hook error: run it in the background"));
      assert.ok(denied, "the deny reason reaches the tool result");
      const bg = f.lines.filter((l) => l.subtype === "background_tasks_changed").map((l) => (l.tasks as unknown[]).length);
      assert.deepEqual(bg, [1, 0]);
    } finally {
      await f.end();
    }
  });

  test("long-command: an allowed hook runs in the foreground", async () => {
    const f = new Fake("long-command");
    try {
      f.init({ PreToolUse: [{ matcher: "Bash", hookCallbackIds: ["hk-1"] }] });
      f.user("run the tests");
      hookReply(f, await f.waitFor(hookCall), {});
      await f.waitFor(isResult);
      assert.match(textOf(f), /foreground/);
      assert.equal(f.lines.filter(hookCall).length, 1);
    } finally {
      await f.end();
    }
  });

  test("hooks: without a registered PreToolUse callback no hook_callback is sent", async () => {
    const f = new Fake("long-command-rules");
    try {
      f.init();
      f.user("go");
      await f.waitFor(isResult);
      assert.equal(f.lines.filter(hookCall).length, 0);
    } finally {
      await f.end();
    }
  });

  test("long-command-rules: every call asks the hook, subagent calls carry their parent", async () => {
    const f = new Fake("long-command-rules");
    try {
      f.init({ PreToolUse: [{ matcher: "Bash", hookCallbackIds: ["hk-1"] }] });
      f.user("go");
      let from = 0;
      for (let i = 0; i < 7; i++) {
        const h = await f.waitFor(hookCall, 5000, from);
        from = f.lines.indexOf(h) + 1;
        hookReply(f, h, {});
      }
      await f.waitFor(isResult);
      const sub = f.lines.find((l) => l.type === "assistant" && l.parent_tool_use_id === "a1");
      assert.ok(sub, "the subagent's Bash call names its parent");
      f.user("again");
      hookReply(f, await f.waitFor(hookCall, 5000, from), {});
      await f.waitFor((l) => isResult(l) && f.lines.filter(isResult).length === 2);
    } finally {
      await f.end();
    }
  });

  test("wait-limit: background_tasks is answered, the call returns and the waiting steer is read", async () => {
    const f = new Fake("wait-limit");
    try {
      f.init({ PreToolUse: [{ matcher: "Bash", hookCallbackIds: ["hk-1"] }] });
      f.user("start the server");
      hookReply(f, await f.waitFor(hookCall), {});
      await f.waitFor((l) => l.subtype === "task_started");
      const s = f.user("then tell me the port");
      const id = f.control({ subtype: "background_tasks", tool_use_id: "tu_1" });
      const r = await f.waitFor(answer(id));
      assert.deepEqual((r.response as Json).response, { backgrounded: true });
      await f.waitFor(isResult);
      assert.ok(f.echoed(s));
      assert.ok(f.lines.some((l) => JSON.stringify(l).includes("manually backgrounded")));
    } finally {
      await f.end();
    }
  });

  test("withdraw: cancel_async_message drops an unread steer; a read one cannot be withdrawn", async () => {
    const f = new Fake("withdraw");
    try {
      f.init();
      f.user("list files");
      await f.waitFor((l) => l.type === "assistant");
      const keep = f.user("keep me");
      const drop = f.user("drop me");
      await f.waitFor((l) => l.type === "command_lifecycle" && l.command_uuid === drop);
      const id = f.control({ subtype: "cancel_async_message", message_uuid: drop });
      assert.deepEqual(((await f.waitFor(answer(id))).response as Json).response, { cancelled: true });
      await f.waitFor(isResult);
      assert.deepEqual(f.lifecycle(drop), ["queued", "cancelled"]);
      assert.equal(f.echoed(drop), false);
      assert.ok(f.echoed(keep));
      const id2 = f.control({ subtype: "cancel_async_message", message_uuid: keep });
      assert.deepEqual(((await f.waitFor(answer(id2))).response as Json).response, { cancelled: false });
    } finally {
      await f.end();
    }
  });

  test("abort-cancel: background_tasks is auto-answered and interrupt with cancel_queued lists the dropped steers", async () => {
    const f = new Fake("abort-cancel");
    try {
      f.init();
      f.user("wait a minute");
      await f.waitFor((l) => l.subtype === "task_started");
      const s = f.user("never mind");
      await f.waitFor((l) => l.type === "command_lifecycle" && l.command_uuid === s);
      const b = f.control({ subtype: "background_tasks" });
      assert.deepEqual(((await f.waitFor(answer(b))).response as Json).response, { backgrounded: true });
      const i = f.control({ subtype: "interrupt", cancel_queued: true });
      assert.deepEqual(((await f.waitFor(answer(i))).response as Json).response, { still_queued: [], cancelled: [s] });
      await f.waitFor(isResult);
      assert.deepEqual(f.lifecycle(s), ["queued", "cancelled"]);
    } finally {
      await f.end();
    }
  });

  test("interrupt without cancel_queued lists the steers that survive", async () => {
    const f = new Fake("abort-cancel");
    try {
      f.init();
      f.user("wait a minute");
      await f.waitFor((l) => l.subtype === "task_started");
      const s = f.user("after that");
      await f.waitFor((l) => l.type === "command_lifecycle" && l.command_uuid === s);
      const i = f.control({ subtype: "interrupt" });
      assert.deepEqual(((await f.waitFor(answer(i))).response as Json).response, { still_queued: [s] });
      await f.waitFor(isResult);
      // The surviving steer opens the next turn.
      await f.waitFor((l) => l.type === "command_lifecycle" && l.command_uuid === s && l.state === "started");
    } finally {
      await f.end();
    }
  });

  test("stop-tool: stop_task is answered and the call fails as killed", async () => {
    const f = new Fake("stop-tool");
    try {
      f.init();
      f.user("wait");
      await f.waitFor((l) => l.subtype === "task_started");
      const id = f.control({ subtype: "stop_task", task_id: "bt_1" });
      await f.waitFor(answer(id));
      await f.waitFor(isResult);
      assert.ok(f.lines.some((l) => l.type === "user" && JSON.stringify(l).includes("[killed]")));
    } finally {
      await f.end();
    }
  });

  const resumeCases: [string[], RegExp][] = [
    [["--replay-user-messages", "--resume", "fake-session"], /resumed session/],
    [["--replay-user-messages"], /fresh session/],
  ];
  for (const [flags, expected] of resumeCases) {
    test(`resume: ${flags.includes("--resume") ? "with" : "without"} --resume`, async () => {
      const f = new Fake("resume", flags);
      try {
        f.init();
        f.user("continue");
        await f.waitFor(isResult);
        assert.match(textOf(f), expected);
      } finally {
        await f.end();
      }
    });
  }

  test("compact: a /compact submission reports a manual compaction boundary", async () => {
    const f = new Fake("compact");
    try {
      f.init();
      f.user("/compact");
      await f.waitFor(isResult);
      const b = f.lines.find((l) => l.subtype === "compact_boundary");
      assert.equal(((b?.compact_metadata as Json) ?? {}).trigger, "manual");
    } finally {
      await f.end();
    }
  });

  test("echo-loop: several turns in one process", async () => {
    const f = new Fake("echo-loop");
    try {
      f.init();
      for (const word of ["one", "two", "three"]) {
        f.user(word);
        await f.waitFor((l) => l.type === "assistant" && JSON.stringify(l).includes(`You said: ${word}`));
      }
      assert.equal(f.lines.filter(isResult).length, 3);
    } finally {
      await f.end();
    }
  });
});

describe("e2e fixture", () => {
  test("branches on the prompt's tag, echoes an untagged prompt, and loops over turns", async () => {
    const f = new Fake("e2e");
    try {
      f.init();
      f.user("[E1] hello");
      await f.waitFor(isResult);
      assert.match(textOf(f), /Hello from the fake Claude/);
      f.user("plain words");
      await f.waitFor((l) => l.type === "assistant" && JSON.stringify(l).includes("You said: plain words"));
      f.user("[MD] render");
      await f.waitFor((l) => l.type === "assistant" && JSON.stringify(l).includes("mermaid"));
    } finally {
      await f.end();
    }
  });

  test("a process started with --resume answers an untagged prompt as the continuation", async () => {
    const f = new Fake("e2e", ["--replay-user-messages", "--resume", "fake-session"]);
    try {
      f.init();
      f.user("The app restarted; continue.");
      await f.waitFor(isResult);
      assert.match(textOf(f), /Continued after restart/);
    } finally {
      await f.end();
    }
  });
});
