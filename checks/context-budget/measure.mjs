// What a fresh chat's first request is made of.
//
//   node checks/context-budget/measure.mjs <session.jsonl>... [--json]
//
// Claude Code writes each session's first request into its log: a `prompt_snapshot` with the system
// prompt and the tool schemas it sent, and attachments for the CLAUDE.md files, the skill listing,
// the deferred tool names and the agent types. The first reply carries the real token count
// (input + cache read + cache creation). The script splits the characters by part and scales
// them to that total, so the rows are estimates (about +-15%) and the totals are exact.
//
// It reads a log and prints numbers; it never sends anything anywhere. `--json` prints one object
// per log instead of a table.

import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

/** The part a system-prompt block belongs to. Helm's own launch prompt starts with one of these. */
function promptPart(s, i) {
  if (i === 0) return "Claude Code system prompt (static)";
  if (/^(You are working with|Helm shows your links|Helm's tools|Rules for everyone)/m.test(s.slice(0, 400)) || /\bHelm\b/.test(s.slice(0, 200))) {
    return "Helm launch prompt";
  }
  return "Claude Code system prompt (dynamic)";
}

/** One log's breakdown: `{ file, model, tokens, chars, ratio, parts: [{part, chars, tokens}], tools }`. */
export function measure(file) {
  const lines = fs.readFileSync(file, "utf8").split("\n").filter(Boolean);
  const parts = new Map();
  const add = (k, n) => parts.set(k, (parts.get(k) ?? 0) + n);
  let usage;
  let model = "";
  let snap;
  for (const l of lines) {
    let j;
    try {
      j = JSON.parse(l);
    } catch {
      continue;
    }
    if (j.type === "assistant") {
      if (!usage && j.message?.usage) {
        usage = j.message.usage;
        model = j.message.model ?? "";
      }
      continue;
    }
    const a = j.attachment;
    if (!a) {
      if (j.type === "user" && !usage) add("first user message", JSON.stringify(j.message?.content ?? "").length);
      continue;
    }
    if (a.type === "prompt_snapshot" && a.tools && !snap) snap = a;
    if (usage) continue;
    if (a.type === "skill_listing") add(`skills list (${a.skillCount})`, a.content.length);
    if (a.type === "deferred_tools_delta") add(`deferred tool names (${a.addedNames.length})`, (a.addedLines || a.addedNames).join("\n").length);
    if (a.type === "agent_listing_delta") add(`agent types (${a.addedTypes.length})`, (a.addedLines || []).join("\n").length);
    if (a.type === "instructions") for (const f of a.files) add(`CLAUDE.md or memory: ${f.path.split(/[\/]/).slice(-3).join("/")}`, f.content.length);
    if (a.type === "mcp_instructions_delta") add("MCP server instructions", JSON.stringify(a.addedBlocks || "").length);
    if (a.type === "hook_additional_context" || a.type === "hook_success") add("hook output", JSON.stringify(a.content || "").length);
    if (a.type === "session_context") add("session context", JSON.stringify(a.context).length);
  }
  const tools = { builtin: 0, helm: 0, otherMcp: 0, count: 0, helmCount: 0 };
  if (snap) {
    snap.systemPrompt.forEach((s, i) => add(promptPart(s, i), s.length));
    for (const t of snap.tools) {
      const n = JSON.stringify(t).length;
      const m = (t.name || "").match(/^mcp__(.+?)__/);
      tools.count += 1;
      if (!m) {
        add("tool schemas: Claude Code built-ins", n);
        tools.builtin += n;
      } else if (m[1] === "helm") {
        add("tool schemas: Helm host tools", n);
        tools.helm += n;
        tools.helmCount += 1;
      } else {
        add(`tool schemas: mcp ${m[1]}`, n);
        tools.otherMcp += n;
      }
    }
  }
  const chars = [...parts.values()].reduce((a, b) => a + b, 0);
  const tokens = usage ? (usage.input_tokens || 0) + (usage.cache_creation_input_tokens || 0) + (usage.cache_read_input_tokens || 0) : 0;
  const ratio = tokens ? chars / tokens : 1;
  const rows = [...parts.entries()].sort((a, b) => b[1] - a[1]).map(([part, c]) => ({ part, chars: c, tokens: Math.round(c / ratio) }));
  return { file: path.basename(file), model, tokens, chars, ratio: Number(ratio.toFixed(2)), parts: rows, tools };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  const json = args.includes("--json");
  const files = args.filter((a) => !a.startsWith("--"));
  if (files.length === 0) {
    console.error("usage: node measure.mjs <session.jsonl>... [--json]");
    process.exit(2);
  }
  for (const f of files) {
    const r = measure(f);
    if (json) {
      console.log(JSON.stringify(r));
      continue;
    }
    console.log(`\n${r.file}  ${r.model || "(no reply)"}  first request ${r.tokens} tokens, ${r.chars} chars (${r.ratio} chars/token); ${r.tools.count} tool schemas inline`);
    for (const p of r.parts) console.log(`${String(p.tokens).padStart(7)} tok ${String(p.chars).padStart(8)} chars  ${p.part}`);
  }
}
