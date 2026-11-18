// Read-only search-quality run against a live hub (not part of the hub image).
//
//   HELM_HUB_URL=http://host:port HELM_HUB_TOKEN=<read token> node eval/live.mjs queries.jsonl [out.md]
//
// `queries.jsonl` has one JSON object per line:
//   {"class": "paraphrase", "text": "how do I know my copies work", "expect": ["Lesson on trusting backups"]}
// An expected entry matches a result whose title or `where` contains it (case-insensitive). The run
// calls only `memory_search`, reports recall@5 and MRR per class, search time p50/p95 and the
// `searched.meaning` path seen. The token is read from the environment and never printed.
import { readFileSync, writeFileSync } from "node:fs";

const url = (process.env.HELM_HUB_URL ?? "").replace(/\/+$/, "");
const token = process.env.HELM_HUB_TOKEN ?? "";
const [file, out] = process.argv.slice(2);
if (!url || !token || !file) {
  console.error("usage: HELM_HUB_URL=... HELM_HUB_TOKEN=... node eval/live.mjs queries.jsonl [out.md]");
  process.exit(2);
}

const queries = readFileSync(file, "utf8")
  .split("\n")
  .filter((l) => l.trim())
  .map((l) => JSON.parse(l));

async function search(text) {
  const started = performance.now();
  const res = await fetch(`${url}/mcp`, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "memory_search", arguments: { query: text, k: 10 } },
    }),
  });
  const body = await res.json();
  const result = body.result?.structuredContent;
  if (!result) throw new Error(`search failed: HTTP ${res.status}`);
  return { result, ms: performance.now() - started };
}

const matches = (hit, want) =>
  `${hit.title ?? ""}\n${hit.where ?? ""}`.toLowerCase().includes(want.toLowerCase());
const pct = (xs, p) => [...xs].sort((a, b) => a - b)[Math.round((xs.length - 1) * p)];

const byClass = new Map();
const times = [];
const meanings = new Set();
for (const q of queries) {
  const { result, ms } = await search(q.text);
  times.push(ms);
  meanings.add(result.searched?.meaning ?? "unknown");
  const hits = result.results ?? [];
  const top5 = hits.slice(0, 5);
  const found = q.expect.filter((w) => top5.some((h) => matches(h, w))).length;
  const first = hits.findIndex((h) => q.expect.some((w) => matches(h, w)));
  const row = byClass.get(q.class) ?? [];
  row.push({ recall: q.expect.length ? found / q.expect.length : 1, rr: first < 0 ? 0 : 1 / (first + 1) });
  byClass.set(q.class, row);
}

const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
let report = "| class | queries | recall@5 | MRR |\n|---|---|---|---|\n";
for (const [cls, rows] of [...byClass].sort()) {
  report += `| ${cls} | ${rows.length} | ${mean(rows.map((r) => r.recall)).toFixed(2)} | ${mean(rows.map((r) => r.rr)).toFixed(2)} |\n`;
}
report += `\nsearch time: p50 ${pct(times, 0.5).toFixed(0)} ms, p95 ${pct(times, 0.95).toFixed(0)} ms\n`;
report += `meaning path: ${[...meanings].join("; ")}\n`;
console.log(report);
if (out) writeFileSync(out, report);
