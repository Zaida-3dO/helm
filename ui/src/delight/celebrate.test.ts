import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MAX_AGE_MS, MIN_GAP_MS, createCelebrator, type Deps, type Effect, type Rect } from './celebrate.ts';
import { INTRO_MS, INTRO_STILL_MS, introKey, introSeen, markIntroSeen, whenVisible } from './intro.ts';

const ROW: Rect = { left: 8, top: 100, width: 240, height: 28 };

/** A celebrator on a clock the test moves; `later` queues until `advance` reaches it. */
function rig(over: Partial<Deps> = {}) {
  const drawn: { at: number; effect: Effect }[] = [];
  const queue: { at: number; fn: () => void }[] = [];
  const clock = { now: 10_000 };
  const deps: Deps = {
    flair: () => 'standard',
    mayMove: () => true,
    hasFocus: () => true,
    now: () => clock.now,
    rowRect: () => ROW,
    draw: (effect) => drawn.push({ at: clock.now, effect }),
    later: (fn, ms) => queue.push({ at: clock.now + ms, fn }),
    ...over,
  };
  const advance = (to: number) => {
    for (;;) {
      queue.sort((a, b) => a.at - b.at);
      const next = queue[0];
      if (!next || next.at > to) break;
      queue.shift();
      clock.now = next.at;
      next.fn();
    }
    clock.now = to;
  };
  return { c: createCelebrator(deps), drawn, queue, clock, advance };
}

test('a finished chat gets a ring only when it may move, and only if the row is showing', () => {
  const cases: { name: string; over: Partial<Deps>; drawn: number; spray?: boolean }[] = [
    { name: 'standard: one ring', over: {}, drawn: 1, spray: false },
    { name: 'maximal adds the spray', over: { flair: () => 'maximal' }, drawn: 1, spray: true },
    { name: 'quiet: nothing', over: { flair: () => 'quiet' }, drawn: 0 },
    { name: 'reduced motion or slow display: nothing', over: { mayMove: () => false }, drawn: 0 },
    { name: 'window not focused: nothing', over: { hasFocus: () => false }, drawn: 0 },
    { name: 'row not on screen: nothing', over: { rowRect: () => null }, drawn: 0 },
  ];
  for (const t of cases) {
    const r = rig(t.over);
    r.c.completion({ chatId: 'a', completedAt: r.clock.now });
    assert.equal(r.drawn.length, t.drawn, t.name);
    assert.equal(r.queue.length, 0, t.name);
    if (t.drawn) {
      assert.equal(r.drawn[0].effect.spray, t.spray, t.name);
      assert.deepEqual(r.drawn[0].effect.rect, ROW, t.name);
      assert.equal(r.drawn[0].effect.check, false, t.name);
    }
  }
});

test('celebrations are spaced 180 ms apart and any that would start too late are dropped', () => {
  const r = rig();
  // Ten chats finish at the same moment: the first at once, then one every 180 ms while it is
  // still within 1.2 s of the finish (0, 180 ... 1080 is seven), and the other three are dropped.
  for (let i = 0; i < 10; i++) r.c.completion({ chatId: `c${i}`, completedAt: r.clock.now });
  assert.equal(r.drawn.length, 1);
  r.advance(r.clock.now + 5_000);
  assert.deepEqual(
    r.drawn.map((d) => d.at - 10_000),
    [0, 1, 2, 3, 4, 5, 6].map((n) => n * MIN_GAP_MS),
  );
  // A completion that is already older than 1.2 s is not worth drawing.
  const old = rig();
  old.c.completion({ chatId: 'x', completedAt: old.clock.now - MAX_AGE_MS - 1 });
  assert.equal(old.drawn.length, 0);
  assert.equal(old.queue.length, 0);
  // One without a time counts as just now.
  const undated = rig();
  undated.c.completion({ chatId: 'x' });
  assert.equal(undated.drawn.length, 1);
  // A turn that waited is re-checked when it comes: the window lost focus in between.
  let focused = true;
  const lost = rig({ hasFocus: () => focused });
  lost.c.completion({ chatId: 'a', completedAt: lost.clock.now });
  lost.c.completion({ chatId: 'b', completedAt: lost.clock.now });
  focused = false;
  lost.advance(lost.clock.now + 1000);
  assert.equal(lost.drawn.length, 1);
});

test('a merge rings the box and pops a check, unless things are quiet', () => {
  const box: Rect = { left: 300, top: 400, width: 500, height: 120 };
  for (const [over, drawn] of [
    [{}, 1],
    [{ flair: () => 'maximal' as const }, 1],
    [{ flair: () => 'quiet' as const }, 0],
    [{ mayMove: () => false }, 0],
    [{ hasFocus: () => false }, 0],
  ] as [Partial<Deps>, number][]) {
    const r = rig(over);
    r.c.merged(box);
    assert.equal(r.drawn.length, drawn);
    if (drawn) assert.deepEqual([r.drawn[0].effect.check, r.drawn[0].effect.rect], [true, box]);
  }
  const none = rig();
  none.c.merged(null);
  assert.equal(none.drawn.length, 0);
});

test('the intro is remembered per profile, waits for a visible window and skips when storage fails', () => {
  assert.equal(introKey('default'), 'helm.introSeen.default');
  assert.equal(introKey(''), 'helm.introSeen.default');
  assert.equal(introKey('p-1'), 'helm.introSeen.p-1');
  assert.ok(INTRO_MS === 2800 && INTRO_STILL_MS === 1200);

  const data = new Map<string, string>();
  const store = { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => void data.set(k, v) };
  assert.equal(introSeen(store, introKey('a')), false);
  markIntroSeen(store, introKey('a'));
  assert.equal(introSeen(store, introKey('a')), true);
  assert.equal(introSeen(store, introKey('b')), false, 'another profile still sees it');
  const broken = {
    getItem: () => {
      throw new Error('denied');
    },
    setItem: () => {
      throw new Error('denied');
    },
  };
  assert.equal(introSeen(broken, 'k'), true);
  assert.doesNotThrow(() => markIntroSeen(broken, 'k'));
  assert.equal(introSeen(null, 'k'), true);

  // Hidden now: waits for the page to become visible, and can be cancelled.
  const handlers: (() => void)[] = [];
  const doc = {
    visibilityState: 'hidden' as DocumentVisibilityState,
    addEventListener: (_k: string, h: () => void) => void handlers.push(h),
    removeEventListener: (_k: string, h: () => void) => void handlers.splice(handlers.indexOf(h), 1),
  };
  let shown = 0;
  whenVisible(doc as unknown as Document, () => shown++);
  handlers.forEach((h) => h());
  assert.equal(shown, 0);
  doc.visibilityState = 'visible';
  handlers.forEach((h) => h());
  assert.deepEqual([shown, handlers.length], [1, 0]);
  let late = 0;
  const cancel = whenVisible({ ...doc, visibilityState: 'hidden' } as unknown as Document, () => late++);
  cancel();
  assert.equal(late, 0);
  whenVisible(doc as unknown as Document, () => late++);
  assert.equal(late, 1, 'visible already: at once');
});
