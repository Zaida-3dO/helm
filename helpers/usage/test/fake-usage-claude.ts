// A stand-in claude for the usage helper's tests and the w4-usage e2e: answers the control
// requests the control reader sends and logs everything it receives to $FAKE_USAGE_LOG.
//
// Usage: node fake-usage-claude.ts <scenario.json> [the real CLI's flags, logged and ignored]
// Scenario: { "usage": <get_usage response body> | { "error": text } | "silent" | "hang" | "exit",
//             "version"?: text }

import fs from "node:fs";

const scenario = JSON.parse(fs.readFileSync(process.argv[2], "utf8")) as {
  usage?: unknown;
  version?: string;
};
const logPath = process.env.FAKE_USAGE_LOG;
const log = (entry: unknown) => {
  if (logPath) fs.appendFileSync(logPath, JSON.stringify(entry) + "\n");
};
log({ kind: "start", argv: process.argv.slice(3), cwd: process.cwd(), envKeys: Object.keys(process.env).sort() });

const out = (o: unknown) => process.stdout.write(JSON.stringify(o) + "\n");
const ok = (request_id: unknown, response: unknown) =>
  out({ type: "control_response", response: { subtype: "success", request_id, response } });

let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d: string) => {
  buf += d;
  let nl: number;
  while ((nl = buf.indexOf("\n")) !== -1) {
    const line = buf.slice(0, nl);
    buf = buf.slice(nl + 1);
    if (!line.trim()) continue;
    const rec = JSON.parse(line) as { type?: string; request_id?: string; request?: { subtype?: string } };
    log({ kind: "in", rec });
    if (rec.type !== "control_request") continue;
    switch (rec.request?.subtype) {
      case "initialize":
        ok(rec.request_id, { commands: [], models: [] });
        break;
      case "get_binary_version":
        ok(rec.request_id, { version: scenario.version ?? "0.0.0-fake" });
        break;
      case "get_usage": {
        const u = scenario.usage;
        if (u === "hang") break;
        if (u === "exit") process.exit(0);
        if (u === "silent" || u === undefined) break;
        if (u && typeof u === "object" && "error" in u) {
          out({ type: "control_response", response: { subtype: "error", request_id: rec.request_id, error: String((u as { error: unknown }).error) } });
          break;
        }
        ok(rec.request_id, u);
        break;
      }
      default:
        break;
    }
  }
});
