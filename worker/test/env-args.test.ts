import assert from "node:assert/strict";
import { test } from "node:test";
import { buildClaudeArgs, disallowedTools } from "../src/drivers/claude/args.ts";
import { cronTiming, NATIVE_SCHEDULE_TOOLS, scheduleRedirect } from "../src/drivers/claude/schedulecalls.ts";
import { assertLoopbackHttp, buildClaudeEnv, STRIPPED_UNLESS_API_KEY_MODE, takeLocalKey } from "../src/drivers/claude/env.ts";

const base = {
  PATH: "/bin",
  ANTHROPIC_API_KEY: "k",
  ANTHROPIC_AUTH_TOKEN: "t",
  ANTHROPIC_BASE_URL: "u",
  CLAUDE_CODE_OAUTH_TOKEN: "o",
  CLAUDECODE: "1",
  CLAUDE_CODE_ENTRYPOINT: "cli",
  KEEP_ME: "yes",
};

test("credential and nesting variables are stripped by default", () => {
  const env = buildClaudeEnv(base, {});
  for (const k of STRIPPED_UNLESS_API_KEY_MODE) assert.equal(env[k], undefined, k);
  assert.equal(env.CLAUDE_CODE_ENTRYPOINT, undefined);
  assert.equal(env.KEEP_ME, "yes");
  assert.equal(env.PATH, "/bin");
});

test("stripping is case-insensitive (Windows env names)", () => {
  const env = buildClaudeEnv({ anthropic_api_key: "k", ClaudeCode: "1" }, {});
  assert.deepEqual(Object.keys(env).filter((k) => /anthropic|claudecode/i.test(k)), []);
});

test("API-key mode keeps them", () => {
  const env = buildClaudeEnv(base, { apiKeyMode: true });
  for (const k of STRIPPED_UNLESS_API_KEY_MODE) assert.equal(env[k], base[k], k);
});

test("non-essential traffic and auto-update are off", () => {
  const env = buildClaudeEnv({}, { chatId: "c1" });
  assert.equal(env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC, "1");
  assert.equal(env.DISABLE_AUTOUPDATER, "1");
  assert.equal(env.HELM_CHAT_ID, "c1");
});

test("a local launch: loopback http only, every model slot local, the effort as a thinking budget, the key from the hand-over", () => {
  // Where the base URL may point.
  for (const ok of ["http://127.0.0.1:18731", "http://localhost:11434/", "http://[::1]:8080"]) assert.doesNotThrow(() => assertLoopbackHttp(ok), ok);
  assert.equal(assertLoopbackHttp("http://localhost:11434/"), "http://localhost:11434");
  for (const bad of ["https://127.0.0.1:1", "http://10.0.0.5:1", "http://127.0.0.1.evil.com:1", "http://127.0.0.1@evil.com:1", "http://user:pw@127.0.0.1:1", "ftp://127.0.0.1", "127.0.0.1:1", ""]) {
    assert.throws(() => assertLoopbackHttp(bad), undefined, bad);
  }

  // The key is taken out of the environment it came in, whatever the case of its name.
  const handed: Record<string, string | undefined> = { Helm_Local_Model_Key: "k-1", KEEP: "x" };
  assert.equal(takeLocalKey(handed), "k-1");
  assert.deepEqual(handed, { KEEP: "x" });
  assert.equal(takeLocalKey({ HELM_LOCAL_MODEL_KEY: "" }), undefined);

  // Efforts become thinking budgets; low is off; no effort uses the launch's own figure.
  const local = { baseUrl: "http://127.0.0.1:18731", model: "qwen:8b", thinkingTokens: 777 };
  const budget = (effort?: string) => buildClaudeEnv({}, { local, localKey: "k", effort }).MAX_THINKING_TOKENS;
  assert.deepEqual(["low", "medium", "high", "xhigh", "max"].map(budget), ["0", "4096", "8192", "16384", "32768"]);
  assert.equal(budget(undefined), "777");
  assert.equal(buildClaudeEnv({}, { local: { ...local, thinkingTokens: undefined }, localKey: "k" }).MAX_THINKING_TOKENS, "16384");

  // Optional limits are set only when known; the token is the key, or a placeholder, never the user's.
  const env = buildClaudeEnv({ ...base, ANTHROPIC_CUSTOM_HEADERS: "x-auth: secret", CLAUDE_CODE_USE_VERTEX: "1", no_proxy: "a" }, { apiKeyMode: true, local, localKey: "k", effort: "low" });
  assert.deepEqual([env.ANTHROPIC_AUTH_TOKEN, env.ANTHROPIC_BASE_URL, env.ANTHROPIC_MODEL, env.ANTHROPIC_DEFAULT_HAIKU_MODEL], ["k", "http://127.0.0.1:18731", "qwen:8b", "qwen:8b"]);
  assert.deepEqual(["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDECODE", "ANTHROPIC_CUSTOM_HEADERS", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_MAX_CONTEXT_TOKENS", "CLAUDE_CODE_MAX_OUTPUT_TOKENS"].filter((k) => k in env), [], "not even in API-key mode");
  assert.equal(env.no_proxy, "a,127.0.0.1,localhost,::1");
  assert.equal(env.CLAUDE_CODE_USE_GATEWAY, "1", "the base URL is treated as a gateway, so the thinking budget is what is sent");
  assert.equal(env.ENABLE_TOOL_SEARCH, "false", "no tool is deferred behind a search a local server cannot answer");
  assert.equal(buildClaudeEnv({}, { local, effort: "low" }).ANTHROPIC_AUTH_TOKEN, "local", "a keyless server still gets a token of its own");
  assert.throws(() => buildClaudeEnv({}, { local: { ...local, baseUrl: "http://example.com" } }));
  assert.throws(() => buildClaudeEnv({}, { local: { ...local, model: " " } }));
  // Without a local launch nothing of this applies.
  assert.equal("MAX_THINKING_TOKENS" in buildClaudeEnv({ HELM_LOCAL_MODEL_KEY: "k" }, {}), false);
  assert.equal("HELM_LOCAL_MODEL_KEY" in buildClaudeEnv({ HELM_LOCAL_MODEL_KEY: "k" }, { local, localKey: "k" }), false, "the hand-over name never reaches claude");

  // The flags: no effort flag, no WebSearch (WebFetch stays), and the domain pre-check off.
  const a = buildClaudeArgs({ model: "qwen:8b", effort: "high", local });
  assert.equal(a.includes("--effort"), false);
  assert.equal(flag(a, "--model"), "qwen:8b");
  assert.deepEqual(disallowedTools({ local }, {}).filter((t) => /^Web/.test(t)), ["WebSearch"]);
  assert.equal(disallowedTools({}, {}).includes("WebSearch"), false, "Claude chats keep it");
  assert.equal(JSON.parse(flag(a, "--settings") ?? "null").skipWebFetchPreflight, true);
  assert.equal("skipWebFetchPreflight" in JSON.parse(flag(buildClaudeArgs({}), "--settings") ?? "null"), false);
});

function flag(args: string[], name: string): string | undefined {
  const i = args.indexOf(name);
  return i === -1 ? undefined : args[i + 1];
}

test("stream-json both ways, partial messages, replayed user messages, stdio permission prompts", () => {
  const a = buildClaudeArgs({});
  assert.ok(a.includes("-p"));
  assert.equal(flag(a, "--input-format"), "stream-json");
  assert.equal(flag(a, "--output-format"), "stream-json");
  assert.ok(a.includes("--include-partial-messages"));
  assert.ok(a.includes("--replay-user-messages"));
  assert.equal(flag(a, "--permission-prompt-tool"), "stdio");
  assert.equal(flag(a, "--setting-sources"), "user,project,local");
});

test("effort defaults to xhigh; haiku gets none; an unknown effort falls back to the default", () => {
  assert.equal(flag(buildClaudeArgs({}), "--effort"), "xhigh");
  assert.equal(flag(buildClaudeArgs({ effort: "low", model: "opus" }), "--effort"), "low");
  assert.equal(flag(buildClaudeArgs({ effort: "bogus" }), "--effort"), "xhigh");
  assert.equal(buildClaudeArgs({ model: "haiku", effort: "low" }).includes("--effort"), false);
  assert.equal(flag(buildClaudeArgs({ model: "haiku" }), "--model"), "haiku");
});

test("skip permissions, resume and settings flags", () => {
  const a = buildClaudeArgs({ skipPermissions: true, resumeSessionId: "s-1", fastMode: true, settings: { other: 1 } });
  assert.equal(flag(a, "--permission-mode"), "bypassPermissions");
  assert.equal(flag(a, "--resume"), "s-1");
  assert.deepEqual(JSON.parse(flag(a, "--settings") ?? "null"), { other: 1, fastMode: true });
  assert.equal(buildClaudeArgs({}).includes("--permission-mode"), false);
});

test("Claude Code's worktree tools stay disallowed unless the launch allows them and no repository has worktrees off", () => {
  const list = (c: object, env: Record<string, string> = {}) => disallowedTools(c, env);
  assert.ok(list({}).includes("EnterWorktree") && list({}).includes("ExitWorktree"));
  assert.ok(list({ worktreeTools: false }).includes("EnterWorktree"));
  const on = list({ worktreeTools: true });
  assert.ok(!on.includes("EnterWorktree") && !on.includes("ExitWorktree"));
  assert.ok(on.includes("CronCreate"), "the other native tools stay disallowed");
  assert.ok(list({ worktreeTools: true }, { HELM_WORKTREE_OFF_LIST: "C:/p/worktrees-off.tsv" }).includes("EnterWorktree"));
  const flagValue = flag(buildClaudeArgs({ worktreeTools: true }), "--disallowedTools") ?? "";
  assert.ok(!flagValue.includes("EnterWorktree") && flagValue.includes("RemoteTrigger"));
});

test("Claude Code's schedule tools come off the disallowed list only on request, and every call is redirected", () => {
  assert.ok(NATIVE_SCHEDULE_TOOLS.every((t) => disallowedTools({}, {}).includes(t)));
  const on = disallowedTools({ scheduleTools: true }, {});
  assert.ok(NATIVE_SCHEDULE_TOOLS.every((t) => !on.includes(t)) && on.includes("EnterWorktree"), "only the schedule tools move");
  // The cron rules that map exactly, and one that does not.
  assert.deepEqual(cronTiming("*/15 * * * *"), { kind: "interval", seconds: 900 });
  assert.deepEqual(cronTiming("0 */6 * * *"), { kind: "interval", seconds: 21600 });
  assert.deepEqual(cronTiming("30 9 * * 1-5"), { kind: "weekly", hour: 9, minute: 30, days: [0, 1, 2, 3, 4] });
  assert.deepEqual(cronTiming("0 8 * * 0"), { kind: "weekly", hour: 8, minute: 0, days: [6] });
  assert.equal(cronTiming("*/7 * * * *"), null);
  assert.equal(cronTiming("0 9 1 * *"), null);
  // The refusal names Helm's tool, clamps the wake-up delay and never repeats the prompt.
  const wake = scheduleRedirect("ScheduleWakeup", { delaySeconds: 5, prompt: "SECRET PROMPT" });
  assert.ok(wake.includes("app_create_schedule") && wake.includes('"inSeconds":60') && !wake.includes("SECRET"));
  assert.ok(scheduleRedirect("ScheduleWakeup", { delaySeconds: 99999 }).includes('"inSeconds":3600'));
  assert.ok(scheduleRedirect("CronCreate", { cron: "*/5 * * * *", prompt: "SECRET" }).includes('"seconds":300'));
  assert.ok(scheduleRedirect("CronList", {}).includes("app_list_schedules"));
  assert.ok(scheduleRedirect("CronDelete", {}).includes("app_cancel_schedule"));
  assert.ok(scheduleRedirect("RemoteTrigger", {}).includes("app_create_schedule"));
});

test("a fork's first launch resumes as a new session; --fork-session needs a session to resume", () => {
  const a = buildClaudeArgs({ resumeSessionId: "s-1", forkSession: true });
  assert.equal(flag(a, "--resume"), "s-1");
  assert.ok(a.includes("--fork-session"));
  assert.equal(buildClaudeArgs({ forkSession: true }).includes("--fork-session"), false);
  assert.equal(buildClaudeArgs({ resumeSessionId: "s-1" }).includes("--fork-session"), false);
});

test("fastMode is always explicit, strictly boolean, and only the launch's own field sets it", () => {
  assert.deepEqual(JSON.parse(flag(buildClaudeArgs({}), "--settings") ?? "null"), { fastMode: false });
  // A customization that sets fastMode is dropped; the launch field wins.
  const custom = buildClaudeArgs({ fastMode: false, settings: { fastMode: true } });
  assert.deepEqual(JSON.parse(flag(custom, "--settings") ?? "null"), { fastMode: false });
  // Anything but the literal true is off.
  const loose = buildClaudeArgs({ fastMode: "true" as unknown as boolean });
  assert.deepEqual(JSON.parse(flag(loose, "--settings") ?? "null"), { fastMode: false });
});
