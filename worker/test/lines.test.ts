import assert from "node:assert/strict";
import { test } from "node:test";
import { encodeRecord, LineReader, MAX_RECORD_BYTES, type LineFault } from "../src/protocol/lines.ts";

function collect(limit?: number) {
  const records: unknown[] = [];
  const faults: LineFault[] = [];
  let ended = false;
  const r = new LineReader({ onRecord: (x) => records.push(x), onFault: (f) => faults.push(f), onEnd: () => (ended = true) }, limit);
  return { r, records, faults, ended: () => ended };
}

test("the record limit is 96 MiB", () => {
  assert.equal(MAX_RECORD_BYTES, 96 * 1024 * 1024);
});

test("records split across chunks are reassembled; CRLF and blank lines tolerated", () => {
  const c = collect();
  c.r.push('{"type":"a"');
  c.r.push(',"n":1}\n\n{"type":"b"}\r\n{"ty');
  c.r.push('pe":"c"}\n');
  c.r.end();
  assert.deepEqual(c.records, [{ type: "a", n: 1 }, { type: "b" }, { type: "c" }]);
  assert.deepEqual(c.faults, []);
  assert.equal(c.ended(), true);
});

test("a line over the limit is 'too long', even before its newline arrives", () => {
  const c = collect(10);
  c.r.push('{"type":"aaaa');
  c.r.push('aaaaaaaa"}');
  assert.equal(c.faults.length, 1);
  assert.equal(c.faults[0].kind, "too_long");
  c.r.push('\n{"type":"b"}\n');
  assert.deepEqual(c.records, [], "nothing is delivered after a fault");
});

test("a line exactly at the limit is accepted; one byte more is too long", () => {
  const exact = '{"type":"x1"}'; // 13 bytes
  const a = collect(13);
  a.r.push(exact + "\n");
  assert.deepEqual(a.faults, []);
  assert.equal(a.records.length, 1);
  const b = collect(12);
  b.r.push(exact + "\n");
  assert.equal(b.faults[0]?.kind, "too_long");
});

test("a stream ending mid-line is 'truncated', not 'too long'", () => {
  const c = collect();
  c.r.push('{"type":"a"}\n{"type":');
  c.r.end();
  assert.equal(c.records.length, 1);
  assert.equal(c.faults.length, 1);
  assert.equal(c.faults[0].kind, "truncated");
  assert.equal(c.ended(), false);
});

test("non-JSON and non-object lines are faults", () => {
  for (const bad of ["not json", "[1,2]", "42", "null"]) {
    const c = collect();
    c.r.push(bad + "\n");
    assert.equal(c.faults[0]?.kind, "bad_json", bad);
  }
});

test("encodeRecord refuses an oversize record", () => {
  assert.equal(encodeRecord({ type: "x" }), '{"type":"x"}\n');
  assert.throws(() => encodeRecord({ type: "x", pad: "y".repeat(50) }, 40), RangeError);
});
