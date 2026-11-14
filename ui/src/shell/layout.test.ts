import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dragWidth, fitSidebar, MAIN_MIN, parseSidebarVisible, parseSidebarWidth, SIDEBAR_DEFAULT, SIDEBAR_MIN, sidebarLayout } from './layout.ts';

test('a stored width is used when it is a number the host allows; anything else is the default', () => {
  assert.equal(parseSidebarWidth(300), 300);
  assert.equal(parseSidebarWidth(300.6), 301);
  assert.equal(parseSidebarWidth(SIDEBAR_MIN), SIDEBAR_MIN);
  assert.equal(parseSidebarWidth(SIDEBAR_MIN - 1), SIDEBAR_DEFAULT);
  assert.equal(parseSidebarWidth(5000), SIDEBAR_DEFAULT);
  assert.equal(parseSidebarWidth('300'), SIDEBAR_DEFAULT);
  assert.equal(parseSidebarWidth(undefined), SIDEBAR_DEFAULT);
});

test('the sidebar is visible unless stored as exactly false', () => {
  assert.equal(parseSidebarVisible(false), false);
  assert.equal(parseSidebarVisible(true), true);
  assert.equal(parseSidebarVisible(undefined), true);
  assert.equal(parseSidebarVisible(0), true);
});

test('a drag clamps at the minimum and leaves the main pane at least 320 px', () => {
  // 480 wide with a 700 px main pane: the sidebar can grow by 380 at most.
  assert.equal(dragWidth(480, 100, 700), 580);
  assert.equal(dragWidth(480, 1000, 700), 480 + 700 - MAIN_MIN);
  assert.equal(dragWidth(480, -1000, 700), SIDEBAR_MIN);
  // A main pane already under the minimum never lets the sidebar grow, and never shrinks it by itself.
  assert.equal(dragWidth(480, 50, 300), 480);
  assert.equal(dragWidth(480, -30, 300), 450);
});

test('the sidebar gives way to the chat in a narrow frame and hides below 900 px', () => {
  const rows: [room: number | null, stored: number, peek: boolean, squeeze: boolean, width: number | 'hidden', fitted: string | null][] = [
    [null, 480, false, false, 480, null], // not measured yet: as stored
    [1440, 480, false, false, 480, null],
    [1040, 480, false, false, 480, null], // exactly leaves the chat 560
    [1000, 480, false, false, 440, 'shrunk'],
    [900, 480, false, false, 340, 'shrunk'],
    [899, 480, false, false, 'hidden', 'hidden'],
    [760, 480, true, false, SIDEBAR_MIN, 'shrunk'], // shown by its toggle: at least the minimum
    [1000, 300, false, false, 300, null], // a narrow stored width needs no fitting
    [1000, 600, false, true, 600, null], // dragged wider: the chat may drop to 320
    [800, 600, false, true, 'hidden', 'hidden'], // but a narrow window still hides it
    [900, 700, false, true, 900 - MAIN_MIN, 'shrunk'],
  ];
  for (const [room, stored, peek, squeeze, width, fitted] of rows) {
    const f = fitSidebar({ width: stored, visible: true }, { room, peek, squeeze });
    const label = `room ${room}, stored ${stored}, peek ${peek}, squeeze ${squeeze}`;
    assert.equal(f.visible ? f.width : 'hidden', width, label);
    assert.equal(f.fitted, fitted, label);
  }
  // Hidden by the user: no fitting, whatever the room.
  assert.deepEqual(fitSidebar({ width: 480, visible: false }, { room: 700, peek: false, squeeze: false }), { width: 480, visible: false, fitted: null });
});

test('the store fits what shows but saves only what the user chose', () => {
  const saved: [string, unknown][] = [];
  sidebarLayout.connect((k, v) => saved.push([k, v]));
  sidebarLayout.load(480, true);
  sidebarLayout.setRoom(1000);
  assert.deepEqual(sidebarLayout.get(), { width: 440, visible: true, fitted: 'shrunk' });
  assert.deepEqual(sidebarLayout.preferred(), { width: 480, visible: true });
  // Narrow: hidden by itself; the toggle peeks and hides again without saving anything.
  sidebarLayout.setRoom(760);
  assert.equal(sidebarLayout.get().visible, false);
  sidebarLayout.toggle();
  assert.deepEqual(sidebarLayout.get(), { width: SIDEBAR_MIN, visible: true, fitted: 'shrunk' });
  sidebarLayout.toggle();
  assert.equal(sidebarLayout.get().visible, false);
  sidebarLayout.toggle(); // peek again; widening ends the peek and the fit
  sidebarLayout.setRoom(1440);
  assert.deepEqual(sidebarLayout.get(), { width: 480, visible: true, fitted: null });
  sidebarLayout.setRoom(800);
  assert.equal(sidebarLayout.get().visible, false, 'a peek does not outlive a wide window');
  // A drag previews without saving; dropping it wider than the chat's 560 allows keeps it.
  sidebarLayout.setRoom(1000);
  sidebarLayout.preview(520);
  assert.equal(sidebarLayout.get().width, 520);
  sidebarLayout.preview(null);
  assert.equal(sidebarLayout.get().width, 440);
  sidebarLayout.setWidth(520, true);
  sidebarLayout.preview(null);
  assert.deepEqual(sidebarLayout.get(), { width: 520, visible: true, fitted: null });
  assert.deepEqual(saved, [['sidebarWidth', 520]]);
  // Hidden by the user in a wide window is saved; showing it from a narrow one is saved too.
  sidebarLayout.setVisible(false);
  sidebarLayout.setRoom(760);
  sidebarLayout.setVisible(true);
  assert.equal(sidebarLayout.get().visible, true);
  assert.deepEqual(saved.slice(1), [
    ['sidebarVisible', false],
    ['sidebarVisible', true],
  ]);
  sidebarLayout.setRoom(4096);
  sidebarLayout.connect(() => undefined);
});

test('the layout store saves width only when asked, and visibility on every change', () => {
  const saved: [string, unknown][] = [];
  sidebarLayout.connect((k, v) => saved.push([k, v]));
  sidebarLayout.load(320, true);
  assert.deepEqual(sidebarLayout.get(), { width: 320, visible: true, fitted: null });
  sidebarLayout.setWidth(330, false);
  sidebarLayout.setWidth(340, true);
  sidebarLayout.toggle();
  sidebarLayout.toggle();
  sidebarLayout.setVisible(true);
  assert.deepEqual(saved, [
    ['sidebarWidth', 340],
    ['sidebarVisible', false],
    ['sidebarVisible', true],
  ]);
  sidebarLayout.connect(() => undefined);
});
