// Entry point: read one summarize job (JSON) from stdin, write one JSON answer to stdout, exit.

import { makeSummary, type SummarizeJob } from "./summarize.ts";

const MAX_JOB_BYTES = 256 * 1024;

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
  let job: SummarizeJob;
  try {
    job = JSON.parse(input) as SummarizeJob;
    if (!Array.isArray(job.messages)) throw new Error("bad job");
  } catch {
    process.stdout.write(JSON.stringify({ error: "the job is not valid JSON" }) + "\n");
    process.exit(2);
  }
  void makeSummary(job).then((r) => {
    process.stdout.write(JSON.stringify(r) + "\n");
    process.exit(0);
  });
});
