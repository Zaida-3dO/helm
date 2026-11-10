// What a benchmark run keeps for each prompt, and how it is turned into the table the user reads.
// Pure functions only: nothing here starts an app or contacts anything.

import type { Prompt } from "./prompts.ts";

/** What the spoken-answer cleanup took out of one answer (host/crates/relay/examples/speakable_counts.rs). */
export interface Removed {
  code: number;
  tables: number;
  links: number;
  urls: number;
  paths: number;
  ids: number;
  markup: number;
}

export const REMOVED_KINDS = ["code", "tables", "links", "urls", "paths", "ids", "markup"] as const;

export type Outcome =
  /** The chat answered. */
  | "answered"
  /** News that the chat judged not worth telling (it answered exactly `[quiet]`). */
  | "quiet"
  /** The turn stopped at an approval only Helm's window can give. */
  | "waiting"
  /** No answer inside the time allowed. */
  | "timeout";

export interface Sample {
  prompt: Prompt;
  outcome: Outcome;
  /** Milliseconds from sending to the hub-side acknowledgement (the turn is in the chat). */
  ackMs?: number;
  /** Milliseconds from sending to the answer arriving on the line (`voice_final` or `voice_notify`). */
  answerMs?: number;
  statusFrames: number;
  /** What went up the line: the cleaned, capped sentences. */
  sentences: string[];
  truncated: boolean;
  /** What the chat wrote, before the cleanup. */
  raw: string;
  /** Filled in once the cleanup has been run over `raw`. */
  removed?: Removed;
  /** Words in the raw answer. */
  rawWords: number;
}

export interface Candidate {
  key: string;
  label: string;
  model: string;
  effort: string;
  samples: Sample[];
}

/** About 155 words a minute, an ordinary speaking pace for a text-to-speech voice. */
export const WORDS_PER_SECOND = 2.6;

export const countWords = (text: string): number => text.split(/\s+/).filter(Boolean).length;

export function percentile(values: number[], p: number): number | undefined {
  if (values.length === 0) return undefined;
  const sorted = [...values].sort((a, b) => a - b);
  const at = Math.ceil((p / 100) * sorted.length) - 1;
  return sorted[Math.min(sorted.length - 1, Math.max(0, at))];
}

export interface Summary {
  prompts: number;
  /** Turns the phone started (not news). */
  turns: number;
  answered: number;
  waiting: number;
  timeouts: number;
  medianAnswerS?: number;
  p90AnswerS?: number;
  /** Median words that would be spoken, and how long that takes at a normal pace. */
  medianSpokenWords?: number;
  medianSpokenS?: number;
  /** Answers over the 75 words the notes aim for. */
  overAim: number;
  /** Answers the 150-word cap cut. */
  cut: number;
  /** Turns whose answer the cleanup had to take something out of, and how many things by kind. */
  cleaned: number;
  cleanedKnown: number;
  removed: Removed;
  news: number;
  quiet: number;
  /** News where the chat did what was wanted: told the user, or stayed quiet. */
  newsRight: number;
}

export const noneRemoved = (): Removed => ({ code: 0, tables: 0, links: 0, urls: 0, paths: 0, ids: 0, markup: 0 });

export const anyRemoved = (r: Removed): boolean => REMOVED_KINDS.some((k) => r[k] > 0);

export function summarise(samples: Sample[]): Summary {
  const turns = samples.filter((s) => s.prompt.kind === "turn");
  const news = samples.filter((s) => s.prompt.kind === "news");
  const spoken = samples.filter((s) => s.outcome === "answered");
  const times = spoken.flatMap((s) => (s.answerMs === undefined ? [] : [s.answerMs / 1000]));
  const words = spoken.map((s) => countWords(s.sentences.join(" ")));
  const medianWords = percentile(words, 50);
  const removed = noneRemoved();
  let cleaned = 0;
  let cleanedKnown = 0;
  for (const s of turns) {
    if (!s.removed) continue;
    cleanedKnown++;
    if (anyRemoved(s.removed)) cleaned++;
    for (const k of REMOVED_KINDS) removed[k] += s.removed[k];
  }
  const quiet = news.filter((s) => s.outcome === "quiet").length;
  const newsRight = news.filter((s) => (s.prompt.expect === "quiet") === (s.outcome === "quiet") && s.outcome !== "timeout").length;
  return {
    prompts: samples.length,
    turns: turns.length,
    answered: samples.filter((s) => s.outcome === "answered").length,
    waiting: samples.filter((s) => s.outcome === "waiting").length,
    timeouts: samples.filter((s) => s.outcome === "timeout").length,
    medianAnswerS: percentile(times, 50),
    p90AnswerS: percentile(times, 90),
    medianSpokenWords: medianWords,
    medianSpokenS: medianWords === undefined ? undefined : medianWords / WORDS_PER_SECOND,
    overAim: words.filter((w) => w > 75).length,
    cut: spoken.filter((s) => s.truncated).length,
    cleaned,
    cleanedKnown,
    removed,
    news: news.length,
    quiet,
    newsRight,
  };
}

const secs = (v: number | undefined): string => (v === undefined ? "n/a" : `${v.toFixed(1)} s`);
const num = (v: number | undefined): string => (v === undefined ? "n/a" : String(Math.round(v)));

/** The removed counts as `code 2, urls 3`, or `none`. */
export function removedText(r: Removed): string {
  const parts = REMOVED_KINDS.filter((k) => r[k] > 0).map((k) => `${k} ${r[k]}`);
  return parts.length ? parts.join(", ") : "none";
}

/** One row per candidate, for the user to compare. The listen-through column is theirs to fill in. */
export function resultsTable(candidates: Candidate[]): string {
  const head = [
    "Candidate",
    "Answered",
    "Median to answer",
    "Slowest 10%",
    "Median spoken",
    "Over 75 words",
    "Cut at the cap",
    "Cleanup touched",
    "Taken out",
    "News told right",
    "Quiet",
    "Listen (1-5)",
  ];
  const rows = candidates.map((c) => {
    const s = summarise(c.samples);
    const problems = [s.waiting ? `${s.waiting} waiting on an approval` : "", s.timeouts ? `${s.timeouts} timed out` : ""].filter(Boolean).join(", ");
    return [
      `${c.label} (${c.model}${c.effort ? `, ${c.effort}` : ""})`,
      `${s.answered} of ${s.prompts}${problems ? ` (${problems})` : ""}`,
      secs(s.medianAnswerS),
      secs(s.p90AnswerS),
      s.medianSpokenWords === undefined ? "n/a" : `${num(s.medianSpokenWords)} words, ${secs(s.medianSpokenS)}`,
      String(s.overAim),
      String(s.cut),
      s.cleanedKnown ? `${s.cleaned} of ${s.cleanedKnown} turns` : "not counted",
      s.cleanedKnown ? removedText(s.removed) : "not counted",
      s.news ? `${s.newsRight} of ${s.news}` : "n/a",
      s.news ? `${s.quiet} of ${s.news}` : "n/a",
      "",
    ];
  });
  const line = (cells: string[]) => `| ${cells.join(" | ")} |`;
  return [line(head), line(head.map(() => "---")), ...rows.map(line)].join("\n");
}

/** Every prompt and reply of one candidate, for the listen-through. Synthetic text only. */
export function repliesDocument(c: Candidate): string {
  const out = [`# ${c.label}`, "", `Model \`${c.model}\`${c.effort ? `, effort ${c.effort}` : ""}. Score each reply 1 to 5 by ear.`, ""];
  for (const s of c.samples) {
    out.push(`## ${s.prompt.id} (${s.prompt.tests})`, "", `> ${s.prompt.text}`, "");
    const timing = s.answerMs === undefined ? "no answer" : `${(s.answerMs / 1000).toFixed(1)} s`;
    out.push(`Outcome: ${s.outcome} · ${timing} · ${s.statusFrames} working lines`, "");
    if (s.outcome === "quiet") out.push(`The chat stayed quiet. Wanted: ${s.prompt.expect ?? "n/a"}.`, "");
    else if (s.sentences.length) {
      out.push(`Spoken (${countWords(s.sentences.join(" "))} words${s.truncated ? ", cut at the cap" : ""}):`, "");
      s.sentences.forEach((t, i) => out.push(`${i + 1}. ${t}`));
      out.push("");
    }
    if (s.removed && anyRemoved(s.removed)) out.push(`Cleanup took out: ${removedText(s.removed)}.`, "");
    if (s.raw && s.raw.trim() !== s.sentences.join(" ").trim()) out.push("As the chat wrote it:", "", "```", s.raw.trim(), "```", "");
    out.push("Score: ", "");
  }
  return out.join("\n");
}
