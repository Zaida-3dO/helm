// Native subagents: the translator turns records carrying
// parent_tool_use_id and the CLI's task records into subagent_start / subagent_step /
// subagent_end, keyed by the parent's Agent tool call, and keeps them out of the transcript.

import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { SubagentBudget } from "../src/drivers/claude/limits.ts";
import { StreamTranslator, assignmentPreview } from "../src/drivers/claude/translate.ts";
import type { Json } from "../src/protocol/types.ts";
import { prompt, startWithFixture, waitReceived } from "./helpers.ts";

function make(stepMs = 100) {
  const events: Json[] = [];
  let clock = 1_000;
  const t = new StreamTranslator({ emit: (e) => events.push(e), diag: () => {}, now: () => clock, subagentStepMs: stepMs });
  return {
    t,
    events,
    advance: (ms: number) => {
      clock += ms;
    },
    of: (type: string) => events.filter((e) => e.type === type),
  };
}

const agentCall = (id: string, input: Json): Json => ({
  type: "assistant",
  message: { id: `m_${id}`, content: [{ type: "tool_use", id, name: "Agent", input }] },
  parent_tool_use_id: null,
});
const subTool = (parent: string, id: string, name: string, input: Json): Json => ({
  type: "assistant",
  message: { id: `sm_${id}`, content: [{ type: "tool_use", id, name, input }] },
  parent_tool_use_id: parent,
});
const subText = (parent: string, msgId: string, text: string): Json => ({
  type: "assistant",
  message: { id: msgId, content: [{ type: "text", text }] },
  parent_tool_use_id: parent,
});
const toolResult = (id: string, text: string, isError = false): Json => ({
  type: "user",
  message: { content: [{ type: "tool_result", tool_use_id: id, content: [{ type: "text", text }], is_error: isError }] },
  parent_tool_use_id: null,
});
const task = (subtype: string, fields: Json): Json => ({ type: "system", subtype, ...fields });
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("subagent translation", () => {
  test("the Agent call starts a subagent with its type, name and the assignment's first lines", () => {
    const s = make();
    s.t.handle(agentCall("sa_1", { description: "Echo alpha", prompt: "Line one\n\nLine two\nLine three\nLine four", subagent_type: "reviewer" }));
    const [start] = s.of("subagent_start");
    assert.ok(start, "subagent_start emitted");
    assert.equal(start.toolCallId, "sa_1");
    assert.equal(start.agentType, "reviewer");
    assert.equal(start.name, "Echo alpha");
    assert.equal(start.description, "Echo alpha");
    assert.equal(start.assignment, "Line one\nLine two\nLine three");
    assert.equal(start.startedAt, 1_000);
    // The call itself is still an ordinary tool row.
    assert.equal(s.of("tool_execution_start").length, 1);
    // Any other tool starts no subagent.
    s.t.handle({ type: "assistant", message: { id: "m2", content: [{ type: "tool_use", id: "b1", name: "Bash", input: { command: "ls" } }] }, parent_tool_use_id: null });
    assert.equal(s.of("subagent_start").length, 1);
  });

  test("an assignment preview is bounded", () => {
    assert.equal(assignmentPreview("x".repeat(1000)).length, 400);
    assert.equal(assignmentPreview(""), "");
  });

  test("records inside a subagent become paced steps and never transcript rows", async () => {
    const s = make(100);
    s.t.handle(agentCall("sa_1", { description: "d", prompt: "p" }));
    const before = s.events.length;
    s.t.handle(subTool("sa_1", "x1", "Bash", { command: "npm test\nsecond line" }));
    s.t.handle(subTool("sa_1", "x2", "Read", { file_path: "src/a.ts" }));
    s.t.handle(subText("sa_1", "sm_t", "Looking at the failing test now.\nMore detail."));
    const after = s.events.slice(before);
    assert.deepEqual(
      after.map((e) => e.type),
      ["subagent_step"],
      "only the first step goes out at once; nothing reaches the transcript",
    );
    assert.deepEqual(after[0], { type: "subagent_step", toolCallId: "sa_1", tool: "Bash", text: "npm test" });
    s.advance(100);
    await wait(150);
    const steps = s.of("subagent_step");
    assert.equal(steps.length, 2, "the rest coalesce into one trailing step");
    assert.deepEqual(steps[1], { type: "subagent_step", toolCallId: "sa_1", text: "Looking at the failing test now." });
    // A record for an unknown parent (a nested agent) is ignored.
    s.t.handle(subTool("zz", "x3", "Bash", { command: "ls" }));
    s.advance(500);
    await wait(150);
    assert.equal(s.of("subagent_step").length, 2);
    assert.deepEqual(
      s.of("message_start").map((e) => (e.message as Json).id),
      ["m_sa_1"],
      "the only message row is the parent's Agent call",
    );
    assert.equal(s.of("tool_execution_start").length, 1, "only the Agent call is a tool row");
  });

  test("a background agent outlives its call, carries its usage and ends on its task notification", () => {
    const s = make(0);
    s.t.handle(agentCall("sa_1", { description: "d", prompt: "p" }));
    s.t.handle(task("task_started", { task_id: "ag_1", tool_use_id: "sa_1", task_type: "local_agent", is_backgrounded: true }));
    assert.equal(s.t.subagentTaskId("sa_1"), "ag_1");
    s.t.handle(toolResult("sa_1", "Async agent launched successfully."));
    assert.equal(s.of("subagent_end").length, 0, "the launch receipt is not the end");
    assert.equal(s.t.runningSubagents, 1);
    s.t.handle(task("task_progress", { task_id: "ag_1", tool_use_id: "sa_1", description: "Running tests", usage: { total_tokens: 900, tool_uses: 2, duration_ms: 100 } }));
    assert.deepEqual(s.of("subagent_step").at(-1), { type: "subagent_step", toolCallId: "sa_1", text: "Running tests", tokens: 900, toolUses: 2 });
    s.t.handle(subText("sa_1", "final", "All green."));
    s.t.handle(task("task_notification", { task_id: "ag_1", tool_use_id: "sa_1", status: "completed", summary: "short", usage: { total_tokens: 1500, tool_uses: 3, duration_ms: 4200 } }));
    const [end] = s.of("subagent_end");
    assert.deepEqual(end, { type: "subagent_end", toolCallId: "sa_1", status: "completed", durationMs: 4200, report: "All green.", tokens: 1500, toolUses: 3 });
    assert.equal(s.t.subagentTaskId("sa_1"), undefined);
    assert.equal(s.t.runningSubagents, 0);
  });

  test("task notifications map stopped and failed; the summary stands in for a missing report", () => {
    const s = make(0);
    for (const id of ["a", "b", "c"]) {
      s.t.handle(agentCall(id, { description: id, prompt: "p" }));
      s.t.handle(task("task_started", { task_id: `t_${id}`, tool_use_id: id, task_type: "local_agent", is_backgrounded: true }));
    }
    s.t.handle(task("task_notification", { task_id: "t_a", tool_use_id: "a", status: "stopped", summary: "a" }));
    s.t.handle(task("task_notification", { task_id: "t_b", tool_use_id: "b", status: "failed", summary: "Rate limited" }));
    // Found by task id alone when the notification names no tool call.
    s.t.handle(task("task_notification", { task_id: "t_c", status: "completed", summary: "Done: 3 files" }));
    const ends = s.of("subagent_end");
    assert.deepEqual(
      ends.map((e) => [e.toolCallId, e.status, e.error ?? null, e.report ?? null]),
      [
        ["a", "stopped", null, null],
        ["b", "failed", "Rate limited", null],
        ["c", "completed", null, "Done: 3 files"],
      ],
    );
  });

  test("a foreground agent ends with its call: the result is the report, an error fails it", () => {
    const s = make(0);
    s.t.handle(agentCall("ok", { description: "d", prompt: "p" }));
    s.t.handle(agentCall("bad", { description: "d", prompt: "p" }));
    s.t.handle(toolResult("ok", "Report: no defects."));
    s.t.handle(toolResult("bad", "Agent crashed: out of context", true));
    const ends = s.of("subagent_end");
    assert.deepEqual(
      ends.map((e) => [e.toolCallId, e.status, e.report ?? null, e.error ?? null]),
      [
        ["ok", "completed", "Report: no defects.", null],
        ["bad", "failed", null, "Agent crashed: out of context"],
      ],
    );
  });

  test("a turn's end stops its foreground agents but not background ones; losing the CLI ends all", () => {
    const s = make(0);
    s.t.handle(agentCall("fg", { description: "d", prompt: "p" }));
    s.t.handle(agentCall("bg", { description: "d", prompt: "p" }));
    s.t.handle(task("task_started", { task_id: "t_bg", tool_use_id: "bg", task_type: "local_agent", is_backgrounded: true }));
    s.t.onResult({ type: "result", subtype: "success" }, true);
    assert.deepEqual(
      s.of("subagent_end").map((e) => [e.toolCallId, e.status]),
      [["fg", "stopped"]],
    );
    s.t.abandon("The worker lost Claude Code.");
    assert.deepEqual(
      s.of("subagent_end").map((e) => [e.toolCallId, e.status, e.error]),
      [
        ["fg", "stopped", "Stopped by the user."],
        ["bg", "failed", "The worker lost Claude Code."],
      ],
    );
  });

  test("a foreground agent moved to the background keeps running past its call's result", () => {
    const s = make(0);
    s.t.handle(agentCall("sa", { description: "d", prompt: "p" }));
    s.t.handle(task("task_started", { task_id: "t", tool_use_id: "sa", task_type: "local_agent", is_backgrounded: false }));
    s.t.handle(task("task_updated", { task_id: "t", patch: { is_backgrounded: true } }));
    s.t.handle(toolResult("sa", "Moved to the background."));
    assert.equal(s.of("subagent_end").length, 0);
  });
});

describe("subagents through the worker (fake claude)", () => {
  test("two subagents: start, step and end events keyed by the Agent call; none of their records reach the transcript", async () => {
    const s = startWithFixture("subagents");
    try {
      await s.host.ready();
      await prompt(s.host, "dispatch two");
      await s.host.waitForType("agent_settled", 10_000, 0);
      const starts = s.host.ofType("subagent_start");
      assert.deepEqual(
        starts.map((e) => [e.toolCallId, e.agentType, e.name, e.assignment]),
        [
          ["sa_1", "general-purpose", "Echo alpha", "Run `echo alpha`.\nReport the output in one line.\nNothing else."],
          ["sa_2", "reviewer", "Review the diff", "Review the diff on this branch. List defects only."],
        ],
      );
      const steps = s.host.ofType("subagent_step");
      assert.ok(steps.some((e) => e.toolCallId === "sa_1" && e.tool === "Bash" && e.text === "echo alpha"), JSON.stringify(steps));
      assert.ok(steps.some((e) => e.toolCallId === "sa_2" && e.tool === "Read" && e.text === "src/App.tsx"), JSON.stringify(steps));
      const ends = s.host.ofType("subagent_end");
      assert.deepEqual(
        ends.map((e) => [e.toolCallId, e.status, e.report]),
        [
          ["sa_1", "completed", "The output is alpha."],
          ["sa_2", "completed", "No defects found."],
        ],
      );
      assert.equal(ends[0]!.tokens, 1500);
      assert.equal(ends[0]!.durationMs, 900);
      // The parent transcript: the two Agent calls and the final answer, nothing from inside.
      assert.deepEqual(
        s.host.ofType("tool_execution_start").map((e) => e.toolCallId),
        ["sa_1", "sa_2"],
      );
      const texts = s.host
        .ofType("message_end")
        .flatMap((e) => ((e.message as Json).content as Json[]).filter((b) => b.type === "text").map((b) => b.text));
      assert.ok(texts.includes("Both agents reported back."));
      assert.ok(!texts.includes("The output is alpha.") && !texts.includes("No defects found."), JSON.stringify(texts));
    } finally {
      await s.done();
    }
  });
});

describe("stopping one subagent", () => {
  test("stop_tool with an Agent call id sends stop_task for that agent's task; get_state reports the capability", async () => {
    const s = startWithFixture("subagent-stop");
    try {
      const ready = await s.host.ready();
      assert.equal(((ready.data as Json).capabilities as Json).stopSubagent, true);
      await prompt(s.host, "survey");
      await s.host.waitFor((e) => e.type === "subagent_start" && e.toolCallId === "sa_1", 5000, 0);
      // The task id that stop_task needs arrives with system/task_started, after the Agent call
      // that starts the card. The fixture sends the call's result after task_started, and the
      // worker reads lines in order, so the call's end means the id is known (a slow CI runner
      // sent stop_tool between the two and got stopped: false).
      await s.host.waitFor((e) => e.type === "tool_execution_end" && e.toolCallId === "sa_1", 5000, 0);
      const r = await s.host.request({ type: "stop_tool", toolCallId: "sa_1", sentAtMs: Date.now() });
      assert.deepEqual(r.data, { stopped: true });
      const st = await waitReceived(s, (x) => x.type === "control_request" && (x.request as Json)?.subtype === "stop_task");
      assert.equal((st.request as Json).task_id, "ag_1");
      const end = await s.host.waitFor((e) => e.type === "subagent_end" && e.toolCallId === "sa_1", 5000, 0);
      assert.equal(end.status, "stopped");
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "complete", "the parent turn carries on");
    } finally {
      await s.done();
    }
  });
});

describe("sub-agent limits", () => {
  const call = (parent: string, id: string, usage?: Json): Json => ({
    type: "assistant",
    message: { id: `m_${id}`, content: [{ type: "tool_use", id, name: "Read", input: {} }], ...(usage ? { usage } : {}) },
    parent_tool_use_id: parent,
  });

  test("a sub-agent is noted at its first call and at the soft limit, refused past the hard limit; the chat's own agent is never counted", () => {
    const b = new SubagentBudget({ softTurns: 10, hardStop: true });
    const step = (agent: string | undefined, id: string) => b.decide({ ...(agent ? { agent_id: agent } : {}) }, id);
    const kinds = (agent: string | undefined, n: number) => Array.from({ length: n }, (_, i) => step(agent, `${agent}-${i}`).kind);
    // The chat's own agent: 40 calls, all allowed, no notes.
    assert.deepEqual([...new Set(kinds(undefined, 40))], ["allow"]);
    // One sub-agent: first call carries the reporting note, then plain until call 10 (soft), a
    // reminder five calls on, then refused after call 13 (130%).
    const seq = kinds("a1", 15);
    assert.deepEqual(seq, ["note", ...Array(8).fill("allow"), "note", "allow", "allow", "allow", "deny", "deny"]);
    assert.match((step("a1", "x") as { text: string }).text, /hand-off/);
    // Without the hard stop (the default) a sub-agent past the limit is only reminded, never refused.
    const soft = new SubagentBudget({ softTurns: 10 });
    const softKinds = Array.from({ length: 40 }, (_, i) => soft.decide({ agent_id: "s" }, `s${i}`).kind);
    assert.ok(!softKinds.includes("deny") && softKinds.filter((k) => k === "note").length > 3);
    // Another sub-agent has its own count.
    assert.equal(step("a2", "y").kind, "note");
    // Off: nothing is ever noted or refused.
    const off = new SubagentBudget({ softTurns: 0 });
    assert.equal(off.enabled, false);
    assert.equal(off.decide({ agent_id: "a1" }, "t").kind, "allow");
  });

  test("a sub-agent past the context budget is told to hand off; the owner is found from its assistant records", () => {
    const b = new SubagentBudget({ softTurns: 100, hardStop: true });
    b.decide({ agent_id: "a1" }, "warm"); // first-call note out of the way
    b.noteAssistant(call("agent_1", "t1", { input_tokens: 10, cache_read_input_tokens: 296_000, cache_creation_input_tokens: 5_000 }));
    // No agent_id on the hook: the tool-use id still names the owner.
    const d = b.decide({}, "t1");
    assert.equal(d.kind, "note");
    assert.match((d as { text: string }).text, /301k tokens/);
    b.noteAssistant(call("agent_1", "t2", { input_tokens: 10, cache_read_input_tokens: 400_000 }));
    assert.equal(b.decide({}, "t2").kind, "deny");
  });
});
