import { test } from 'node:test';
import assert from 'node:assert/strict';
import { FONT_DEFAULTS, fontVars, parseFontSizes, parseZoom } from './theme.ts';
import { sizeOptions, zoomOptions } from '../settings/generalOptions.ts';

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
});
