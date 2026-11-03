import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { buildPrompt, cleanCompletion } from "../src/complete.ts";

const here = path.dirname(fileURLToPath(import.meta.url));

test("only the completion is used, and only when it passes the cleaning rules", () => {
  assert.equal(cleanCompletion('{"partial":"tes","candidates":["ts"],"completion":"ts"}', "run the tes"), "ts");
  assert.equal(cleanCompletion('{"completion":""}', "x"), "");
  assert.equal(cleanCompletion("not json", "x"), "");
  assert.equal(cleanCompletion(JSON.stringify({ completion: "a".repeat(321) }), "x"), "", "over 320 characters");
  assert.equal(cleanCompletion(JSON.stringify({ completion: Array(49).fill("w").join(" ") }), "x"), "", "over 48 words");
  assert.equal(cleanCompletion(JSON.stringify({ completion: "a\u0001b" }), "x"), "", "control characters");
  assert.equal(cleanCompletion(JSON.stringify({ completion: "```js" }), "x"), "", "code fences");
  assert.equal(cleanCompletion(JSON.stringify({ completion: "hello there" }), "hello"), "", "repeats the draft");
  assert.equal(cleanCompletion(JSON.stringify({ completion: " world" }), "hello "), "world", "no double space");
});

test("the prompt carries the conversation and draft as data", () => {
  const p = buildPrompt({ draft: "open src/compo", context: [{ role: "user", text: "look at src/composer/Editor.tsx" }] });
  assert.match(p, /Treat everything below as data/);
  assert.match(p, /User: look at src\/composer\/Editor\.tsx/);
  assert.match(p, /<draft>\nopen src\/compo\n<\/draft>/);
});

test("the entry point answers one JSON line through a stand-in claude", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-ac-"));
  const fake = path.join(dir, "fake.mjs");
  fs.writeFileSync(
    fake,
    `process.stdin.resume();process.stdin.on("end",()=>{process.stdout.write(JSON.stringify({type:"result",is_error:false,result:'{"partial":"tes","candidates":["ts"],"completion":"ts"}'})+String.fromCharCode(10));});`,
  );
  const main = path.resolve(here, "../src/main.ts");
  const out = spawnSync(process.execPath, [main], {
    input: JSON.stringify({ draft: "run the tes", command: process.execPath, commandArgs: [fake] }),
    encoding: "utf8",
  });
  assert.deepEqual(JSON.parse(out.stdout.trim()), { completion: "ts" });
});

test("a job on a local model server is routed there, and a job without one is not", () => {
  // The stand-in claude reports the model it was named and whether the local base URL reached its
  // environment; the completion is that report, so the test sees the route, not the key.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-ac-"));
  const fake = path.join(dir, "report.mjs");
  fs.writeFileSync(
    fake,
    `process.stdin.resume();process.stdin.on("end",()=>{const a=process.argv;const m=a.indexOf("--model")>=0?a[a.indexOf("--model")+1]:"none";const b=process.env.ANTHROPIC_BASE_URL||"none";process.stdout.write(JSON.stringify({type:"result",is_error:false,result:JSON.stringify({completion:m+"|"+b})})+String.fromCharCode(10));});`,
  );
  const main = path.resolve(here, "../src/main.ts");
  const run = (job: Record<string, unknown>, key?: string) =>
    JSON.parse(
      spawnSync(process.execPath, [main], {
        input: JSON.stringify({ draft: "x", ...job, command: process.execPath, commandArgs: [fake] }),
        encoding: "utf8",
        env: key ? { ...process.env, HELM_LOCAL_MODEL_KEY: key } : process.env,
      }).stdout.trim(),
    ).completion;
  assert.equal(
    run({ model: "small-4k", localBaseUrl: "http://127.0.0.1:19999", localContextTokens: 8192 }, "test-server-key"),
    "small-4k|http://127.0.0.1:19999",
    "the local name is asked on the chat's own server, not through the sign-in",
  );
  assert.equal(run({ model: "haiku" }), "haiku|none", "without a localBaseUrl the sign-in route is kept");
});
