import assert from 'node:assert/strict';
import { test } from 'node:test';
import { PerfRecorder, SEND_WINDOW_MS, summariseSend } from './marks.ts';

function rig() {
  let t = 0;
  const frames: (() => void)[] = [];
  const r = new PerfRecorder(
    () => t,
    (fn) => frames.push(fn),
  );
  return {
    r,
    at: (ms: number) => {
      t = ms;
    },
    paint: () => frames.splice(0).forEach((f) => f()),
  };
}

test('a send is timed from the click to the row and to the first words painted', () => {
  const { r, at, paint } = rig();
  at(100);
  r.sendIntent();
  at(150);
  r.userRow('c1');
  at(170);
  paint();
  at(400);
  r.tokenArrived('c1');
  at(450);
  r.tokenShown();
  at(480);
  paint();
  const tl = r.sends[0]!;
  assert.deepEqual(tl, { chatId: 'c1', send: 100, rowArrived: 150, rowPainted: 170, tokenArrived: 400, tokenShown: 450, tokenPainted: 480 });
  assert.deepEqual(summariseSend(tl), { rowMs: 70, firstWordMs: 380 });
  assert.deepEqual(r.last(), { rowMs: 70, firstWordMs: 380 });
});

test('a row long after the last send press is not credited to it', () => {
  const { r, at, paint } = rig();
  at(0);
  r.sendIntent();
  at(SEND_WINDOW_MS + 1);
  r.userRow('c1');
  paint();
  assert.equal(r.sends[0]!.send, null);
  // Timed from the row instead.
  assert.equal(r.last()!.rowMs, 0);
});

test('a press is used once', () => {
  const { r, at } = rig();
  at(0);
  r.sendIntent();
  at(10);
  r.userRow('c1');
  at(20);
  r.userRow('c1');
  assert.equal(r.sends[0]!.send, 0);
  assert.equal(r.sends[1]!.send, null);
});

test('only the first text of the newest send in the same chat counts', () => {
  const { r, at, paint } = rig();
  at(0);
  r.userRow('c1');
  at(5);
  r.tokenShown(); // nothing arrived yet: ignored
  at(10);
  r.tokenArrived('other');
  at(20);
  r.tokenArrived('c1');
  at(30);
  r.tokenArrived('c1');
  at(40);
  r.tokenShown();
  at(50);
  r.tokenShown();
  paint();
  const tl = r.sends[0]!;
  assert.equal(tl.tokenArrived, 20);
  assert.equal(tl.tokenShown, 40);
  assert.equal(tl.tokenPainted, 50);
});

test('marks and sends are bounded', () => {
  const { r } = rig();
  for (let i = 0; i < 500; i++) r.mark('x');
  for (let i = 0; i < 50; i++) r.userRow('c');
  assert.equal(r.marks.length <= 200, true);
  assert.equal(r.sends.length, 20);
});
