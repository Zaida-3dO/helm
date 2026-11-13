// A local chat refuses a sub-agent that asks for a Claude model (subagentmodel.ts): the call, the
// agent definitions, and the hook that rides only on a local chat.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { isClaudeModelRef, SubagentModelGuard, subagentModelDenyReason } from "../src/drivers/claude/subagentmodel.ts";
import type { Json } from "../src/protocol/types.ts";
import { startWithFixture } from "./helpers.ts";

test("a Claude model name, and what is not one", () => {
  for (const m of ["opus", "Sonnet", "HAIKU", "claude-opus-4-5", "claude-haiku-4-5-20251001", " opus "]) assert.ok(isClaudeModelRef(m), m);
  for (const m of ["inherit", "", "qwen:8b", "local/strata/x", "myopus", "claude", "gpt-4"]) assert.ok(!isClaudeModelRef(m), m);
});

test("the refusal names the agent, the model as asked, and app_create_chat as the way out", () => {
  const r = subagentModelDenyReason("coder", "sonnet");
  assert.match(r, /The sub-agent 'coder' asks for sonnet, but this chat runs on a local model server/);
  assert.match(r, /app_create_chat \(model "sonnet"\)/);
  assert.match(r, /report back to this chat with app_send_message/);
  assert.match(r, /no model of its own/);
});

test("the asked model comes from the call or from the agent's definition, whichever speaks first", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-sub-m-"));
  const cwd = path.join(dir, "proj");
  const home = path.join(dir, "home");
  for (const d of [path.join(cwd, ".claude", "agents"), path.join(home, ".claude", "agents")]) fs.mkdirSync(d, { recursive: true });
  const agentsFile = path.join(dir, "agents.json");
  fs.writeFileSync(agentsFile, JSON.stringify({ coder: { model: "sonnet" }, inheritOne: { model: "inherit" }, noModel: {}, both: { model: "sonnet" } }));
  fs.writeFileSync(path.join(cwd, ".claude", "agents", "helper.md"), "---\nname: helper\ndescription: does things\nmodel: haiku\n---\nBody that is not read.\n");
  fs.writeFileSync(path.join(cwd, ".claude", "agents", "renamed.md"), "---\nname: other\nmodel: opus\n---\n");
  fs.writeFileSync(path.join(cwd, ".claude", "agents", "both.md"), "---\nname: both\nmodel: opus\n---\n");
  fs.writeFileSync(path.join(home, ".claude", "agents", "homey.md"), "---\nmodel: claude-opus-4-5\n---\n");
  fs.writeFileSync(path.join(home, ".claude", "agents", "broken.md"), "no frontmatter at all");
  const g = new SubagentModelGuard({ agentsFile, cwd, home });

  const rows: Array<[string, Json, string | null, string?]> = [
    ["a model on the call, no type", { model: "opus" }, "default"],
    ["a model on the call wins over the definition", { subagent_type: "coder", model: "opus" }, "coder", "opus"],
    ["the launch's agents file", { subagent_type: "coder" }, "coder", "sonnet"],
    ["a project .md agent", { subagent_type: "helper" }, "helper", "haiku"],
    ["a .md whose name differs from its file", { subagent_type: "other" }, "other", "opus"],
    ["a home .md named by its file, with a full id", { subagent_type: "homey" }, "homey", "claude-opus-4-5"],
    ["the launch's file beats a project file of the same name", { subagent_type: "both" }, "both", "sonnet"],
    ["inherit on the call", { model: "inherit" }, null],
    ["inherit in the definition", { subagent_type: "inheritOne" }, null],
    ["an unknown agent", { subagent_type: "nobody" }, null],
    ["no model anywhere", {}, null],
    ["a local model name is fine", { model: "qwen:8b" }, null],
    ["a definition without a model is fine", { subagent_type: "noModel" }, null],
    ["a markdown file without frontmatter changes nothing", { subagent_type: "broken" }, null],
  ];
  for (const [label, input, name, model] of rows) {
    const r = g.decide(input as Json & Record<string, unknown>);
    if (name === null) {
      assert.equal(r, null, label);
      continue;
    }
    assert.ok(r, `${label}: refused`);
    assert.ok((r as string).includes(`'${name}'`), `${label}: names '${name}'`);
    if (model) assert.ok((r as string).includes(`asks for ${model},`), `${label}: keeps '${model}' as asked`);
  }
  // Definitions are read once and cached: removing the file changes nothing afterwards.
  fs.rmSync(agentsFile);
  assert.match(String(g.decide({ subagent_type: "coder" })), /asks for sonnet,/);
  // A missing or odd agents file is quiet, and so is a guard with no sources at all.
  const bare = new SubagentModelGuard({ agentsFile: path.join(dir, "gone.json") });
  assert.equal(bare.decide({ subagent_type: "x" }), null);
  fs.rmSync(dir, { recursive: true, force: true });
});

test("the hook rides on a local chat only", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-sub-hook-"));
  const run = async (claude: Json) => {
    const s = startWithFixture("tool-call", { claude, launch: { hostTools: [], chatToolsDirectory: dir } });
    try {
      await s.host.ready();
      const init = s.received().find((r) => r.type === "control_request" && (r.request as Json).subtype === "initialize")?.request as Json;
      return JSON.stringify((init.hooks as Json | undefined)?.PreToolUse ?? []);
    } finally {
      await s.done();
    }
  };
  try {
    const local = await run({ local: { baseUrl: "http://127.0.0.1:9", model: "m" } });
    assert.ok(local.includes("helm-subagent-model") && local.includes("Agent|Task"), "a local chat registers it on Agent and Task");
    const claude = await run({});
    assert.ok(!claude.includes("helm-subagent-model"), "a Claude chat does not");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
