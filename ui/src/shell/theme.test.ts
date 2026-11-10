import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyTheme, DOPAMINE_DEFAULT, FONT_DEFAULTS, parseDopamine, SKINS, applyZoom, fontVars, parseFontSizes, parseSkin, parseZoom } from './theme.ts';
import { motionOptions, sizeOptions, skinOptions, volumeOptions, zoomOptions } from '../settings/generalOptions.ts';
import { connectMotion, flair, mayMove, onMotionChange } from './motion.ts';

test('stored text sizes are kept only when whole and in range; the rest fall back to the defaults', () => {
  assert.deepEqual(parseFontSizes(undefined), FONT_DEFAULTS);
  assert.deepEqual(parseFontSizes({ interface: 15, response: 16, review: 12 }), { interface: 15, response: 16, review: 12 });
  assert.deepEqual(parseFontSizes({ interface: 30, response: 13.5, review: '12' }), FONT_DEFAULTS);
  assert.deepEqual(parseFontSizes({ interface: 11 }), { ...FONT_DEFAULTS, interface: 11 });
  // The host and the settings tool accept up to 20 / 24 / 20: a size set there is shown, not reset.
  assert.deepEqual(parseFontSizes({ interface: 20, response: 24, review: 20 }), { interface: 20, response: 24, review: 20 });
  assert.deepEqual(parseFontSizes({ interface: 21, response: 25, review: 21 }), FONT_DEFAULTS);
});

test('zoom is 50-200 in steps of 10, else 100', () => {
  assert.equal(parseZoom(120), 120);
  assert.equal(parseZoom(50), 50);
  assert.equal(parseZoom(200), 200);
  assert.equal(parseZoom(125), 100);
  assert.equal(parseZoom(40), 100);
  assert.equal(parseZoom('120'), 100);
});

test('the sizes set the interface, reply and review variables, with the review line height at +11', () => {
  assert.deepEqual(fontVars({ interface: 15, response: 16, review: 12 }), {
    '--interface-font-size': '15px',
    '--response-font-size': '16px',
    '--review-font-size': '12px',
    '--review-line-height': '23px',
  });
});

test('the General pane offers every size in range and every zoom step, defaults marked', () => {
  const iface = sizeOptions('interface');
  assert.equal(iface[0]?.value, '11');
  assert.equal(iface[iface.length - 1]?.value, '20');
  assert.equal(sizeOptions('response').at(-1)?.value, '24');
  assert.equal(iface.find((o) => o.value === '13')?.title, '13 px (default)');
  const z = zoomOptions();
  assert.equal(z.length, 16);
  assert.equal(z.find((o) => o.value === '100')?.title, '100% (default)');
  const vol = volumeOptions();
  assert.equal(vol[0]?.title, 'Silent');
  assert.equal(vol.find((o) => o.value === '45')?.title, '45% (default)');
  assert.equal(vol.at(-1)?.value, '100');
});

test('the skin keeps only names styles.css carries; anything else reads as the default', () => {
  for (const s of SKINS) assert.equal(parseSkin(s), s);
  assert.equal(parseSkin('windows-xp'), 'retro-desktop');
  assert.equal(parseSkin('wikipedia'), 'encyclopedia');
  assert.equal(parseSkin('constructor'), 'default');
  for (const bad of [undefined, null, '', 'dracula', 'team-chat ', 'DEFAULT', 7]) assert.equal(parseSkin(bad), 'default');
  // The General picker offers exactly those skins, in that order, the default marked.
  const opts = skinOptions();
  assert.deepEqual(opts.map((o) => o.value), [...SKINS]);
  assert.equal(opts[0]?.title, 'Default (no skin)');
  assert.equal(opts.find((o) => o.value === 'team-chat')?.title, 'Team chat');
});

test('the delight level keeps the three names, plain or in a document, and everything else reads as standard', () => {
  for (const l of ['zen', 'standard', 'brainrot']) assert.equal(parseDopamine(l), l);
  assert.equal(parseDopamine({ mode: 'zen' }), 'zen');
  // A custom set has no control editor yet, so it sits at the standard amount.
  assert.equal(parseDopamine({ mode: 'custom', version: 1 }), 'standard');
  for (const bad of [undefined, null, 'calm', 7, {}, { mode: 5 }, []]) assert.equal(parseDopamine(bad), DOPAMINE_DEFAULT);
  const opts = motionOptions();
  assert.deepEqual(opts.map((o) => o.value), ['zen', 'standard', 'brainrot']);
  assert.equal(opts.find((o) => o.value === 'standard')?.title, 'Standard (default)');
});

test('the theme write carries the level, and Zen also asks for the quiet effect set', () => {
  const g = globalThis as { window?: unknown };
  g.window = { matchMedia: () => ({ matches: true, addEventListener: () => {}, removeEventListener: () => {} }) };
  try {
    const root = { dataset: {} as Record<string, string> } as unknown as HTMLElement;
    const stop = applyTheme(root, 'system', 'default', 'zen');
    assert.equal(root.dataset.theme, 'dark');
    assert.equal(root.dataset.dopamine, 'zen');
    assert.equal(root.dataset.effects, 'quiet');
    stop();
    applyTheme(root, 'light', 'team-chat', 'brainrot');
    assert.equal(root.dataset.dopamine, 'brainrot');
    assert.equal(root.dataset.effects, 'full');
    applyTheme(root, 'light', 'team-chat');
    assert.equal(root.dataset.dopamine, 'standard');
    // The colours hook in after every write, the first one included.
    let applied = 0;
    applyTheme(root, 'dark', 'default', 'standard', () => applied++);
    assert.equal(applied, 1);
    assert.equal(root.dataset.theme, 'dark');
  } finally {
    delete g.window;
  }
});

test('zoom goes to the host web view and leaves no CSS zoom on the page', () => {
  const calls: unknown[] = [];
  const api = (op: string, args?: unknown) => {
    calls.push([op, args]);
    return Promise.resolve(null as never);
  };
  const root = { style: { zoom: '1.2' } } as unknown as HTMLElement;
  applyZoom(api, root, 120);
  assert.deepEqual(calls, [['window_zoom', { level: 120 }]]);
  assert.equal(root.style.zoom, '');
});

test('mayMove says no under reduced motion, Zen or the slow-display mode, and while scrolling when pausing is on', () => {
  const root = { dataset: {} as Record<string, string> } as unknown as HTMLElement;
  let reduced = false;
  const timers: (() => void)[] = [];
  let onScroll: (() => void) | undefined;
  let onPref: ((e: Record<string, unknown>) => void) | undefined;
  const win = {
    matchMedia: () => ({ matches: reduced, addEventListener: () => undefined, removeEventListener: () => undefined }),
    addEventListener: (k: string, h: () => void) => {
      if (k === 'scroll') onScroll = h;
    },
    removeEventListener: () => undefined,
    setTimeout: (h: () => void) => timers.push(h),
    clearTimeout: () => undefined,
  };
  const bus = {
    on: (k: string, h: (e: Record<string, unknown>) => void) => {
      if (k === 'preferences') onPref = h;
      return () => undefined;
    },
  };
  const stop = connectMotion(() => Promise.resolve(null as never), bus, root, win);
  let changes = 0;
  const off = onMotionChange(() => changes++);
  assert.equal(mayMove(), true);
  assert.equal(flair(), 'standard');
  for (const [level, want, may] of [['zen', 'quiet', false], ['brainrot', 'maximal', true], ['standard', 'standard', true]] as const) {
    root.dataset.dopamine = level;
    assert.deepEqual([flair(), mayMove()], [want, may], level);
  }
  root.dataset.motion = 'low';
  assert.equal(mayMove(), false, 'the slow-display mode');
  delete root.dataset.motion;
  reduced = true;
  assert.equal(mayMove(), false, 'the operating system asks for less motion');
  reduced = false;
  // Scrolling only matters once "Pause animations while scrolling" is on.
  onScroll!();
  assert.equal(mayMove(), true);
  onPref!({ changed: { pauseAnimationsOnScroll: true } });
  onScroll!();
  assert.equal(mayMove(), false);
  assert.equal(root.dataset.scrolling, '1');
  assert.ok(changes > 0, 'listeners hear the change');
  timers.at(-1)!();
  assert.equal(mayMove(), true, 'the page has stopped scrolling');
  assert.equal(root.dataset.scrolling, undefined);
  off();
  stop();
  assert.equal(mayMove(), true);
});
