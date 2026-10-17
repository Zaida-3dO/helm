// Entry point: read one autocomplete job (JSON) from stdin, write one JSON answer, exit.

import { complete, type CompleteJob } from "./complete.ts";

const MAX_JOB_BYTES = 64 * 1024;

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
  let job: CompleteJob;
  try {
    job = JSON.parse(input) as CompleteJob;
    if (typeof job.draft !== "string") throw new Error("bad job");
  } catch {
    process.stdout.write(JSON.stringify({ error: "the job is not valid JSON" }) + "\n");
    process.exit(2);
  }
  void complete(job).then((r) => {
    process.stdout.write(JSON.stringify(r) + "\n");
    process.exit(0);
  });
});
