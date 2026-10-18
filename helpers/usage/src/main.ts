// Entry point: read one usage job (JSON) from stdin, write one JSON answer to stdout, exit.

import { readControlUsage, type ControlJob } from "./control.ts";
import { countModes, type ModesJob } from "./modes.ts";

const MAX_JOB_BYTES = 64 * 1024;

interface Job extends ControlJob, Partial<ModesJob> {
  kind?: string;
}

let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d: string) => {
  input += d;
  if (input.length > MAX_JOB_BYTES) {
    process.stdout.write(JSON.stringify({ ok: false, reason: "error", message: "job too large" }) + "\n");
    process.exit(2);
  }
});
process.stdin.on("end", () => {
  let job: Job;
  try {
    job = JSON.parse(input) as Job;
  } catch {
    process.stdout.write(JSON.stringify({ ok: false, reason: "error", message: "the job is not valid JSON" }) + "\n");
    process.exit(2);
  }
  if (job.kind === "modes") {
    try {
      const j = job as ModesJob;
      if (!j.projectsDir || !Array.isArray(j.sessions) || typeof j.since !== "number" || typeof j.until !== "number") throw new Error("bad modes job");
      process.stdout.write(JSON.stringify({ ok: true, modes: countModes(j) }) + "\n");
      process.exit(0);
    } catch (e) {
      process.stdout.write(JSON.stringify({ ok: false, reason: "error", message: (e as Error).message }) + "\n");
      process.exit(2);
    }
  }
  if (job.kind !== "plan") {
    process.stdout.write(JSON.stringify({ ok: false, reason: "error", message: `unknown job kind: ${String(job.kind)}` }) + "\n");
    process.exit(2);
  }
  void readControlUsage(job).then((r) => {
    process.stdout.write(JSON.stringify(r) + "\n");
    process.exit(0);
  });
});
