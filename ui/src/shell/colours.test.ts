import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  COLOURS_DEFAULT,
  COLOUR_PRESETS,
  COLOUR_VARS,
  applyColours,
  colourProfileKey,
  coloursFor,
  contrastRatio,
  isDefaultColours,
  mixRgb,
  oklabToRgb,
  oklchToRgb,
  parseColour,
  parseColours,
  readBase,
  rgbToOklab,
  solveColours,
  toHex,
  withColours,
  type ColourBase,
  type Colours,
  type Rgb,
} from './colours.ts';
import { SKINS } from './theme.ts';

// --- the skins' own colours, read from styles.css -------------------------------------------

function stripComments(s: string): string {
  let out = '';
  let i = 0;
  for (;;) {
    const a = s.indexOf('/*', i);
    if (a < 0) return out + s.slice(i);
    out += s.slice(i, a);
    const b = s.indexOf('*/', a + 2);
    if (b < 0) return out;
    i = b + 2;
  }
}

/** Selector (whitespace collapsed) to its custom properties, for every plain rule in the file. */
function readBlocks(css: string): Map<string, Map<string, string>> {
  const blocks = new Map<string, Map<string, string>>();
  for (const chunk of stripComments(css).split('}')) {
    const open = chunk.indexOf('{');
    if (open < 0) continue;
    const selector = chunk.slice(0, open).trim().replace(/\s+/g, ' ');
    const vars = blocks.get(selector) ?? new Map<string, string>();
    for (const decl of chunk.slice(open + 1).split(';')) {
      const at = decl.indexOf(':');
      const name = decl.slice(0, at).trim();
      if (at > 0 && name.startsWith('--')) vars.set(name, decl.slice(at + 1).trim());
    }
    if (!blocks.has(selector)) blocks.set(selector, vars);
  }
  return blocks;
}

const blocks = readBlocks(readFileSync(new URL('../styles.css', import.meta.url), 'utf8'));
const MODES = ['light', 'dark'] as const;

/** The skin's colours for a mode: the plain theme's values with the skin's own laid over them. */
function baseFor(skin: string, mode: 'light' | 'dark'): ColourBase {
  const plain = blocks.get(mode === 'light' ? ":root, :root[data-theme='light']" : ":root[data-theme='dark']");
  assert.ok(plain, `the plain ${mode} block`);
  const own = skin === 'default' ? undefined : blocks.get(`:root[data-skin='${skin}'][data-theme='${mode}']`);
  if (skin !== 'default') assert.ok(own, `${skin} ${mode} block`);
  const base = readBase((name) => own?.get(name) ?? plain.get(name) ?? '');
  assert.ok(base, `${skin} ${mode}: every colour the solver reads is a colour`);
  return base;
}

test('colour maths: the WCAG ratio, OKLab round trips and the colour text forms', () => {
  assert.ok(Math.abs(contrastRatio([0, 0, 0], [255, 255, 255]) - 21) < 1e-9);
  assert.equal(contrastRatio([128, 128, 128], [128, 128, 128]), 1);
  for (const c of [[255, 0, 0], [12, 200, 90], [251, 251, 250], [0, 0, 0]] as Rgb[]) assert.deepEqual(oklabToRgb(rgbToOklab(c)), c);
  const [L, a, b] = rgbToOklab([255, 0, 0]);
  assert.ok(Math.abs(L - 0.628) < 0.01 && Math.abs(Math.hypot(a, b) - 0.2577) < 0.01, 'red is where OKLab puts it');
  // Chroma is lowered, never wrapped, when a colour is outside the screen's range.
  const [r, g, bl] = oklchToRgb(0.62, 0.4, 145);
  assert.ok(g > r && g > bl, 'a hugely saturated green is still green');
  assert.deepEqual(mixRgb([0, 0, 0], [255, 255, 255], 0), [0, 0, 0]);
  assert.deepEqual(mixRgb([0, 0, 0], [255, 255, 255], 1), [255, 255, 255]);
  for (const [text, want] of [
    ['#fbfbfa', [251, 251, 250]],
    ['#FFF', [255, 255, 255]],
    ['#112233cc', [17, 34, 51]],
    ['rgb(10, 20, 30)', [10, 20, 30]],
    ['rgb(10 20 30 / 50%)', [10, 20, 30]],
    ['red', null],
    ['#12', null],
    ['#gggggg', null],
    ['', null],
  ] as [string, Rgb | null][]) assert.deepEqual(parseColour(text), want, text);
  assert.equal(toHex([1, 171, 255]), '#01abff');
});

test('stored colours: one entry per profile, read back safely, written without touching the others', () => {
  assert.deepEqual(parseColours(undefined), COLOURS_DEFAULT);
  assert.deepEqual(parseColours({ accentHue: 4, tint: 'soft', contrast: 'more' }), { accentHue: 4, tint: 'soft', contrast: 'more' });
  for (const bad of [{ accentHue: 360 }, { accentHue: -1 }, { accentHue: 4.5 }, { accentHue: '4' }, { tint: 'heavy' }, { contrast: 'high' }, 7, 'x', []]) {
    assert.deepEqual(parseColours(bad), COLOURS_DEFAULT, JSON.stringify(bad));
  }
  assert.deepEqual(parseColours({ accentHue: 0 }), { ...COLOURS_DEFAULT, accentHue: 0 });
  for (const [active, key] of [['p-1', 'p-1'], [null, 'default'], [undefined, 'default'], ['', 'default'], ['all', 'default'], ['x'.repeat(65), 'default'], [7, 'default']] as [unknown, string][]) {
    assert.equal(colourProfileKey(active), key, String(active));
  }
  const map = { default: { accentHue: 4, tint: 'none', contrast: 'standard' }, 'p-1': { accentHue: 235, tint: 'soft', contrast: 'standard' } };
  assert.equal(coloursFor(map, 'p-1').accentHue, 235);
  assert.deepEqual(coloursFor(map, 'p-2'), COLOURS_DEFAULT);
  assert.deepEqual(coloursFor(null, 'default'), COLOURS_DEFAULT);
  assert.deepEqual(coloursFor({ constructor: { accentHue: 1 } }, 'toString'), COLOURS_DEFAULT);
  const next = withColours(map, 'default', { accentHue: 285, tint: 'none', contrast: 'more' });
  assert.deepEqual(next, { default: { accentHue: 285, tint: 'none', contrast: 'more' }, 'p-1': map['p-1'] });
  // A profile back at the defaults leaves the map; the last one leaving clears the preference.
  assert.deepEqual(withColours(map, 'default', COLOURS_DEFAULT), { 'p-1': map['p-1'] });
  assert.equal(withColours({ default: map.default }, 'default', COLOURS_DEFAULT), null);
  assert.equal(withColours(null, 'default', COLOURS_DEFAULT), null);
  assert.ok(isDefaultColours(COLOURS_DEFAULT) && !isDefaultColours({ ...COLOURS_DEFAULT, contrast: 'more' }));
});

test('every preset, tint and contrast choice stays readable in light and dark on all nine skins', () => {
  assert.equal(COLOUR_PRESETS.length, 8);
  assert.deepEqual(
    COLOUR_PRESETS.map((p) => [p.id, p.hue]),
    [['red', 4], ['coral', 18], ['amber', 45], ['green', 145], ['teal', 185], ['blue', 235], ['violet', 285], ['pink', 340]],
  );
  assert.equal(SKINS.length, 9);
  let cases = 0;
  for (const skin of SKINS) {
    for (const mode of MODES) {
      const base = baseFor(skin, mode);
      for (const preset of COLOUR_PRESETS) {
        for (const tint of ['none', 'soft', 'strong'] as const) {
          for (const contrast of ['standard', 'more'] as const) {
            cases++;
            const c: Colours = { accentHue: preset.hue, tint, contrast };
            const label = `${skin} ${mode} ${preset.id} ${tint} ${contrast}`;
            const need = contrast === 'more' ? 7 : 4.5;
            const { vars } = solveColours(c, mode, base);
            const colour = (name: string, fallback: Rgb): Rgb => parseColour(vars[name] ?? '') ?? fallback;
            const pages = [colour('--bg', base.bg), colour('--sidebar', base.sidebar), colour('--surface', base.surface)];
            const origPages = [base.bg, base.sidebar, base.surface];
            const accent = colour('--accent', [0, 0, 0]);
            assert.notEqual(vars['--accent'], undefined, label);
            pages.forEach((bg, i) => {
              assert.ok(contrastRatio(accent, bg) >= need, `${label}: the accent reads on page ${i} (${contrastRatio(accent, bg).toFixed(2)})`);
              assert.ok(contrastRatio(base.text, bg) >= need, `${label}: the text reads on page ${i}`);
              assert.ok(contrastRatio(colour('--muted', base.muted), bg) >= Math.min(contrast === 'more' ? 7 : 4.5, contrastRatio(base.muted, origPages[i]!) - 0.05), `${label}: muted text keeps its contrast on page ${i}`);
            });
            assert.ok(contrastRatio(colour('--on-accent-fill', [0, 0, 0]), colour('--accent-fill', [0, 0, 0])) >= need, `${label}: text on the accent fill reads`);
            assert.ok(contrastRatio(base.onSelected, colour('--chat-selected-fill', base.selected)) >= Math.min(need, contrastRatio(base.onSelected, base.selected)), `${label}: the selected row's text reads`);
            // Nothing outside the cleared set is ever written.
            for (const name of Object.keys(vars)) assert.ok((COLOUR_VARS as readonly string[]).includes(name), `${label}: ${name}`);
          }
        }
      }
    }
  }
  assert.equal(cases, 9 * 2 * 8 * 3 * 2);
});

test('no hue changes nothing, more contrast alone lifts the quiet text, and the note says when the colour was adjusted', () => {
  const light = baseFor('default', 'light');
  const dark = baseFor('default', 'dark');
  assert.deepEqual(solveColours(COLOURS_DEFAULT, 'light', light), { vars: {}, adjusted: false });
  // No hue means no tint to wash with, and the accent stays the skin's own.
  assert.deepEqual(solveColours({ accentHue: null, tint: 'strong', contrast: 'standard' }, 'dark', dark), { vars: {}, adjusted: false });
  const more = solveColours({ accentHue: null, tint: 'none', contrast: 'more' }, 'light', light);
  assert.equal(more.vars['--accent'], undefined);
  assert.ok(contrastRatio(parseColour(more.vars['--muted'] ?? '')!, light.bg) >= 7, 'muted text reaches 7:1');
  assert.ok(contrastRatio(parseColour(more.vars['--secondary'] ?? '')!, light.surface) >= 7, 'secondary text reaches 7:1');
  // Red on the light default starts too pale for 4.5:1 and is darkened; the answer says so.
  const red = solveColours({ accentHue: 4, tint: 'none', contrast: 'standard' }, 'light', light);
  assert.equal(red.adjusted, true);
  // The same hue lands on a different lightness per mode: lighter to be read on the dark pages.
  const darkRed = solveColours({ accentHue: 4, tint: 'none', contrast: 'standard' }, 'dark', dark);
  assert.ok(rgbToOklab(parseColour(darkRed.vars['--accent']!)!)[0] > rgbToOklab(parseColour(red.vars['--accent']!)!)[0]);
  // A tint that would spoil the text is eased off rather than refused: on white pages with muted
  // text just past 4.5:1, the strong wash cannot stay, and what is left still reads.
  const white = parseColour('#ffffff')!;
  const edge: ColourBase = { ...light, bg: white, sidebar: white, surface: white, surfaceAlt: white, muted: parseColour('#767676')! };
  const eased = solveColours({ accentHue: 4, tint: 'strong', contrast: 'standard' }, 'light', edge);
  assert.equal(eased.adjusted, true);
  const page = parseColour(eased.vars['--bg'] ?? '#ffffff')!;
  assert.ok(contrastRatio(edge.muted, page) >= Math.min(4.5, contrastRatio(edge.muted, white) - 0.05));
});

test('applyColours writes the overrides on the root and a default clears every one of them', () => {
  const light = baseFor('default', 'light');
  const names: Record<string, string> = {
    '--bg': toHex(light.bg),
    '--sidebar': toHex(light.sidebar),
    '--surface': toHex(light.surface),
    '--surface-alt': toHex(light.surfaceAlt),
    '--hover': toHex(light.hover),
    '--selected': toHex(light.selected),
    '--text': toHex(light.text),
    '--secondary': toHex(light.secondary),
    '--muted': toHex(light.muted),
    '--on-selected': toHex(light.onSelected),
  };
  const set = new Map<string, string>();
  const root = { style: { setProperty: (k: string, v: string) => set.set(k, v), removeProperty: (k: string) => set.delete(k) } } as unknown as HTMLElement;
  const g = globalThis as { getComputedStyle?: unknown };
  g.getComputedStyle = () => ({ getPropertyValue: (n: string) => names[n] ?? '' });
  try {
    const r = applyColours(root, { accentHue: 4, tint: 'soft', contrast: 'standard' }, 'light');
    assert.equal(r.adjusted, true);
    assert.ok(set.has('--accent') && set.has('--accent-bg') && set.has('--bg'), 'accent, its wash and the tinted page are written');
    applyColours(root, { accentHue: 235, tint: 'none', contrast: 'standard' }, 'light');
    assert.ok(!set.has('--bg'), 'the tint from the earlier colours is gone');
    assert.equal(applyColours(root, COLOURS_DEFAULT, 'light').adjusted, false);
    assert.equal(set.size, 0, 'the skin shows through again');
    // A page the solver cannot read leaves the skin alone.
    names['--bg'] = 'var(--nope)';
    assert.equal(applyColours(root, { accentHue: 4, tint: 'none', contrast: 'standard' }, 'light').adjusted, false);
    assert.equal(set.size, 0);
  } finally {
    delete g.getComputedStyle;
  }
});
