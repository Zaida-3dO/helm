// The voice chat benchmark. Run by hand: `HELM_LIVE_CLAUDE=1 npm run voice-bench -w checks`.
//
// For each candidate model the runner starts the debug app on a scratch profile with the real
// `claude` (under your own sign-in, so it SPENDS PLAN USAGE), points the app's hub line at a
// fake hub of its own, and plays the 20 synthetic prompts into the voice chat the way the phone
// would. It records how long each answer took, what the spoken-answer cleanup had to take out,
// how long the answers are and how often the chat stays quiet about news, and writes a table
// and every reply for the listen-through. See README.md.
//
// Nothing here runs from `npm test` or `npm run e2e`, and without HELM_LIVE_CLAUDE=1 it only
// prints what it would do. No model server is contacted except the ones named below, by you.

import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { api, dispose, launchApp, quitGracefully, REPO, scratchDir, type App } from "../e2e/harness.ts";
import { startFakeHub, type FakeHub, type Frame } from "./fake-hub.ts";
import { choose, type Prompt } from "./prompts.ts";
import { REMOVED_KINDS, countWords, resultsTable, repliesDocument, summarise, type Candidate, type Outcome, type Removed, type Sample } from "./report.ts";

// Made up, used only between this runner and the app it starts.
const TOKEN = "voice-bench-line-token-0123456789abcdef";
const WAITING_WORDS = "approve something on the PC";
/** After the chat's answer shows in the transcript, how long a news prompt waits for it to go up the line. */
const NEWS_GRACE_MS = 4_000;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

interface Spec {
  key: string;
  label: string;
  model: string;
  effort: string;
  /** A profile folder to run against instead of a fresh scratch one (the Strata candidate needs its server set up). */
  profile?: string;
  env?: Record<string, string>;
}

interface Options {
  quick: boolean;
  only?: string[];
  timeoutS: number;
  out?: string;
  recount?: string;
}

const USAGE = `Usage: HELM_LIVE_CLAUDE=1 npm run voice-bench -w checks -- [--quick] [--only haiku,sonnet-low] [--timeout 240] [--out <dir>]

  --quick     seven prompts instead of twenty (a cheap trial)
  --only      candidates to run: haiku, sonnet-low, sonnet-medium, strata
  --timeout   seconds to wait for one answer (default 240)
  --out       where to write the results (default checks/voice-bench/out/<time>)
  --recount   <results folder>: redo only the cleanup counts for an earlier run (starts no app, spends nothing)

Without HELM_LIVE_CLAUDE=1 nothing is started; the plan is printed and that is all.`;

function parseArgs(argv: string[]): Options {
  const o: Options = { quick: false, timeoutS: 240 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--quick") o.quick = true;
    else if (a === "--only") o.only = (argv[++i] ?? "").split(",").filter(Boolean);
    else if (a === "--timeout") o.timeoutS = Number(argv[++i]);
    else if (a === "--out") o.out = argv[++i];
    else if (a === "--recount") o.recount = argv[++i];
    else if (a === "--help" || a === "-h") {
      console.log(USAGE);
      process.exit(0);
    } else throw new Error(`Unknown option ${a}.\n${USAGE}`);
  }
  if (!Number.isFinite(o.timeoutS) || o.timeoutS < 10) throw new Error("--timeout must be at least 10 seconds.");
  return o;
}

/** The candidates. Model ids are overridable because they are whatever the installed `claude` calls them. */
function specs(): Spec[] {
  const e = process.env;
  const list: Spec[] = [
    // Haiku takes the app's usual effort: it may not offer the effort levels.
    { key: "haiku", label: "Haiku 4.5", model: e.HELM_BENCH_HAIKU_MODEL || "haiku", effort: "" },
    { key: "sonnet-low", label: "Sonnet 5.5, low effort", model: e.HELM_BENCH_SONNET_MODEL || "claude-sonnet-5-5", effort: "low" },
    { key: "sonnet-medium", label: "Sonnet 5.5, medium effort", model: e.HELM_BENCH_SONNET_MODEL || "claude-sonnet-5-5", effort: "medium" },
  ];
  if (e.HELM_BENCH_STRATA_MODEL) {
    const addr = e.HELM_BENCH_STRATA_ADDR ?? "";
    const profile = e.HELM_BENCH_STRATA_PROFILE ?? "";
    if (!/^http:\/\/(127\.0\.0\.1|localhost|\[::1\]):\d+\/?$/.test(addr)) {
      throw new Error("HELM_BENCH_STRATA_ADDR must be the model server's loopback address, like http://127.0.0.1:8080.");
    }
    if (!profile || !fs.existsSync(profile)) {
      throw new Error("HELM_BENCH_STRATA_PROFILE must be a scratch COPY of a Helm profile folder in which the Strata server is set up in Settings.");
    }
    list.push({ key: "strata", label: "Strata", model: e.HELM_BENCH_STRATA_MODEL, effort: "", profile, env: { HELM_TEST_LOCAL_SERVERS: addr } });
  }
  return list;
}

// ---- reading the app ----------------------------------------------------------------------

type Row = { id: number; text: string };

async function transcript(app: App, chat: string): Promise<any[]> {
  const page = (await api(app.page, "transcript_page", { chatId: chat })) as { messages?: any[] };
  return page.messages ?? [];
}

async function lastRow(app: App, chat: string): Promise<number> {
  return (await transcript(app, chat)).reduce((m, r) => Math.max(m, Number(r.id)), 0);
}

/** The newest answer the chat wrote after row `after`: what its turn ended with. */
async function answerRow(app: App, chat: string, after: number): Promise<Row | undefined> {
  const rows = await transcript(app, chat);
  for (let i = rows.length - 1; i >= 0; i--) {
    const r = rows[i];
    if (Number(r.id) > after && r.role === "assistant" && r.details?.stopReason === "end_turn" && String(r.text).trim()) {
      return { id: Number(r.id), text: String(r.text) };
    }
  }
  return undefined;
}

async function voiceChatId(app: App): Promise<string | undefined> {
  const v = (await api(app.page, "relay_voice_get", {})) as { chat?: { id?: string } | null };
  return v.chat?.id;
}

/** Waits until the chat has no turn running, so the next prompt is not steered into a turn that is winding down. */
async function settle(app: App, chat: string, ms = 60_000): Promise<void> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const all = (await api(app.page, "chat_list", {})) as { id: string; status: string }[];
    if (all.find((c) => c.id === chat)?.status === "idle") return;
    await sleep(500);
  }
}

const isQuiet = (text: string): boolean => text.replace(/\s+/g, "").toLowerCase() === "[quiet]";

// ---- one prompt ---------------------------------------------------------------------------

interface Run {
  app: App;
  hub: FakeHub;
  chat?: string;
  lastTurn?: string;
  timeoutMs: number;
}

async function ask(run: Run, p: Prompt): Promise<Sample> {
  const { app, hub } = run;
  const from = hub.frames.length;
  const before = run.chat ? await lastRow(app, run.chat) : 0;
  const turn = randomUUID();
  const t0 = performance.now();
  if (p.kind === "turn") {
    hub.send({
      type: "voice",
      id: turn,
      mode: "conversation",
      text: p.text,
      source: "speech",
      sentAt: Date.now(),
      ageMinutes: null,
      continues: p.continues ? (run.lastTurn ?? null) : null,
    });
    run.lastTurn = turn;
  } else {
    if (!run.chat) throw new Error(`${p.id} is news, which needs the voice chat to exist: put a turn before it.`);
    // The way another chat's message reaches the voice chat (the same path the hub's tool call takes).
    hub.send({ type: "call", id: turn, client: "remote", tool: "app_send_message", args: { chatId: run.chat, message: p.text, mode: "steer" } });
    const sent = await hub.waitFor((f) => f.type === "result" && f.id === turn, 30_000, from);
    if (!sent.ok) throw new Error(`The message to the voice chat was refused: ${sent.error}`);
  }

  let ack: Frame | undefined;
  let final: Frame | undefined;
  let row: Row | undefined;
  let rowAt = 0;
  const deadline = t0 + run.timeoutMs;
  for (;;) {
    const frames = hub.frames.slice(from);
    ack ??= p.kind === "turn" ? frames.find((f) => f.type === "voice_ack" && f.id === turn) : undefined;
    if (!run.chat && ack) run.chat = await voiceChatId(app);
    final = frames.find((f) => (p.kind === "turn" ? f.type === "voice_final" && f.turn === turn : f.type === "voice_notify"));
    if (final) break;
    if (run.chat && !row) {
      row = await answerRow(app, run.chat, before);
      if (row) rowAt = performance.now();
    }
    // News the chat judged not worth telling goes nowhere: its answer is only in the chat.
    if (p.kind === "news" && row && performance.now() - rowAt > NEWS_GRACE_MS) break;
    if (performance.now() > deadline) break;
    await sleep(250);
  }
  // What the chat wrote, for the cleanup counts and the listen-through (its row lands a moment before the answer goes up).
  for (let i = 0; run.chat && !row && final && i < 20; i++) {
    row = await answerRow(app, run.chat, before);
    if (!row) await sleep(250);
  }

  const frames = hub.frames.slice(from);
  const sentences: string[] = Array.isArray(final?.sentences) ? final!.sentences.map((s: { text: string }) => s.text) : [];
  const raw = row?.text ?? "";
  let outcome: Outcome;
  if (final) outcome = String(final.text ?? "").includes(WAITING_WORDS) ? "waiting" : "answered";
  else if (row && isQuiet(row.text)) outcome = "quiet";
  else if (row) outcome = "answered";
  else outcome = "timeout";
  if (outcome === "timeout" && p.kind === "turn") hub.send({ type: "voice_stop", turn });
  if (run.chat) await settle(app, run.chat);

  return {
    prompt: p,
    outcome,
    ackMs: ack ? hub.arrivedAt(ack) - t0 : undefined,
    answerMs: final ? hub.arrivedAt(final) - t0 : undefined,
    statusFrames: frames.filter((f) => f.type === "voice_status" && f.turn === turn).length,
    sentences,
    truncated: final?.truncated === true,
    raw,
    rawWords: countWords(raw),
  };
}

// ---- one candidate ------------------------------------------------------------------------

let current: App | undefined;

async function runCandidate(spec: Spec, prompts: Prompt[], timeoutMs: number): Promise<Candidate> {
  console.log(`\n== ${spec.label} (${spec.model}${spec.effort ? `, ${spec.effort}` : ""})`);
  const hub = await startFakeHub({ token: TOKEN });
  const samples: Sample[] = [];
  let app: App | undefined;
  try {
    const profile = spec.profile ?? scratchDir(`helm-voice-bench-${spec.key}-`);
    app = current = await launchApp(profile, {
      fake: false,
      env: { HELM_TEST_RELAY_URL: hub.url, HELM_TEST_RELAY_TOKEN: TOKEN, ...(spec.env ?? {}) },
    });
    await hub.waitConnected(60_000);
    await hub.waitFor((f) => f.type === "sharing", 15_000);
    hub.send({ type: "voice_presence", live: true, mode: "conversation" });
    // The idle and age limits are lifted so one conversation lasts the whole run; the watch is off.
    await api(app.page, "relay_voice_save", { model: spec.model, effort: spec.effort, idleMinutes: 1440, maxAgeHours: 720, maxTokens: 5_000_000, watch: "off" });
    const run: Run = { app, hub, timeoutMs };
    for (const p of prompts) {
      const s = await ask(run, p);
      samples.push(s);
      const took = s.answerMs === undefined ? "no answer" : `${(s.answerMs / 1000).toFixed(1)} s`;
      console.log(`  ${p.id} ${s.outcome.padEnd(8)} ${took.padStart(9)}  ${countWords(s.sentences.join(" "))} words  (${p.tests})`);
    }
  } finally {
    if (app) await quitGracefully(app).catch(() => undefined);
    await dispose(app);
    current = undefined;
    await hub.close();
  }
  return { key: spec.key, label: spec.label, model: spec.model, effort: spec.effort, samples };
}

// ---- the cleanup counts -------------------------------------------------------------------

/** Runs the cleanup over every answer the chats wrote (a tiny Rust program that reads and prints JSON; it contacts nothing). */
function countRemoved(candidates: Candidate[]): void {
  const todo = candidates.flatMap((c) => c.samples).filter((s) => s.raw.trim());
  if (todo.length === 0) return;
  const r = spawnSync("cargo", ["run", "-q", "-p", "helm-relay", "--example", "speakable_counts"], {
    cwd: REPO,
    input: JSON.stringify(todo.map((s) => s.raw)),
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  });
  if (r.error || r.status !== 0) {
    console.warn(`\nThe cleanup counts were skipped (${r.error?.message ?? (r.stderr ?? "").slice(-300)}). Put cargo on PATH and run \`cargo build -p helm-relay --examples\`, then count again with --recount <the results folder>.`);
    return;
  }
  const counts = JSON.parse(r.stdout) as Record<string, number>[];
  todo.forEach((s, i) => {
    const c = counts[i];
    const removed = {} as Removed;
    for (const k of REMOVED_KINDS) removed[k] = c[k] ?? 0;
    s.removed = removed;
  });
}

// ---- output -------------------------------------------------------------------------------

function method(prompts: Prompt[]): string {
  return [
    `${prompts.length} synthetic prompts (${prompts.filter((p) => p.kind === "turn").length} spoken turns, ${prompts.filter((p) => p.kind === "news").length} pieces of news), one conversation per candidate.`,
    "",
    "- **Median to answer / slowest 10%**: from the turn reaching Helm to its answer arriving on the line. On the phone, add speech-to-text and the network; measure those once on the device.",
    "- **Median spoken**: words the phone would say, and how long that takes at a normal pace (about 155 words a minute).",
    "- **Over 75 words**: answers longer than the notes ask for. **Cut at the cap**: answers the 150-word cap had to cut.",
    "- **Cleanup touched / taken out**: spoken turns whose answer had code, tables, links, addresses, paths or ids to remove, and how many of each.",
    "- **News told right**: news where the chat told you when it should, or stayed quiet when it should. **Quiet**: how often it stayed quiet.",
    "- **Listen**: yours to fill in from the replies files.",
  ].join("\n");
}

function write(dir: string, prompts: Prompt[], candidates: Candidate[]): string {
  fs.mkdirSync(dir, { recursive: true });
  const table = resultsTable(candidates);
  fs.writeFileSync(path.join(dir, "results.json"), JSON.stringify(candidates.map((c) => ({ ...c, summary: summarise(c.samples) })), null, 2));
  fs.writeFileSync(path.join(dir, "results.md"), `# Voice chat benchmark\n\n${method(prompts)}\n\n${table}\n`);
  for (const c of candidates) fs.writeFileSync(path.join(dir, `replies-${c.key}.md`), repliesDocument(c));
  return table;
}

/** Counts again for a finished run: reads its results.json and writes the files afresh. No app, no model. */
function recount(dir: string): void {
  const saved = JSON.parse(fs.readFileSync(path.join(dir, "results.json"), "utf8")) as (Candidate & { summary?: unknown })[];
  const candidates: Candidate[] = saved.map(({ summary: _summary, ...c }) => c);
  for (const c of candidates) for (const s of c.samples) delete s.removed;
  countRemoved(candidates);
  const prompts = candidates[0]?.samples.map((s) => s.prompt) ?? [];
  console.log(`${write(dir, prompts, candidates)}\n\nCounted again in ${dir}`);
}

async function main(): Promise<void> {
  const o = parseArgs(process.argv.slice(2));
  if (o.recount) return recount(path.resolve(o.recount));
  const prompts = choose(o.quick);
  let list = specs();
  if (o.only) {
    const unknown = o.only.filter((k) => !list.some((s) => s.key === k));
    if (unknown.length) throw new Error(`No such candidate: ${unknown.join(", ")}. Known: ${list.map((s) => s.key).join(", ")}.`);
    list = list.filter((s) => o.only!.includes(s.key));
  }
  const dir = o.out ?? path.join(REPO, "checks", "voice-bench", "out", new Date().toISOString().replace(/[:.]/g, "-"));
  console.log(`Voice chat benchmark: ${prompts.length} prompts × ${list.length} candidates (${list.map((s) => s.key).join(", ")}).`);
  console.log(`Results go to ${dir}`);
  if (process.env.HELM_LIVE_CLAUDE !== "1") {
    console.log("\nNothing was started. A run sends every prompt to the real model under your sign-in and spends plan usage.");
    console.log("Set HELM_LIVE_CLAUDE=1 to run it. Build the debug app first (`npm run build:debug`).");
    return;
  }
  process.on("SIGINT", () => {
    void dispose(current).finally(() => process.exit(130));
  });
  const done: Candidate[] = [];
  for (const spec of list) {
    done.push(await runCandidate(spec, prompts, o.timeoutS * 1000));
    write(dir, prompts, done); // after each candidate, so an interrupted run keeps what it has
  }
  countRemoved(done);
  console.log(`\n${write(dir, prompts, done)}\n\nReplies for the listen-through are in ${dir}`);
}

main().catch((e) => {
  console.error(e instanceof Error ? e.message : e);
  process.exitCode = 1;
});
