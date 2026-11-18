// Entry point: read one gatekeeper job (JSON) from stdin, write one JSON answer to stdout, exit.
// A job with `kind: "stop"` is the stop judge (a chat ended a clean turn and may be about to
// wait for the user); any other job is a question card.

import { gatekeeper, type GateJob } from "./gatekeeper.ts";
import { judgeStop, type StopJob } from "./stop.ts";

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
const answer = (r: unknown) => {
  process.stdout.write(JSON.stringify(r) + "\n");
  process.exit(0);
};

process.stdin.on("end", () => {
  let job: unknown;
  try {
    job = JSON.parse(input);
    if (!job || typeof job !== "object") throw new Error("bad job");
  } catch {
    process.stdout.write(JSON.stringify({ error: "the job is not valid JSON" }) + "\n");
    process.exit(2);
  }
  if ((job as StopJob).kind === "stop") {
    const stop = job as StopJob;
    if (!Array.isArray(stop.turns)) stop.turns = [];
    if (typeof stop.last !== "string") stop.last = "";
    void judgeStop(stop).then(answer);
    return;
  }
  const card = job as GateJob;
  if (!card.card || typeof card.card !== "object") {
    process.stdout.write(JSON.stringify({ error: "the job is not valid JSON" }) + "\n");
    process.exit(2);
  }
  if (!Array.isArray(card.card.options)) card.card.options = [];
  if (!Array.isArray(card.turns)) card.turns = [];
  void gatekeeper(card).then(answer);
});
