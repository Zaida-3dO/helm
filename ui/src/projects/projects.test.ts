import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { EMOJI, firstEmoji, searchEmoji } from './emoji.ts';
import { ACCEPTED_TYPES, IMAGE_MAX_BYTES, base64Bytes, iconChoice, imageProblem, libName, squareCrop } from './iconImage.ts';

const LIBRARY_NAMES = [
  'anchor', 'compass', 'book', 'briefcase', 'code', 'terminal', 'bug', 'flask', 'leaf', 'house', 'car', 'money',
  'chart', 'camera', 'music', 'heart', 'star', 'globe', 'map', 'mail', 'chat', 'calendar', 'clock', 'gift',
  'cart', 'tool', 'gear', 'shield', 'key', 'lock', 'cloud', 'server', 'database', 'phone', 'laptop', 'game',
  'paint', 'pen', 'puzzle', 'rocket', 'school', 'baby', 'dog', 'plant', 'sun', 'moon', 'flag', 'sparkle',
];

test('the icon library holds the 48 names, once each and in order', () => {
  // The component file is read as text: it is a React file, which node:test cannot load.
  const src = readFileSync(new URL('./iconLibrary.tsx', import.meta.url), 'utf8');
  const found = [...src.matchAll(/name: '([^']+)'/g)].map((m) => m[1]);
  assert.equal(LIBRARY_NAMES.length, 48);
  assert.deepEqual(found, LIBRARY_NAMES);
  assert.equal(new Set(found).size, 48);
});

test('iconChoice and libName tell the three kinds of icon apart', () => {
  const rows: [string | null | undefined, string, string | null][] = [
    [null, 'default', null],
    [undefined, 'default', null],
    ['', 'default', null],
    ['img', 'img', null],
    ['lib:anchor', 'lib', 'anchor'],
    ['lib:', 'default', null],
    ['📁', 'emoji', null],
    ['😀', 'emoji', null],
    ['folder', 'default', null],
  ];
  for (const [icon, choice, name] of rows) {
    assert.equal(iconChoice(icon), choice, `choice of ${String(icon)}`);
    assert.equal(libName(icon), name, `name of ${String(icon)}`);
  }
});

test('the emoji list is big and unique, and search finds by word', () => {
  assert.ok(EMOJI.length >= 300);
  assert.equal(new Set(EMOJI.map((e) => e.char)).size, EMOJI.length);
  for (const e of EMOJI) assert.equal(e.words, e.words.toLowerCase());
  const rows: [string, (list: string[]) => boolean][] = [
    ['', (l) => l.length === EMOJI.length],
    ['   ', (l) => l.length === EMOJI.length],
    ['ROCKET', (l) => l.includes('🚀')],
    ['heart blue', (l) => l.length === 1 && l[0] === '💙'],
    ['folder', (l) => l.includes('📁') && l.includes('📂')],
    ['zzzz-no-such-word', (l) => l.length === 0],
  ];
  for (const [query, ok] of rows) {
    assert.ok(ok(searchEmoji(EMOJI, query).map((e) => e.char)), `search "${query}"`);
  }
});

/** A woman and a laptop joined into one emoji with the zero-width joiner. */
const TECHNOLOGIST = String.fromCodePoint(0x1f469, 0x200d, 0x1f4bb);

test('firstEmoji takes the first single emoji out of typed text', () => {
  const rows: [string, string | null][] = [
    ['hello', null],
    ['', null],
    ['   ', null],
    ['12345', null],
    ['😀 smile', '😀'],
    ['smile 😀', '😀'],
    [`${TECHNOLOGIST}x`, TECHNOLOGIST],
    ['🇬🇧', '🇬🇧'],
    ['🇬🇧🇫🇷', '🇬🇧'],
  ];
  for (const [text, want] of rows) assert.equal(firstEmoji(text), want, JSON.stringify(text));
});

test('squareCrop cuts the biggest centred square, zoomed and moved, inside the picture', () => {
  const rows: [number[], { sx: number; sy: number; side: number }][] = [
    [[400, 200], { sx: 100, sy: 0, side: 200 }],
    [[200, 400], { sx: 0, sy: 100, side: 200 }],
    [[300, 300], { sx: 0, sy: 0, side: 300 }],
    [[400, 200, 2], { sx: 150, sy: 50, side: 100 }],
    [[400, 200, 0.5], { sx: 100, sy: 0, side: 200 }],
    [[400, 200, 1, 0, 0], { sx: 0, sy: 0, side: 200 }],
    [[400, 200, 1, 1, 1], { sx: 200, sy: 0, side: 200 }],
    [[400, 200, 2, 1, 1], { sx: 300, sy: 100, side: 100 }],
    [[400, 200, 2, -3, 9], { sx: 0, sy: 100, side: 100 }],
    [[0, 0], { sx: 0, sy: 0, side: 0 }],
  ];
  for (const [args, want] of rows) {
    const [w, h, zoom, cx, cy] = args as [number, number, number?, number?, number?];
    assert.deepEqual(squareCrop(w, h, zoom, cx, cy), want, args.join(','));
  }
});

test('imageProblem refuses what cannot be an icon picture', () => {
  const rows: [{ type: string; size: number }, boolean][] = [
    [{ type: 'image/svg+xml', size: 100 }, true],
    [{ type: 'application/pdf', size: 100 }, true],
    [{ type: 'image/png', size: IMAGE_MAX_BYTES + 1 }, true],
    [{ type: 'image/png', size: 0 }, true],
    [{ type: 'image/png', size: 10 }, false],
    [{ type: 'image/jpeg', size: IMAGE_MAX_BYTES }, false],
    [{ type: 'image/webp', size: 1 }, false],
    [{ type: 'image/gif', size: 1 }, false],
  ];
  for (const [file, refused] of rows) {
    const problem = imageProblem(file);
    assert.equal(problem !== null, refused, `${file.type} ${file.size}`);
    if (problem) assert.ok(problem.length > 0 && !problem.includes('undefined'));
  }
  assert.equal(ACCEPTED_TYPES.length, 4);
});

test('base64Bytes gives the decoded size', () => {
  const rows: [string, number][] = [
    ['', 0],
    ['QQ==', 1],
    ['QUI=', 2],
    ['QUJD', 3],
    ['QUJDRA==', 4],
    ['QUJD\nRA==', 4],
  ];
  for (const [b64, want] of rows) assert.equal(base64Bytes(b64), want, JSON.stringify(b64));
});
