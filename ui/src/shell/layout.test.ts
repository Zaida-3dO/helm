import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dragWidth, MAIN_MIN, parseSidebarVisible, parseSidebarWidth, SIDEBAR_DEFAULT, SIDEBAR_MIN, sidebarLayout } from './layout.ts';

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

test('the layout store saves width only when asked, and visibility on every change', () => {
  const saved: [string, unknown][] = [];
  sidebarLayout.connect((k, v) => saved.push([k, v]));
  sidebarLayout.load(320, true);
  assert.deepEqual(sidebarLayout.get(), { width: 320, visible: true });
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
