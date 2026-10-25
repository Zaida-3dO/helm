import assert from "node:assert/strict";
import { test } from "node:test";
import { buildClaudeArgs, disallowedTools } from "../src/drivers/claude/args.ts";
import { buildClaudeEnv, STRIPPED_UNLESS_API_KEY_MODE } from "../src/drivers/claude/env.ts";

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
