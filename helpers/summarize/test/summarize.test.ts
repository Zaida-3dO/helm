import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { buildPrompt, cleanSummary, makeSummary, MAX_SUMMARY_CHARS } from "../src/summarize.ts";

/** A stand-in claude: answers each run with the next line of a script file. */
function fakeClaude(answers: string[]): { command: string; commandArgs: string[] } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-summarize-"));
  const state = path.join(dir, "n.txt");
  fs.writeFileSync(state, "0");
  const script = path.join(dir, "fake.mjs");
  fs.writeFileSync(
    script,
    `import fs from "node:fs";
const n = Number(fs.readFileSync(${JSON.stringify(state)}, "utf8"));
fs.writeFileSync(${JSON.stringify(state)}, String(n + 1));
const answers = ${JSON.stringify(answers)};
process.stdin.resume();
process.stdin.on("end", () => {
  const a = answers[Math.min(n, answers.length - 1)];
  process.stdout.write(JSON.stringify({ type: "system", subtype: "init" }) + "\\n");
  process.stdout.write(JSON.stringify({ type: "result", subtype: "success", is_error: false, result: a }) + "\\n");
  setTimeout(() => process.exit(0), 10);
});
`,
  );
  return { command: process.execPath, commandArgs: [script] };
}

test("cleanSummary keeps one plain line and cuts a long answer at a word", () => {
  assert.equal(cleanSummary(" **Fixing** the `hover` card.\nNow it waits for review.  "), "Fixing the hover card. Now it waits for review.");
  assert.equal(cleanSummary("“About the plan, step two.”"), "About the plan, step two.");
  assert.equal(cleanSummary("   "), null);
  assert.equal(cleanSummary(""), null);
  const long = "alpha beta gamma ".repeat(40);
  const got = cleanSummary(long)!;
  assert.ok(got.length <= MAX_SUMMARY_CHARS, `${got.length}`);
  assert.ok(!got.endsWith(" "), "no dangling space at the cut");
  assert.ok(!/[,;:.]$/.test(got), "no dangling mark at the cut");
  // Tabs and runs of whitespace fold to one space; ordinary text stays.
  assert.equal(cleanSummary("one\t two \n three"), "one two three");
});

test("the prompt names the chat, carries the transcript, and says the ask twice", () => {
  const p = buildPrompt(
    { title: "Hover cards", messages: [{ role: "user", text: "make the card show a summary" }, { role: "assistant", text: "working on it" }] },
    false,
  );
  assert.match(p, /<title>\nHover cards\n<\/title>/);
  assert.match(p, /user: make the card show a summary/);
  assert.match(p, /assistant: working on it/);
  assert.match(p, /exactly two short sentences/);
  assert.match(p, /Now reply with only the two sentences/);
  const strict = buildPrompt({ messages: [{ role: "user", text: "hi" }] }, true);
  assert.match(strict, /Reply with only two short sentences/);
  assert.ok(!strict.includes("<title>"));
});

test("makeSummary cleans the answer; an empty answer is no summary", async () => {
  const ok = fakeClaude(["  **About the plan.** It waits on review.  "]);
  const r = await makeSummary({ title: "Plan", messages: [{ role: "user", text: "plan the thing" }], command: ok.command, commandArgs: ok.commandArgs, timeoutMs: 5_000 });
  assert.deepEqual(r, { summary: "About the plan. It waits on review." });

  const junk = fakeClaude(["", "   "]);
  const bad = await makeSummary({ messages: [{ role: "user", text: "x" }], command: junk.command, commandArgs: junk.commandArgs, timeoutMs: 5_000 });
  assert.ok("error" in bad && typeof bad.error === "string");

  const none = await makeSummary({ messages: [] });
  assert.ok("error" in none);
});
