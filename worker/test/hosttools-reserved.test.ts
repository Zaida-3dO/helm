// Drift test: the worker's own host-tool names are reserved in the host's file, and none is a
// name the host never registers.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { WORKER_SERVED } from "../src/hosttools/names.ts";

const file = fileURLToPath(new URL("../../host/crates/hosttools/reserved.json", import.meta.url));
const reserved = JSON.parse(readFileSync(file, "utf8")) as { names: string[]; dropped?: string[] };

test("every worker-served host tool is reserved and none is dropped", () => {
  for (const n of WORKER_SERVED) {
    assert.ok(reserved.names.includes(n), `${n} is missing from reserved.json`);
    assert.ok(!(reserved.dropped ?? []).includes(n), `${n} is listed as dropped`);
    assert.match(n, /^app_[a-z0-9_]{1,60}$/);
  }
});
