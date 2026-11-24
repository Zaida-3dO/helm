// Chat summaries for the sidebar hover card. The host starts this helper once per job.
//
// - The prompt asks for exactly two short sentences — what the conversation is about, and
//   where it stands now — with the transcript treated as data, not as instructions.
// - A retry uses a stricter prompt. The answer is cleaned: Markdown marks, quotes and control
//   characters out, whitespace folded, and a too-long answer cut at the last whole word that
//   fits the card. Only an answer with nothing left in it gives no summary.
// - The launch reuses the titles helper's print-mode call: the unmodified `claude` binary, in a
//   fresh empty folder with `--setting-sources ""`, no tools and no MCP servers, thinking off.
//   When the job names a local model server the same loopback-only rule and environment hand-
//   over apply (key in HELM_LOCAL_MODEL_KEY, read once, never passed on); the host always routes
//   these jobs to a local server, so a hover never spends the Claude plan.
// - One attempt plus one stricter retry, each bounded by `timeoutMs`; the host keeps its own,
//   slightly longer, deadline.

import { askClaude, headTail, type TitleJob } from "../../titles/src/title.ts";

export interface SummarizeMessage {
  role: "user" | "assistant";
  /** The message text, head and tail kept by the host when long. */
  text: string;
}

export interface SummarizeJob {
  /** The chat's title, as context for what it is about. */
  title?: string;
  /** The newest messages, oldest first. */
  messages: SummarizeMessage[];
  /** Milliseconds per attempt; default 9 s. */
  timeoutMs?: number;
  /** The model and route: always a local model server when the host starts this helper. */
  model?: string;
  localBaseUrl?: string;
  localContextTokens?: number;
  localMaxOutputTokens?: number;
  command?: string;
  commandArgs?: string[];
}

export const MAX_SUMMARY_CHARS = 240;
export const MAX_MESSAGES = 40;
/** Per-message cap whatever the host sent: the transcript box for the model. */
export const MESSAGE_CHARS = 900;

export function buildPrompt(job: SummarizeJob, strict: boolean): string {
  const ask = strict
    ? "Reply with only two short sentences about the conversation below: the first says what it is about, the second says where it stands now. At most 40 words, no preamble, nothing else."
    : "Write exactly two short sentences about the conversation below. The first says what the conversation is about. The second says where it stands now: what has been done and what it is waiting on or working on next. Plain words, at most 60 words in total, no Markdown, no lists, no names of tools. Do not copy sentences from the transcript; say it in your own words.";
  const parts = [ask];
  if (job.title?.trim()) parts.push(`<title>\n${job.title.trim().slice(0, 200)}\n</title>`);
  const transcript = job.messages
    .map((m) => `${m.role}: ${headTail(m.text, MESSAGE_CHARS)}`)
    .join("\n");
  parts.push(`<transcript>\n${transcript}\n</transcript>`);
  // A long transcript can bury the ask, so it is said again after the data.
  parts.push("Now reply with only the two sentences, beginning at once.");
  return parts.join("\n\n");
}

/** Clean a model answer into a summary: one plain line. Empty when nothing real is left. */
export function cleanSummary(raw: string): string | null {
  let t = raw
    .replace(/[*_`#>|]/g, "")
    .replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, "")
    .split(/\s+/)
    .join(" ")
    .trim();
  t = t.replace(/^["'“”‘’\s]+|["'“”‘’\s]+$/g, "");
  if (!t) return null;
  if (t.length > MAX_SUMMARY_CHARS) {
    const cut = t.slice(0, MAX_SUMMARY_CHARS);
    const space = cut.lastIndexOf(" ");
    t = (space >= MAX_SUMMARY_CHARS / 2 ? cut.slice(0, space) : cut).trimEnd();
    t = t.replace(/[,;:.]+$/, "");
  }
  return t || null;
}

/** The job: one attempt, then one stricter retry; the transcript is what it is. */
export async function makeSummary(job: SummarizeJob): Promise<{ summary: string } | { error: string }> {
  try {
    if (!Array.isArray(job.messages) || job.messages.length === 0) {
      return { error: "no messages to summarise" };
    }
    const messages = job.messages.slice(-MAX_MESSAGES);
    let last = "the answer held no summary";
    for (const strict of [false, true]) {
      try {
        // The titles helper's print-mode call: it reads only the model/route/command fields,
        // and this job carries the same ones.
        const raw = await askClaude(job as unknown as TitleJob, buildPrompt({ ...job, messages }, strict));
        const t = cleanSummary(raw);
        if (t) return { summary: t };
      } catch (e) {
        last = e instanceof Error ? e.message : String(e);
      }
    }
    return { error: last };
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) };
  }
}
