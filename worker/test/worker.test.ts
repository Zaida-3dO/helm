// End-to-end worker tests: fake host <-> worker <-> fake claude, one fixture each.

import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { isAlive } from "../src/process/kill.ts";
import { prompt, sleep, startWithFixture, uuid, waitReceived } from "./helpers.ts";

const isUser = (r: Record<string, unknown>) => r.type === "user";
const ctl = (subtype: string) => (r: Record<string, unknown>) =>
  r.type === "control_request" && (r.request as Record<string, unknown>)?.subtype === subtype;

describe("launch and handshake", () => {
  test("get_state answers once the CLI is initialised, with the model catalogue", async () => {
    const s = startWithFixture("streaming");
    try {
      const r = await s.host.ready();
      assert.equal(r.success, true);
      const data = r.data as Record<string, unknown>;
      assert.equal(data.harness, "claude");
      assert.equal(data.executableSource, "launch");
      assert.equal(data.effort, "xhigh");
      assert.deepEqual((data.models as { id: string }[]).map((m) => m.id), ["default", "haiku"]);
      assert.ok(s.received().some(ctl("initialize")), "initialize control request sent");
    } finally {
      await s.done();
    }
  });

  test("a prompt before get_state is refused and nothing reaches Claude", async () => {
    const s = startWithFixture("streaming");
    try {
      const r = await prompt(s.host, "too early");
      assert.equal(r.success, false);
      assert.match(String(r.error), /get_state/);
      await s.host.ready();
      assert.equal(s.received().filter(isUser).length, 0);
    } finally {
      await s.done();
    }
  });

  test("the fake claude sees the stream-json flags and the stripped environment", async () => {
    const s = startWithFixture("streaming", {
      env: { ANTHROPIC_API_KEY: "sk-test", CLAUDE_CODE_OAUTH_TOKEN: "tok", ANTHROPIC_BASE_URL: "http://x", ANTHROPIC_AUTH_TOKEN: "a", CLAUDECODE: "1" },
    });
    try {
      await s.host.ready();
      const start = s.fakeStart();
      const keys = (start?.envKeys as string[]).map((k) => k.toUpperCase());
      for (const k of ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDECODE"]) {
        assert.equal(keys.includes(k), false, `${k} must not reach claude`);
      }
      assert.ok(keys.includes("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"));
      assert.ok(keys.includes("DISABLE_AUTOUPDATER"));
      const argv = start?.argv as string[];
      assert.ok(argv.includes("--replay-user-messages"));
      assert.ok(argv.includes("--include-partial-messages"));
    } finally {
      await s.done();
    }
  });

  test("API-key mode passes the credential variables through", async () => {
    const s = startWithFixture("streaming", { claude: { apiKeyMode: true }, env: { ANTHROPIC_API_KEY: "sk-test", CLAUDECODE: "1" } });
    try {
      await s.host.ready();
      const keys = (s.fakeStart()?.envKeys as string[]).map((k) => k.toUpperCase());
      assert.ok(keys.includes("ANTHROPIC_API_KEY"));
      assert.ok(keys.includes("CLAUDECODE"));
    } finally {
      await s.done();
    }
  });

  test("a .cmd claude command is refused: get_state fails, nothing spawned", async () => {
    const s = startWithFixture("streaming", { claude: { command: "C:\\tools\\claude.cmd", commandArgs: [] } });
    try {
      const r = await s.host.ready();
      assert.equal(r.success, false);
      assert.match(String(r.error), /wrapper script/);
      assert.equal(s.fakeStart(), undefined);
    } finally {
      await s.done();
    }
  });

  test("a non-JSON command line fails the run", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      s.host.sendRaw("this is not json\n");
      const err = await s.host.waitForType("worker_error", 5000, 0);
      assert.match(String(err.error), /not a JSON object/);
      const exit = await s.host.exited;
      assert.equal(exit.code, 1);
    } finally {
      await s.done();
    }
  });

  test("an oversize command line fails the run as 'too long'", async () => {
    const s = startWithFixture("streaming", { limits: { maxRecordBytes: 4096 } });
    try {
      await s.host.ready();
      s.host.sendRaw(JSON.stringify({ type: "prompt", id: "x", submissionId: uuid(), text: "y".repeat(5000) }) + "\n");
      const err = await s.host.waitForType("worker_error", 5000, 0);
      assert.match(String(err.error), /too long/);
      assert.equal((await s.host.exited).code, 1);
    } finally {
      await s.done();
    }
  });

  test("unknown commands get exactly one failure response; commands such as compact wait for get_state", async () => {
    const s = startWithFixture("streaming");
    try {
      const early = await s.host.request({ type: "compact" });
      assert.match(String(early.error), /not ready/);
      await s.host.ready();
      const a = await s.host.request({ type: "frobnicate" });
      assert.equal(a.success, false);
      assert.match(String(a.error), /unknown command/);
    } finally {
      await s.done();
    }
  });
});

describe("turns and transcript events", () => {
  test("streaming: lifecycle, deltas, usage and a clean settle", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const sid = uuid();
      const r = await prompt(s.host, "hi", {}, sid);
      assert.equal(r.success, true);
      assert.deepEqual(r.data, { submissionId: sid, state: "accepted" });
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "complete");
      assert.equal(settled.sessionId, "fake-session");
      const types = s.host.events.map((e) => e.type);
      assert.ok(types.indexOf("agent_start") < types.indexOf("message_start"));
      const deltas = s.host.ofType("message_update").map((e) => (e.delta as { type: string; text: string }));
      assert.deepEqual(deltas, [
        { type: "thinking_delta", text: "Pondering" },
        { type: "text_delta", text: "Hello " },
        { type: "text_delta", text: "world" },
      ]);
      const ends = s.host.ofType("message_end").map((e) => e.message as Record<string, unknown>);
      const user = ends.find((m) => m.role === "user");
      assert.equal(user?.submissionId, sid);
      const asst = ends.filter((m) => m.role === "assistant");
      assert.equal(asst.length, 1, "the whole-message record does not duplicate the streamed one");
      assert.equal(asst[0].status, "complete");
      assert.deepEqual(asst[0].content, [{ type: "thinking", thinking: "Pondering" }, { type: "text", text: "Hello world" }]);
      const usage = s.host.ofType("usage")[0];
      assert.equal(usage.inputTokens, 10);
      assert.equal(usage.outputTokens, 5);
      assert.equal(usage.cacheReadTokens, 2);
      assert.equal(s.host.ofType("permission_mode")[0]?.mode, "default");
      assert.equal(s.host.ofType("native_session")[0]?.sessionId, "fake-session");
    } finally {
      await s.done();
    }
  });

  test("tool-call: start and end events, arguments from deltas, subagent traffic filtered", async () => {
    const s = startWithFixture("tool-call");
    try {
      await s.host.ready();
      await prompt(s.host, "read it");
      await s.host.waitForType("agent_settled", 5000, 0);
      const start = s.host.ofType("tool_execution_start")[0];
      assert.equal(start.toolCallId, "tu_1");
      assert.equal(start.toolName, "Read");
      assert.deepEqual(start.args, { file_path: "a.txt" });
      const end = s.host.ofType("tool_execution_end")[0];
      assert.equal(end.toolCallId, "tu_1");
      assert.equal(end.isError, false);
      assert.equal((end.result as { text: string }).text, "file contents");
      const toolMsg = s.host.ofType("message_end").map((e) => e.message as Record<string, unknown>).find((m) => m.id === "msg_t");
      assert.deepEqual(toolMsg?.content, [{ type: "toolCall", id: "tu_1", name: "Read", arguments: { file_path: "a.txt" } }]);
      const texts = JSON.stringify(s.host.events);
      assert.equal(texts.includes("subagent chatter"), false);
    } finally {
      await s.done();
    }
  });

  test("retry-recovered: a retry that later succeeds ends 'recovered'", async () => {
    const s = startWithFixture("retry-recovered");
    try {
      await s.host.ready();
      await prompt(s.host, "go");
      await s.host.waitForType("agent_settled", 5000, 0);
      const start = s.host.ofType("auto_retry_start");
      assert.equal(start.length, 1);
      assert.equal(start[0].attempt, 1);
      assert.equal(start[0].errorStatus, 529);
      assert.deepEqual(s.host.ofType("auto_retry_end").map((e) => e.state), ["recovered"]);
    } finally {
      await s.done();
    }
  });

  test("retry-failed: the first retry is 'retried', the run ends 'failed' and settles in error", async () => {
    const s = startWithFixture("retry-failed");
    try {
      await s.host.ready();
      await prompt(s.host, "go");
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "error");
      assert.deepEqual(s.host.ofType("auto_retry_end").map((e) => e.state), ["retried", "failed"]);
      const err = s.host.ofType("message_end").map((e) => e.message as Record<string, unknown>).find((m) => m.status === "error");
      assert.match(String(err?.errorMessage), /500/);
    } finally {
      await s.done();
    }
  });
});

describe("submissions", () => {
  test("dispatching the same submission id twice sends it once; both get the same answer", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const sid = uuid();
      const [a, b] = await Promise.all([prompt(s.host, "hi", {}, sid), prompt(s.host, "hi", {}, sid)]);
      assert.equal(a.success, true);
      assert.deepEqual(a.data, b.data);
      await s.host.waitForType("agent_settled", 5000, 0);
      const c = await prompt(s.host, "hi", {}, sid);
      assert.equal(c.success, true);
      await sleep(100);
      const users = s.received().filter(isUser);
      assert.equal(users.length, 1);
      assert.equal(users[0].uuid, sid, "the submission id is the Claude message UUID");
    } finally {
      await s.done();
    }
  });

  test("a context block rides as its own text block after the message", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const r = await prompt(s.host, "look", { context: "Looking at: src/a.rs" });
      assert.equal(r.success, true);
      const user = await waitReceived(s, isUser);
      const content = (user.message as { content: { type: string; text: string }[] }).content;
      assert.deepEqual(content, [
        { type: "text", text: "look" },
        { type: "text", text: "Looking at: src/a.rs" },
      ]);
    } finally {
      await s.done();
    }
  });

  test("text over 2 MB is refused; exactly 2 MB is not", async () => {
    const s = startWithFixture("no-ack", { limits: { promptAckMs: 200 } });
    try {
      await s.host.ready();
      const over = await prompt(s.host, "a".repeat(2 * 1024 * 1024 + 1));
      assert.equal(over.success, false);
      assert.match(String(over.error), /2 MB/);
      assert.equal(over.uncertain, undefined);
      const exact = await prompt(s.host, "a".repeat(2 * 1024 * 1024));
      assert.equal(exact.uncertain, true, "accepted for dispatch (then unacknowledged by this fixture)");
    } finally {
      await s.done();
    }
  });

  test("no-ack: no acknowledgement in time makes the prompt uncertain, and it is never resent", async () => {
    const s = startWithFixture("no-ack", { limits: { promptAckMs: 300 } });
    try {
      await s.host.ready();
      const sid = uuid();
      const r = await prompt(s.host, "hello?", {}, sid);
      assert.equal(r.success, false);
      assert.equal(r.uncertain, true);
      const again = await prompt(s.host, "hello?", {}, sid);
      assert.equal(again.uncertain, true);
      await sleep(400);
      assert.equal(s.received().filter(isUser).length, 1);
    } finally {
      await s.done();
    }
  });

  test("a submission id that is not a UUID is refused", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const r = await s.host.request({ type: "prompt", submissionId: "not-a-uuid", text: "x" });
      assert.equal(r.success, false);
    } finally {
      await s.done();
    }
  });
});

describe("interactions", () => {
  async function approvalRun(choice: string) {
    const s = startWithFixture("approval");
    await s.host.ready();
    await prompt(s.host, "list files");
    const req = await s.host.waitFor((e) => e.type === "extension_ui_request" && e.method === "select", 5000, 0);
    s.host.send({ type: "extension_ui_response", id: req.id, value: choice });
    const answer = await waitReceived(s, (r) => r.type === "control_response");
    const settled = await s.host.waitForType("agent_settled", 5000, 0);
    return { s, req, answer: (answer.response as { response: Record<string, unknown> }).response, settled };
  }

  test("approval offers once, session, deny and stop, with context rows", async () => {
    const { s, req, answer, settled } = await approvalRun("Allow once");
    try {
      assert.equal(req.attention, "approval");
      assert.equal(req.title, "Allow Bash?");
      assert.deepEqual(req.options, ["Allow once", "Allow for this session", "Deny", "Stop turn"]);
      const ctx = req.context as { label: string; text: string }[];
      assert.deepEqual(ctx.find((r) => r.label === "Command")?.text, "ls -la");
      assert.equal(answer.behavior, "allow");
      assert.deepEqual(answer.updatedInput, { command: "ls -la", description: "List files" });
      assert.equal(answer.updatedPermissions, undefined);
      assert.equal(settled.outcome, "complete");
    } finally {
      await s.done();
    }
  });

  test("allow for this session narrows the CLI's suggestion to the session", async () => {
    const { s, answer } = await approvalRun("Allow for this session");
    try {
      assert.equal(answer.behavior, "allow");
      const perms = answer.updatedPermissions as Record<string, unknown>[];
      assert.equal(perms.length, 1);
      assert.equal(perms[0].destination, "session");
      assert.equal(perms[0].type, "addRules");
    } finally {
      await s.done();
    }
  });

  test("deny answers deny without interrupting", async () => {
    const { s, answer, settled } = await approvalRun("Deny");
    try {
      assert.equal(answer.behavior, "deny");
      assert.equal(answer.interrupt, undefined);
      assert.equal(settled.outcome, "complete");
    } finally {
      await s.done();
    }
  });

  test("stop turn denies with interrupt and the run settles as stopped", async () => {
    const { s, answer, settled } = await approvalRun("Stop turn");
    try {
      assert.equal(answer.behavior, "deny");
      assert.equal(answer.interrupt, true);
      assert.equal(settled.outcome, "stopped");
    } finally {
      await s.done();
    }
  });

  test("an answer that was not offered is a deny", async () => {
    const { s, answer } = await approvalRun("Yes please");
    try {
      assert.equal(answer.behavior, "deny");
    } finally {
      await s.done();
    }
  });

  test("approval-large: details over 64 KiB offer only deny or stop", async () => {
    const s = startWithFixture("approval-large");
    try {
      await s.host.ready();
      await prompt(s.host, "write it");
      const req = await s.host.waitForType("extension_ui_request", 5000, 0);
      assert.deepEqual(req.options, ["Deny", "Stop turn"]);
      assert.match(String(req.message), /64 KiB/);
      s.host.send({ type: "extension_ui_response", id: req.id, value: "Allow once" });
      const answer = await waitReceived(s, (r) => r.type === "control_response");
      assert.equal(((answer.response as Record<string, unknown>).response as Record<string, unknown>).behavior, "deny");
    } finally {
      await s.done();
    }
  });

  test("question: one step at a time, goto revisits a step, answers go back to Claude", async () => {
    const s = startWithFixture("question");
    try {
      await s.host.ready();
      await prompt(s.host, "ask me");
      const q1 = await s.host.waitForType("extension_ui_request", 5000, 0);
      assert.equal(q1.attention, "question");
      assert.equal(q1.title, "Which colour?");
      assert.deepEqual(q1.options, ["Red", "Blue"]);
      assert.equal(q1.step, 0);
      assert.equal(q1.steps, 2);
      assert.equal(q1.allowOther, true);
      s.host.send({ type: "extension_ui_response", id: q1.id, value: "Red" });
      const q2 = await s.host.waitFor((e) => e.type === "extension_ui_request" && e.step === 1);
      assert.equal(q2.title, "Which size?");
      s.host.send({ type: "extension_ui_response", id: q2.id, goto: 0 });
      const back = await s.host.waitFor((e) => e.type === "extension_ui_request" && e.step === 0);
      assert.equal(back.prefill, "Red");
      s.host.send({ type: "extension_ui_response", id: q2.id, value: "stale" }); // old id: ignored
      s.host.send({ type: "extension_ui_response", id: back.id, value: "Blue" });
      const q2b = await s.host.waitFor((e) => e.type === "extension_ui_request" && e.step === 1);
      s.host.send({ type: "extension_ui_response", id: q2b.id, value: "Large" });
      const answer = await waitReceived(s, (r) => r.type === "control_response");
      const resp = (answer.response as Record<string, unknown>).response as Record<string, unknown>;
      assert.equal(resp.behavior, "allow");
      assert.deepEqual((resp.updatedInput as Record<string, unknown>).answers, { "Which colour?": "Blue", "Which size?": "Large" });
    } finally {
      await s.done();
    }
  });

  test("question-too-many: more than 4 questions is denied without asking", async () => {
    const s = startWithFixture("question-too-many");
    try {
      await s.host.ready();
      await prompt(s.host, "ask me");
      const answer = await waitReceived(s, (r) => r.type === "control_response");
      const resp = (answer.response as Record<string, unknown>).response as Record<string, unknown>;
      assert.equal(resp.behavior, "deny");
      assert.match(String(resp.message), /4 questions/);
      await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(s.host.ofType("extension_ui_request").length, 0);
    } finally {
      await s.done();
    }
  });

  for (const [label, response, behavior] of [
    ["approve", { confirmed: true }, "allow"],
    ["keep planning", { value: "Keep planning" }, "deny"],
  ] as const) {
    test(`plan: ${label}`, async () => {
      const s = startWithFixture("plan");
      try {
        await s.host.ready();
        await prompt(s.host, "plan it");
        const req = await s.host.waitForType("extension_ui_request", 5000, 0);
        assert.equal(req.attention, "plan");
        assert.equal(req.method, "confirm");
        assert.deepEqual(req.options, ["Approve plan", "Keep planning"]);
        assert.equal((req.context as { markdown: boolean }[])[0].markdown, true);
        s.host.send({ type: "extension_ui_response", id: req.id, ...response });
        const answer = await waitReceived(s, (r) => r.type === "control_response");
        assert.equal(((answer.response as Record<string, unknown>).response as Record<string, unknown>).behavior, behavior);
      } finally {
        await s.done();
      }
    });
  }
});

describe("stop, crash and bad records", () => {
  test("abort: interrupt is sent, the error-labelled result settles as stopped", async () => {
    const s = startWithFixture("abort");
    try {
      await s.host.ready();
      await prompt(s.host, "work");
      await s.host.waitForType("message_update", 5000, 0);
      const r = await s.host.request({ type: "abort" });
      assert.equal(r.success, true);
      assert.ok(s.received().some(ctl("interrupt")));
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "stopped");
      const asst = s.host.ofType("message_end").map((e) => e.message as Record<string, unknown>).find((m) => m.role === "assistant");
      assert.equal(asst?.status, "interrupted");
    } finally {
      await s.done();
    }
  });

  test("abort when idle answers without touching Claude", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const r = await s.host.request({ type: "abort" });
      assert.equal((r.data as Record<string, unknown>).state, "idle");
      assert.equal(s.received().some(ctl("interrupt")), false);
    } finally {
      await s.done();
    }
  });

  test("abort-hang: no settle after stop means the tree is killed and the run ends interrupted", async () => {
    const s = startWithFixture("abort-hang", { limits: { stopSettleMs: 500 } });
    try {
      await s.host.ready();
      await prompt(s.host, "work");
      await s.host.waitForType("message_start", 5000, 0);
      const fakePid = s.fakeStart()?.pid as number;
      assert.ok(isAlive(fakePid));
      await s.host.request({ type: "abort" });
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "interrupted");
      await s.host.waitForType("worker_error", 5000, 0);
      assert.equal((await s.host.exited).code, 1);
      await sleep(300);
      assert.equal(isAlive(fakePid), false, "claude process tree is gone");
    } finally {
      await s.done();
    }
  });

  test("crash: claude dying mid-turn interrupts the row and fails the worker", async () => {
    const s = startWithFixture("crash");
    try {
      await s.host.ready();
      await prompt(s.host, "work");
      const err = await s.host.waitForType("worker_error", 5000, 0);
      assert.match(String(err.error), /exited unexpectedly \(code 3\)/);
      const asst = s.host.ofType("message_end").map((e) => e.message as Record<string, unknown>).find((m) => m.role === "assistant");
      assert.equal(asst?.status, "interrupted");
      assert.deepEqual(asst?.content, [{ type: "text", text: "Half a thou" }]);
      assert.equal(s.host.ofType("agent_settled")[0]?.outcome, "interrupted");
      assert.equal((await s.host.exited).code, 1);
    } finally {
      await s.done();
    }
  });

  test("torn-line: a non-JSON line from claude fails the run", async () => {
    const s = startWithFixture("torn-line");
    try {
      await s.host.ready();
      await prompt(s.host, "go");
      const err = await s.host.waitForType("worker_error", 5000, 0);
      assert.match(String(err.error), /bad record: record is not a JSON object/);
      assert.equal((await s.host.exited).code, 1);
    } finally {
      await s.done();
    }
  });

  test("truncated: claude ending mid-line is reported as truncated", async () => {
    const s = startWithFixture("truncated");
    try {
      await s.host.ready();
      await prompt(s.host, "go");
      const err = await s.host.waitForType("worker_error", 5000, 0);
      assert.match(String(err.error), /truncated/);
    } finally {
      await s.done();
    }
  });

  test("oversize: a record from claude over the limit fails the run as too long", async () => {
    const s = startWithFixture("oversize", { limits: { maxRecordBytes: 4096 } });
    try {
      await s.host.ready();
      await prompt(s.host, "go");
      const err = await s.host.waitForType("worker_error", 5000, 0);
      assert.match(String(err.error), /too long/);
      assert.equal((await s.host.exited).code, 1);
    } finally {
      await s.done();
    }
  });

  test("shutdown ends claude and the worker cleanly", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const fakePid = s.fakeStart()?.pid as number;
      s.host.send({ type: "shutdown" });
      const exit = await s.host.exited;
      assert.equal(exit.code, 0);
      await sleep(200);
      assert.equal(isAlive(fakePid), false);
    } finally {
      await s.done();
    }
  });
});

describe("model and effort", () => {
  test("idle changes are sent to Claude and acknowledged", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const t0 = Date.now();
      const m = await s.host.request({ type: "set_model", model: "sonnet" });
      const e = await s.host.request({ type: "set_thinking_level", level: "max" });
      assert.ok(Date.now() - t0 < 10_000);
      assert.equal(m.success, true);
      assert.equal(e.success, true);
      const sm = s.received().find(ctl("set_model"));
      assert.equal((sm?.request as Record<string, unknown>).model, "sonnet");
      const ef = s.received().find(ctl("apply_flag_settings"));
      assert.deepEqual((ef?.request as Record<string, unknown>).settings, { effortLevel: "max" });
    } finally {
      await s.done();
    }
  });

  test("an unknown effort is refused", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const r = await s.host.request({ type: "set_thinking_level", level: "ultra" });
      assert.equal(r.success, false);
    } finally {
      await s.done();
    }
  });

  test("two-turns: changes during a turn are acknowledged at once and applied before the next prompt", async () => {
    const s = startWithFixture("two-turns");
    try {
      await s.host.ready();
      await prompt(s.host, "first");
      await s.host.waitForType("message_start", 5000, 0);
      const m = await s.host.request({ type: "set_model", model: "opus" });
      const e = await s.host.request({ type: "set_thinking_level", level: "low" });
      assert.deepEqual(m.data, { model: "opus", applies: "next_turn" });
      assert.deepEqual(e.data, { effort: "low", applies: "next_turn" });
      assert.equal(s.received().some(ctl("set_model")), false, "nothing changes mid-turn");
      await s.host.waitForType("agent_settled", 5000, 0);
      const r2 = await prompt(s.host, "second");
      assert.equal(r2.success, true);
      const rec = s.received();
      const iModel = rec.findIndex(ctl("set_model"));
      const iEffort = rec.findIndex(ctl("apply_flag_settings"));
      const users = rec.map((r, i) => (isUser(r) ? i : -1)).filter((i) => i >= 0);
      assert.equal(users.length, 2);
      assert.ok(iModel > users[0] && iModel < users[1], "model applied between the turns");
      assert.ok(iEffort > users[0] && iEffort < users[1], "effort applied between the turns");
      const second = await s.host.waitFor((e) => e.type === "agent_settled", 5000, s.host.events.indexOf(s.host.ofType("agent_settled")[0]) + 1);
      assert.equal(second.outcome, "complete");
    } finally {
      await s.done();
    }
  });
});

test("every fake claude fixture is used by a worker test or a fake test", async () => {
  const fs = await import("node:fs");
  const path = await import("node:path");
  const { FIXTURES } = await import("./helpers.ts");
  const { fileURLToPath } = await import("node:url");
  const dirs = [path.dirname(fileURLToPath(import.meta.url)), path.resolve(FIXTURES, "../../test")];
  const src = dirs
    .flatMap((d) => fs.readdirSync(d).filter((n: string) => n.endsWith(".test.ts")).map((n: string) => fs.readFileSync(path.join(d, n), "utf8")))
    .join("\n");
  for (const f of fs.readdirSync(FIXTURES).filter((n: string) => n.endsWith(".json"))) {
    const name = path.basename(f, ".json");
    const used = src.includes(`startWithFixture("${name}"`) || src.includes(`new Fake("${name}"`) || src.includes(`fixtures/${name}.json`);
    assert.ok(used, `fixture ${name} has no test`);
  }
});
