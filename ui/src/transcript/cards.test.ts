import { test } from 'node:test';
import assert from 'node:assert/strict';
import { autoOpenPicks, boardRef, cardKind, chosenText, forwardRef, galleryRef, optionForKey, optionMessage, panelRef } from './cards.ts';
import { stepImage } from './messageImages.ts';
import { swipeStep, wheelStep, WHEEL_COOLDOWN_MS } from './swipe.ts';
import { row } from '../dev/fixtures.ts';

const tool = (name: string, text: string, extra: Partial<Parameters<typeof row>[0]> = {}) => row({ role: 'toolResult', toolName: `mcp__helm__${name}`, text, ...extra });

test('a card exists only for a finished, well-formed call of its tool', () => {
  const gallery = tool('app_show_images', '{"galleryId":"g1","count":3}');
  const board = tool('app_offer_options', '{"boardId":"b1","next":"wait"}');
  assert.deepEqual(galleryRef(gallery), { galleryId: 'g1', count: 3 });
  assert.deepEqual(boardRef(board), { boardId: 'b1' });
  assert.deepEqual([cardKind(gallery), cardKind(board)], ['gallery', 'board']);
  const plain = [
    tool('app_show_images', '{"galleryId":"g1","count":3}', { status: 'streaming' }),
    tool('app_show_images', 'Refused: no pictures', { status: 'error' }),
    tool('app_show_images', '{"count":3}'),
    tool('app_offer_options', 'not json'),
    tool('app_list_chats', '{"galleryId":"g1"}'),
    row({ role: 'toolResult', toolName: 'Bash', text: '{"galleryId":"g1"}' }),
    row({ role: 'assistant', text: '1. Red\n2. Blue' }),
  ];
  assert.deepEqual(plain.map(cardKind), plain.map(() => null));
});

test('a choice goes out as "<question>: <choice>", with a picked tile\'s detail, and number keys pick tiles', () => {
  const board = {
    question: ' Which colour? ',
    options: [
      { index: 0, label: 'Red', detail: 'warm', hasImage: false },
      { index: 1, label: 'Blue', detail: null, hasImage: true },
    ],
  };
  assert.equal(optionMessage(board, ['Red']), 'Which colour?: Red (warm)');
  assert.equal(optionMessage(board, ['Blue', 'Red']), 'Which colour?: Blue, Red (warm)');
  assert.equal(optionMessage(board, [], ' teal, please '), 'Which colour?: teal, please');
  assert.equal(chosenText({ labels: ['Red', 'Blue'], other: null, answeredAt: 1 }), 'Red, Blue');
  assert.equal(chosenText({ labels: [], other: 'teal', answeredAt: 1 }), 'Other: teal');
  assert.deepEqual(['1', '2', '3', '0', 'a', '10'].map((k) => optionForKey(k, 2)), [0, 1, null, null, null, null]);
});

test('a drag or a touchpad scroll steps once, and mostly-vertical movement does not', () => {
  assert.deepEqual([-80, 80, -30, 80, -80].map((dx, i) => swipeStep(dx, [0, 10, 0, 0, 90][i]!)), [1, -1, 0, -1, 0]);
  assert.equal(wheelStep(60, 4, 1000, -Infinity), 1);
  assert.equal(wheelStep(-60, 4, 1000, -Infinity), -1);
  assert.equal(wheelStep(8, 0, 1000, -Infinity), 0);
  assert.equal(wheelStep(4, 60, 1000, -Infinity), 0);
  // One flick is one step: the rest of its scroll events arrive inside the cool-down.
  assert.equal(wheelStep(60, 0, 1000 + WHEEL_COOLDOWN_MS - 1, 1000), 0);
  assert.equal(wheelStep(60, 0, 1000 + WHEEL_COOLDOWN_MS, 1000), 1);
  assert.deepEqual([stepImage(0, 3, -1), stepImage(2, 3, 1)], [2, 0]);
});

test('panel and forward results are read from their JSON, and anything odd is no card', () => {
  const file = tool('app_open_panel', JSON.stringify({ opened: { kind: 'file', path: 'C:/work/site/src/app.ts', line: 12 }, link: 'helm://files/x', note: 'shown' }));
  assert.deepEqual(panelRef(file), { kind: 'file', target: { kind: 'helm', href: 'helm://files/C%3A%2Fwork%2Fsite%2Fsrc%2Fapp.ts:12' }, label: 'app.ts' });
  const page = tool('app_open_panel', JSON.stringify({ opened: { kind: 'page', pageId: 'ab12', path: 'C:/p/x.html', title: 'Progress' } }));
  assert.deepEqual(panelRef(page), { kind: 'page', target: { kind: 'helm', href: 'helm://pages/ab12' }, label: 'Progress' });
  const forward = tool('app_forward_port', JSON.stringify({ forward: { port: 5173, title: 'Site preview' }, typedOn: 'Laptop', samePc: false, note: 'n' }));
  assert.deepEqual(forwardRef(forward), { port: 5173, title: 'Site preview', samePc: false, typedOn: 'Laptop' });
  assert.deepEqual(forwardRef(tool('app_forward_port', '{"forward":{"port":3000},"typedOn":null,"samePc":true}')), { port: 3000, title: '', samePc: true, typedOn: null });
  const junk = [
    tool('app_open_panel', JSON.stringify({ opened: { kind: 'file', path: String.raw`\\host\share\x.ts` } })),
    tool('app_open_panel', JSON.stringify({ opened: { kind: 'file', path: 'C:x.ts' } })),
    tool('app_open_panel', JSON.stringify({ opened: { kind: 'file', path: `C:/a${String.fromCharCode(0)}b` } })),
    tool('app_open_panel', JSON.stringify({ opened: { kind: 'page', pageId: '../x' } })),
    tool('app_open_panel', JSON.stringify({ opened: { kind: 'chat', chatId: 'c2' } })),
    tool('app_open_panel', JSON.stringify({ link: 'helm://pages/ab12' })),
    tool('app_open_panel', 'Refused: that file is outside the project'),
    tool('app_open_panel', JSON.stringify({ opened: { kind: 'file', path: 'C:/a.ts' } }), { status: 'streaming' }),
    tool('app_list_pages', JSON.stringify({ opened: { kind: 'page', pageId: 'ab12' } })),
    tool('app_forward_port', '{"forward":{"port":0}}'),
    tool('app_forward_port', '{"forward":{"port":"5173"}}'),
    tool('app_forward_port', '{"forward":{"port":70000}}'),
    tool('app_forward_port', '{"port":5173}'),
    row({ role: 'toolResult', toolName: 'Bash', text: '{"forward":{"port":5173}}' }),
  ];
  assert.deepEqual(junk.map((r) => panelRef(r) ?? forwardRef(r)), junk.map(() => null));
});

test('only rows newer than the last full read act, once each, one panel and one forward per batch', () => {
  const open = (id: number, path: string) => tool('app_open_panel', JSON.stringify({ opened: { kind: 'file', path } }), { id });
  const fwd = (id: number, port: number, samePc = false) => tool('app_forward_port', JSON.stringify({ forward: { port }, typedOn: 'Laptop', samePc }), { id });
  const done = new Set<number>();
  const picks = (rows: Parameters<typeof autoOpenPicks>[0], remote = true, after = 10) => {
    const p = autoOpenPicks(rows, { after, done, remote });
    return [p.panel?.rowId ?? null, p.forward?.rowId ?? null];
  };
  // History (at or before the last full read) never acts, and is not marked either.
  assert.deepEqual(picks([open(9, 'C:/a.ts'), fwd(10, 5173)]), [null, null]);
  // A live batch acts on its last panel and its last forward only; the rest are spent.
  assert.deepEqual(picks([open(11, 'C:/a.ts'), open(12, 'C:/b.ts'), fwd(13, 5173), fwd(14, 3000)]), [12, 14]);
  // The same rows again (a later revision of them) never act twice.
  assert.deepEqual(picks([open(11, 'C:/a.ts'), open(12, 'C:/b.ts'), fwd(14, 3000)]), [null, null]);
  assert.deepEqual([...done].sort((a, b) => a - b), [11, 12, 13, 14]);
  // A forward needs a chat on another PC, and a person who is not at that PC already.
  assert.deepEqual(picks([fwd(15, 8080)], false), [null, null]);
  assert.deepEqual(picks([fwd(16, 8080, true)]), [null, null]);
  assert.deepEqual(picks([fwd(17, 8080), open(18, 'C:/c.ts')]), [18, 17]);
  // A running call or another row is not a pick and is not spent, so it can act once it finishes.
  const running = tool('app_open_panel', JSON.stringify({ opened: { kind: 'file', path: 'C:/d.ts' } }), { id: 19, status: 'streaming' });
  assert.deepEqual(picks([running, row({ id: 20, role: 'assistant', text: 'helm://pages/ab12' })]), [null, null]);
  assert.equal(done.has(19), false);
  assert.deepEqual(picks([{ ...running, status: 'complete' }]), [19, null]);
});
