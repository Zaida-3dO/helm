// Worker behaviour against the fake claude: acknowledgement and delivery of steers,
// the wait limit, the long-command rule, withdraw / stop_tool / compact / abort,
// disallowed native tools, background tasks and stoppable calls.

import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { Json } from "../../fakes/host/fake-host.ts";
import { DISALLOWED_TOOLS } from "../src/drivers/claude/args.ts";
import { PROMPT_RULE } from "../src/drivers/claude/longcmd.ts";
import { prompt, startWithFixture, uuid, waitReceived, type Started } from "./helpers.ts";

const ctl = (subtype: string) => (r: Json) => r.type === "control_request" && (r.request as Json)?.subtype === subtype;
const hookAnswers = (s: Started) =>
  s
    .received()
    .filter((r) => r.type === "control_response" && String((r.response as Json)?.request_id ?? "").startsWith("fake-hook-"))
    .map((r) => ((r.response as Json).response as Json) ?? {});
const decision = (a: Json) => ((a.hookSpecificOutput as Json | undefined)?.permissionDecision as string | undefined) ?? (a.hookSpecificOutput ? "remind" : "allow");

async function steerDuringTool(s: Started): Promise<string> {
  await s.host.waitForType("tool_execution_start", 5000, 0);
  const sid = uuid();
  const r = await prompt(s.host, "also check the README", { streamingBehavior: "steer" }, sid);
  assert.equal(r.success, true, String(r.error));
  return sid;
}

describe("asap steers", () => {
  test("a mid-turn steer is accepted at once and delivered only at the next tool boundary", async () => {
    const s = startWithFixture("steer-read");
    try {
      const ready = await s.host.ready();
      assert.deepEqual((ready.data as Json).capabilities, { withdraw: true, deliverNow: true, stopTool: true, compact: true });
      assert.equal((ready.data as Json).cliVersion, "0.0.0-fake");
      const first = uuid();
      await prompt(s.host, "list the files", {}, first);
      await s.host.waitFor((e) => e.type === "submission_delivered" && e.submissionId === first, 5000, 0);
      const sid = await steerDuringTool(s);
      assert.equal(s.host.ofType("tool_execution_end").length, 0, "accepted before the tool boundary, not at it");
      const at = (pred: (e: Json) => boolean) => s.host.events.findIndex(pred);
      const delivered = await s.host.waitFor((e) => e.type === "submission_delivered" && e.submissionId === sid, 5000, 0);
      assert.ok(delivered);
      const toolEnd = at((e) => e.type === "tool_execution_end");
      const userRow = at((e) => e.type === "message_end" && (e.message as Json).id === sid);
      const deliveredAt = at((e) => e.type === "submission_delivered" && e.submissionId === sid);
      assert.ok(toolEnd !== -1 && toolEnd < userRow && userRow < deliveredAt, "read after the running tool returned");
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "complete");
      const reply = s.host
        .ofType("message_end")
        .map((e) => e.message as Json)
        .filter((m) => m.role === "assistant")
        .at(-1);
      assert.match(JSON.stringify(reply?.content), /Noted your message: also check the README/);
      assert.ok(at((e) => e.type === "agent_settled") > deliveredAt, "same turn");
      assert.equal(s.host.ofType("agent_start").length, 1, "one turn");
    } finally {
      await s.done();
    }
  });
});

describe("wait limit", () => {
  test("deliver_now answers waiting before the call is eligible, then moves it to the background", async () => {
    const s = startWithFixture("wait-limit");
    try {
      await s.host.ready();
      await prompt(s.host, "start the server");
      await s.host.waitFor((e) => e.type === "tool_execution_update" && e.stoppable === true, 5000, 0);
      const sid = await steerDuringTool(s);
      const early = await s.host.request({ type: "deliver_now", submissionId: sid, reason: "wait_limit", minRunMs: 60_000 });
      assert.deepEqual(early.data, { result: "waiting" });
      assert.equal(s.received().some(ctl("background_tasks")), false, "nothing moved while not eligible");
      const r = await s.host.request({ type: "deliver_now", submissionId: sid, reason: "wait_limit", minRunMs: 0 });
      assert.deepEqual(r.data, { result: "unblocked", moved: ["tu_1"] });
      const bg = await waitReceived(s, ctl("background_tasks"));
      assert.equal((bg.request as Json).tool_use_id, "tu_1");
      await s.host.waitFor((e) => e.type === "submission_delivered" && e.submissionId === sid, 5000, 0);
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "complete", "the turn was not ended");
      const tasks = s.host.ofType("background_tasks")[0]?.tasks as Json[];
      assert.deepEqual(tasks.map((t) => [t.id, t.kind, t.command]), [["bt_1", "shell", "node server.js"]]);
      const after = await s.host.request({ type: "deliver_now", submissionId: sid, reason: "wait_limit", minRunMs: 0 });
      assert.deepEqual(after.data, { result: "stopped" }, "no turn running");
    } finally {
      await s.done();
    }
  });

  test("deliver_now: nothing_waiting for a steer already read; waiting while an approval is open", async () => {
    const s = startWithFixture("approval");
    try {
      await s.host.ready();
      const first = uuid();
      await prompt(s.host, "list files", {}, first);
      await s.host.waitForType("extension_ui_request", 5000, 0);
      const read = await s.host.request({ type: "deliver_now", submissionId: first, reason: "wait_limit", minRunMs: 0 });
      assert.deepEqual(read.data, { result: "nothing_waiting" });
      const sid = uuid();
      await prompt(s.host, "steer", { streamingBehavior: "steer" }, sid);
      const r = await s.host.request({ type: "deliver_now", submissionId: sid, reason: "wait_limit", minRunMs: 0 });
      assert.deepEqual(r.data, { result: "waiting" });
      s.host.send({ type: "extension_ui_response", id: (s.host.ofType("extension_ui_request")[0].id as string), value: "Deny" });
      await s.host.waitForType("agent_settled", 5000, 0);
    } finally {
      await s.done();
    }
  });
});

describe("long-command rule", () => {
  test("redirect: the hook is registered, the prompt rule appended, the call denied once and re-run in the background", async () => {
    const s = startWithFixture("long-command", { claude: { appendSystemPrompt: "Base instructions." }, sendBehavior: { longCommands: "redirect", optIn: true } });
    try {
      await s.host.ready();
      const init = s.received().find(ctl("initialize"))!;
      assert.deepEqual((init.request as Json).hooks, { PreToolUse: [{ matcher: "Bash", hookCallbackIds: ["helm-long-commands"] }] });
      const argv = s.fakeStart()?.argv as string[];
      const append = argv[argv.indexOf("--append-system-prompt") + 1];
      assert.equal(append, `Base instructions.\n\n${PROMPT_RULE}`);
      await prompt(s.host, "run the tests in the foreground");
      const settled = await s.host.waitForType("agent_settled", 8000, 0);
      assert.equal(settled.outcome, "complete");
      const lc = s.host.ofType("long_command");
      assert.deepEqual(lc.map((e) => [e.toolCallId, e.action]), [["tu_1", "denied"]]);
      assert.match(String(lc[0].reason), /npm test/);
      assert.deepEqual(hookAnswers(s).map(decision), ["deny", "allow"], "the background re-run is allowed");
      const counts = s.host.ofType("background_tasks").map((e) => (e.tasks as unknown[]).length);
      assert.deepEqual(counts, [1, 0]);
    } finally {
      await s.done();
    }
  });

  test("redirect rules: exact repeat escapes once, another root call clears it, subagents and background runs are exempt, a new turn clears it", async () => {
    const s = startWithFixture("long-command-rules", { sendBehavior: { longCommands: "redirect" } });
    try {
      await s.host.ready();
      await prompt(s.host, "go");
      await s.host.waitForType("agent_settled", 8000, 0);
      await prompt(s.host, "again");
      await s.host.waitFor((e) => e.type === "agent_settled", 8000, s.host.events.indexOf(s.host.ofType("agent_settled")[0]) + 1);
      // r1 npm test, r2 npm test (repeat), r3 cargo build, r4 git status, r5 cargo build,
      // s1 subagent npm run build, r6 background npm run dev, then next turn r7 cargo build.
      assert.deepEqual(hookAnswers(s).map(decision), ["deny", "allow", "deny", "allow", "deny", "allow", "allow", "deny"]);
      assert.deepEqual(
        s.host.ofType("long_command").map((e) => [e.toolCallId, e.action]),
        [["r1", "denied"], ["r2", "escaped"], ["r3", "denied"], ["r5", "denied"], ["r7", "denied"]],
      );
    } finally {
      await s.done();
    }
  });

  test("warn: a reminder is added and nothing is denied", async () => {
    const s = startWithFixture("long-command-rules", { sendBehavior: { longCommands: "warn" } });
    try {
      await s.host.ready();
      await prompt(s.host, "go");
      await s.host.waitForType("agent_settled", 8000, 0);
      const answers = hookAnswers(s);
      assert.equal(answers.some((a) => decision(a) === "deny"), false);
      const reminded = answers.filter((a) => decision(a) === "remind");
      assert.equal(reminded.length, 4, "r1, r2, r3, r5");
      assert.match(String((reminded[0].hookSpecificOutput as Json).additionalContext), /long-running command/);
      assert.ok(s.host.ofType("long_command").every((e) => e.action === "reminded"));
    } finally {
      await s.done();
    }
  });

  test("off: no hook is registered and no prompt rule is appended", async () => {
    const s = startWithFixture("long-command", { sendBehavior: { longCommands: "off" } });
    try {
      await s.host.ready();
      const init = s.received().find(ctl("initialize"))!;
      assert.equal((init.request as Json).hooks, undefined);
      assert.equal((s.fakeStart()?.argv as string[]).includes("--append-system-prompt"), false);
      await prompt(s.host, "run the tests");
      await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(s.host.ofType("long_command").length, 0);
    } finally {
      await s.done();
    }
  });
});

describe("withdraw, stop_tool, compact and abort", () => {
  test("withdraw takes back an unread steer; a read one answers withdrawn:false", async () => {
    const s = startWithFixture("withdraw");
    try {
      await s.host.ready();
      await prompt(s.host, "list files");
      await s.host.waitForType("tool_execution_start", 5000, 0);
      const keep = uuid();
      const drop = uuid();
      await prompt(s.host, "keep", { streamingBehavior: "steer" }, keep);
      await prompt(s.host, "drop", { streamingBehavior: "steer" }, drop);
      const w = await s.host.request({ type: "withdraw", submissionId: drop });
      assert.deepEqual(w.data, { withdrawn: true });
      await s.host.waitForType("agent_settled", 5000, 0);
      const delivered = s.host.ofType("submission_delivered").map((e) => e.submissionId);
      assert.ok(delivered.includes(keep));
      assert.equal(delivered.includes(drop), false);
      const late = await s.host.request({ type: "withdraw", submissionId: keep });
      assert.deepEqual(late.data, { withdrawn: false });
    } finally {
      await s.done();
    }
  });

  test("stop_tool stops a stoppable foreground Bash call; a stale line is refused", async () => {
    const s = startWithFixture("stop-tool");
    try {
      await s.host.ready();
      await prompt(s.host, "wait");
      const start = await s.host.waitForType("tool_execution_start", 5000, 0);
      assert.equal(start.stoppable, false);
      await s.host.waitFor((e) => e.type === "tool_execution_update" && e.stoppable === true, 5000, 0);
      const stale = await s.host.request({ type: "stop_tool", toolCallId: "tu_1", sentAtMs: Date.now() - 6000 });
      assert.equal(stale.success, false);
      const unknown = await s.host.request({ type: "stop_tool", toolCallId: "nope", sentAtMs: Date.now() });
      assert.deepEqual(unknown.data, { stopped: false });
      const r = await s.host.request({ type: "stop_tool", toolCallId: "tu_1", sentAtMs: Date.now() });
      assert.deepEqual(r.data, { stopped: true });
      const st = await waitReceived(s, ctl("stop_task"));
      assert.equal((st.request as Json).task_id, "bt_1");
      const end = await s.host.waitForType("tool_execution_end", 5000, 0);
      assert.equal(end.isError, true);
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "complete", "the turn carries on");
    } finally {
      await s.done();
    }
  });

  test("abort backgrounds the foreground shell, interrupts with cancel_queued and reports the dropped steers", async () => {
    const s = startWithFixture("abort-cancel");
    try {
      await s.host.ready();
      await prompt(s.host, "wait a minute");
      await s.host.waitFor((e) => e.type === "tool_execution_update" && e.stoppable === true, 5000, 0);
      const steer = uuid();
      await prompt(s.host, "never mind", { streamingBehavior: "steer" }, steer);
      const r = await s.host.request({ type: "abort" });
      assert.equal(r.success, true);
      assert.deepEqual((r.data as Json).cancelledSubmissionIds, [steer]);
      const received = s.received();
      const bgAt = received.findIndex(ctl("background_tasks"));
      const intAt = received.findIndex(ctl("interrupt"));
      assert.ok(bgAt !== -1 && bgAt < intAt, "the shell is moved before the interrupt");
      assert.equal((received[intAt].request as Json).cancel_queued, true);
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.outcome, "stopped");
      assert.equal(s.host.ofType("submission_delivered").some((e) => e.submissionId === steer), false);
    } finally {
      await s.done();
    }
  });

  test("compact while idle sends /compact and reports start and end; refused while a turn runs", async () => {
    const s = startWithFixture("compact");
    try {
      await s.host.ready();
      const r = await s.host.request({ type: "compact" });
      assert.equal(r.success, true, String(r.error));
      assert.ok(s.host.ofType("compaction_start").length === 1);
      const end = await s.host.waitForType("compaction_end", 5000, 0);
      assert.deepEqual([end.trigger, end.ok], ["manual", true]);
      const sent = await waitReceived(s, (x) => x.type === "user");
      assert.match(JSON.stringify(sent), /\/compact/);
    } finally {
      await s.done();
    }
    const busy = startWithFixture("stop-tool");
    try {
      await busy.host.ready();
      await prompt(busy.host, "wait");
      await busy.host.waitForType("tool_execution_start", 5000, 0);
      const refused = await busy.host.request({ type: "compact" });
      assert.equal(refused.success, false);
      assert.match(String(refused.error), /while a turn is running/);
    } finally {
      await busy.done();
    }
  });

  test("send now: priority now during a turn is followed by a plain interrupt and the turn ends superseded", async () => {
    const s = startWithFixture("abort");
    try {
      await s.host.ready();
      await prompt(s.host, "work");
      await s.host.waitForType("message_update", 5000, 0);
      await prompt(s.host, "do this instead", { priority: "now" });
      const int = await waitReceived(s, ctl("interrupt"));
      assert.equal((int.request as Json).cancel_queued, undefined);
      const settled = await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(settled.superseded, true);
    } finally {
      await s.done();
    }
  });
});

describe("native tools", () => {
  test("the cron, wakeup and worktree tools are disallowed", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const argv = s.fakeStart()?.argv as string[];
      const i = argv.indexOf("--disallowedTools");
      assert.ok(i !== -1);
      assert.deepEqual(argv[i + 1].split(","), [...DISALLOWED_TOOLS]);
      for (const t of ["CronCreate", "ScheduleWakeup", "EnterWorktree", "ExitWorktree"]) assert.ok(DISALLOWED_TOOLS.includes(t as never), t);
    } finally {
      await s.done();
    }
  });
});

describe("resume", () => {
  test("a launch with resumeSessionId passes --resume to claude", async () => {
    const s = startWithFixture("resume", { claude: { resumeSessionId: "fake-session" } });
    try {
      await s.host.ready();
      await prompt(s.host, "continue");
      await s.host.waitForType("agent_settled", 5000, 0);
      const text = JSON.stringify(s.host.ofType("message_end").map((e) => (e.message as Json).content));
      assert.match(text, /Continuing the resumed session/);
    } finally {
      await s.done();
    }
  });

  test("echo-loop: several turns in one worker", async () => {
    const s = startWithFixture("echo-loop");
    try {
      await s.host.ready();
      for (let i = 1; i <= 3; i++) {
        await prompt(s.host, `turn ${i}`);
        await s.host.waitFor(() => s.host.ofType("agent_settled").length === i, 5000, 0);
      }
      assert.match(JSON.stringify(s.host.ofType("message_end").map((e) => (e.message as Json).content)), /You said: turn 3/);
    } finally {
      await s.done();
    }
  });
});
