// The visual-check fixture (fakes/claude/fixtures/w3-visual.json): each tag gets the reply
// the w3-visual check asserts on, and an untagged prompt completes at once.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

type Json = Record<string, unknown>;
const here = path.dirname(fileURLToPath(import.meta.url));
const FAKE_CLAUDE = path.resolve(here, "../claude/fake-claude.ts");
const FIXTURE = path.resolve(here, "../claude/fixtures/w3-visual.json");

/** Send one prompt and collect the assistant text of its turn. */
async function turn(text: string): Promise<{ text: string; tools: string[] }> {
  const child = spawn(process.execPath, [FAKE_CLAUDE, FIXTURE, "--replay-user-messages"], { stdio: ["pipe", "pipe", "pipe"] });
  const lines: Json[] = [];
  let buf = "";
  child.stdout.setEncoding("utf8");
  const done = new Promise<void>((resolve, reject) => {
    const t = setTimeout(() => reject(new Error("no result in time")), 10_000);
    child.stdout.on("data", (d: string) => {
      buf += d;
      let i: number;
      while ((i = buf.indexOf("\n")) !== -1) {
        const l = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        if (!l) continue;
        const rec = JSON.parse(l) as Json;
        lines.push(rec);
        if (rec.type === "result") {
          clearTimeout(t);
          resolve();
        }
      }
    });
  });
  child.stdin.write(JSON.stringify({ type: "user", message: { role: "user", content: [{ type: "text", text }] }, parent_tool_use_id: null, session_id: "", uuid: randomUUID() }) + "\n");
  await done;
  child.stdin.end();
  child.kill();
  const blocks = lines
    .filter((l) => l.type === "assistant")
    .flatMap((l) => ((l.message as Json).content as Json[]) ?? []);
  return {
    text: blocks.filter((b) => b.type === "text").map((b) => String(b.text)).join("\n"),
    tools: blocks.filter((b) => b.type === "tool_use").map((b) => String(b.name)),
  };
}

test("[LINKS] carries a wikilink, a file link, a refused link and a web link", async () => {
  const r = await turn("[LINKS] show me");
  assert.match(r.text, /\[\[Fixture Note\]\]/);
  assert.match(r.text, /\(src\/main\.ts#L3\)/);
  assert.match(r.text, /\(C:javascript:alert\(1\)\)/);
  assert.match(r.text, /\(https:\/\/example\.com\/\)/);
});

test("[OPTIONS] lists numbered options; [QUESTION] ends in a question mark", async () => {
  const o = await turn("[OPTIONS] how?");
  assert.equal((o.text.match(/^\d\. /gm) ?? []).length, 3);
  const q = await turn("[QUESTION] next?");
  assert.ok(q.text.trimEnd().endsWith("?"));
});

test("[BUSY] runs a tool; an untagged prompt completes at once", async () => {
  const b = await turn("[BUSY] go");
  assert.deepEqual(b.tools, ["Bash"]);
  const d = await turn("hello");
  assert.match(d.text, /^Done\./);
});
