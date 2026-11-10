// The voice benchmark's own parts: its prompts, its fake hub line and its table maths. This starts
// no app and no model: the fake hub is a loopback server and the other two are pure. The runner
// itself (checks/voice-bench/run.ts) only ever runs by hand.
//
// Run: `node --test e2e/voice-bench.e2e.ts` in checks/ (or `npm run e2e -w checks`).

import assert from "node:assert/strict";
import http from "node:http";
import { describe, test } from "node:test";
import { startFakeHub, type Frame } from "../voice-bench/fake-hub.ts";
import { choose, PROMPTS } from "../voice-bench/prompts.ts";
import { noneRemoved, percentile, repliesDocument, resultsTable, summarise, type Candidate, type Sample } from "../voice-bench/report.ts";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("the voice benchmark", () => {
  test("its prompts: twenty, synthetic, with every kind the plan lists, news last", () => {
    assert.equal(PROMPTS.length, 20);
    assert.equal(new Set(PROMPTS.map((p) => p.id)).size, 20);
    const kinds = (k: string) => PROMPTS.filter((p) => p.kind === k);
    assert.equal(kinds("news").length, 5);
    assert.ok(kinds("news").every((p) => p.expect === "tell" || p.expect === "quiet"));
    assert.ok(kinds("news").some((p) => p.expect === "quiet") && kinds("news").some((p) => p.expect === "tell"));
    // News needs the voice chat to exist, so every news prompt comes after every turn.
    const firstNews = PROMPTS.findIndex((p) => p.kind === "news");
    assert.ok(PROMPTS.slice(firstNews).every((p) => p.kind === "news"));
    // What the plan asks for: a short question, a long result, a list, a link and an id, a status.
    const text = PROMPTS.map((p) => p.text).join("\n");
    assert.match(text, /https:\/\/example\.com\//);
    assert.match(text, /#1234/);
    assert.ok(PROMPTS.some((p) => p.tests.includes("status")));
    assert.ok(PROMPTS.some((p) => p.tests.includes("list")));
    assert.ok(PROMPTS.some((p) => p.text.split(/\s+/).length > 100));
    // Nothing private: no drive paths, no addresses but the example domain, no names.
    assert.doesNotMatch(text, /[A-Z]:\\|\/Users\/|@|https?:\/\/(?!example\.com)/);
    // The cheap trial: seven prompts, both news outcomes among them, still news last.
    const quick = choose(true);
    assert.equal(quick.length, 7);
    assert.ok(quick.some((p) => p.expect === "quiet") && quick.some((p) => p.expect === "tell"));
    assert.equal(choose(false).length, 20);
  });

  test("its fake hub speaks the relay line: handshake, auth, frames both ways, big and split frames", async () => {
    // Without a token anything may connect; with one, only the right header.
    const open = await startFakeHub();
    const got: Frame[] = [];
    const ws = new WebSocket(open.url);
    ws.onmessage = (e) => got.push(JSON.parse(String(e.data)));
    await new Promise<void>((resolve, reject) => {
      ws.onopen = () => resolve();
      ws.onerror = () => reject(new Error("the fake hub did not complete the handshake"));
    });
    await open.waitConnected(5_000);
    // Client frames are masked and may use any of the three length forms.
    const pads = [0, 200, 70_000];
    ws.send(JSON.stringify({ type: "sharing", chats: false }));
    for (const n of pads) ws.send(JSON.stringify({ type: "pad", n, pad: "é".repeat(n) }));
    const last = await open.waitFor((f) => f.type === "pad" && f.n === 70_000, 5_000);
    assert.equal(last.pad.length, 70_000);
    assert.deepEqual(open.frames.map((f) => f.type), ["sharing", "pad", "pad", "pad"]);
    assert.ok(open.arrivedAt(last) >= open.arrivedAt(open.frames[0]));
    // Frames down the line, including one past 64 KiB.
    open.send({ type: "voice_presence", live: true, mode: "conversation" });
    open.send({ type: "voice", id: "t1", text: "x".repeat(70_000) });
    for (let i = 0; i < 100 && got.length < 2; i++) await sleep(50);
    assert.deepEqual(got.map((f) => f.type), ["voice_presence", "voice"]);
    assert.equal(got[1].text.length, 70_000);
    // waitFor looks from an index on, and gives up when nothing matches.
    assert.equal(await open.waitFor((f) => f.type === "pad", 1_000, 2).then((f) => f.n), 200);
    await assert.rejects(open.waitFor((f) => f.type === "never", 150), /timed out/);
    ws.close();
    for (let i = 0; i < 100 && open.connected(); i++) await sleep(50);
    assert.equal(open.connected(), false);
    assert.throws(() => open.send({ type: "x" }), /not connected/);
    await open.close();

    const guarded = await startFakeHub({ token: "t-123" });
    const upgrade = (headers: Record<string, string>) =>
      new Promise<number>((resolve, reject) => {
        const req = http.request(guarded.url.replace("ws://", "http://"), {
          headers: { Connection: "Upgrade", Upgrade: "websocket", "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==", "Sec-WebSocket-Version": "13", ...headers },
        });
        req.on("response", (r) => {
          resolve(r.statusCode ?? 0);
          r.resume();
        });
        req.on("upgrade", (_r, socket) => {
          socket.destroy();
          resolve(101);
        });
        req.on("error", reject);
        req.end();
      });
    assert.equal(await upgrade({}), 401);
    assert.equal(await upgrade({ Authorization: "Bearer wrong" }), 401);
    assert.equal(await upgrade({ Authorization: "Bearer t-123" }), 101);
    await guarded.close();
  });

  test("its table: medians, the slowest tenth, the cleanup, the cap and how news was told", () => {
    const [turn1, turn2, turn3] = PROMPTS.filter((p) => p.kind === "turn");
    const [tell, quiet] = [PROMPTS.find((p) => p.expect === "tell")!, PROMPTS.find((p) => p.expect === "quiet")!];
    const words = (n: number) => Array.from({ length: n }, (_, i) => `w${i}`).join(" ");
    const sample = (prompt: Sample["prompt"], over: Partial<Sample>): Sample => ({
      prompt,
      outcome: "answered",
      answerMs: 2_000,
      statusFrames: 0,
      sentences: [words(10)],
      truncated: false,
      raw: "",
      rawWords: 0,
      ...over,
    });
    const samples: Sample[] = [
      sample(turn1, { answerMs: 1_000, removed: { ...noneRemoved(), urls: 2, ids: 1 } }),
      sample(turn2, { answerMs: 3_000, sentences: [words(80)], removed: noneRemoved() }),
      sample(turn3, { answerMs: 5_000, sentences: [words(140), "There's more.", "Want the rest?"], truncated: true, removed: { ...noneRemoved(), code: 1 } }),
      sample(PROMPTS.find((p) => p.id === "p15")!, { outcome: "timeout", answerMs: undefined, sentences: [] }),
      sample(tell, { answerMs: 4_000 }),
      sample(quiet, { outcome: "quiet", answerMs: undefined, sentences: [], raw: "[quiet]" }),
    ];
    const s = summarise(samples);
    assert.deepEqual([s.prompts, s.turns, s.answered, s.timeouts, s.waiting], [6, 4, 4, 1, 0]);
    // Answer times: 1, 3, 5 and 4 seconds.
    assert.equal(s.medianAnswerS, 3);
    assert.equal(s.p90AnswerS, 5);
    assert.deepEqual([s.overAim, s.cut], [2, 1]);
    // The cleanup: two of three counted turns had something taken out.
    assert.deepEqual([s.cleaned, s.cleanedKnown], [2, 3]);
    assert.deepEqual([s.removed.urls, s.removed.ids, s.removed.code], [2, 1, 1]);
    assert.deepEqual([s.news, s.quiet, s.newsRight], [2, 1, 2]);
    assert.equal(percentile([], 50), undefined);
    assert.equal(percentile([5, 1, 3], 50), 3);

    const c: Candidate = { key: "k", label: "Test model", model: "m-1", effort: "low", samples };
    const table = resultsTable([c]).split("\n");
    assert.equal(table.length, 3);
    assert.equal(table[0].split("|").length, table[2].split("|").length, "every row has every column");
    assert.match(table[2], /Test model \(m-1, low\)/);
    assert.match(table[2], /4 of 6 \(1 timed out\)/);
    assert.match(table[2], /2 of 3 turns/);
    assert.match(table[2], /code 1, urls 2, ids 1/);
    assert.match(table[2], /2 of 2 \| 1 of 2/);
    // The replies file holds each prompt and answer, and a score to fill in.
    const doc = repliesDocument(c);
    assert.match(doc, new RegExp(`## ${turn1.id}`));
    assert.match(doc, /Cleanup took out: urls 2, ids 1\./);
    assert.match(doc, /The chat stayed quiet\. Wanted: quiet\./);
    assert.equal((doc.match(/^Score: $/gm) ?? []).length, 6);
  });
});
