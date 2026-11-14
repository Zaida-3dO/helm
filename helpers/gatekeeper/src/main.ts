// Entry point: read one gatekeeper job (JSON) from stdin, write one JSON answer to stdout, exit.

import { gatekeeper, type GateJob } from "./gatekeeper.ts";

const MAX_JOB_BYTES = 128 * 1024;

let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d: string) => {
  input += d;
  if (input.length > MAX_JOB_BYTES) {
    process.stdout.write(JSON.stringify({ error: "job too large" }) + "\n");
    process.exit(2);
  }
});
process.stdin.on("end", () => {
  let job: GateJob;
  try {
    job = JSON.parse(input) as GateJob;
    if (!job || !job.card || typeof job.card !== "object") throw new Error("bad job");
    if (!Array.isArray(job.card.options)) job.card.options = [];
    if (!Array.isArray(job.turns)) job.turns = [];
  } catch {
    process.stdout.write(JSON.stringify({ error: "the job is not valid JSON" }) + "\n");
    process.exit(2);
  }
  void gatekeeper(job).then((r) => {
    process.stdout.write(JSON.stringify(r) + "\n");
    process.exit(0);
  });
});
