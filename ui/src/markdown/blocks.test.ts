import { test } from 'node:test';
import assert from 'node:assert/strict';
import { StreamBlocks } from './blocks.ts';
import { renderToHtml } from './render.ts';
import type { RenderOptions } from './render.ts';

const CORPUS = [
  'Hello **world**.\n\nA second paragraph with `code` and a [link](https://example.com).',
  '# Title\n\nIntro line\nwrapped line.\n\n- one\n- two\n  - nested\n\n1. first\n\n2. loose second\n\nAfter the list.',
  'Before\n\n```ts\nconst a = 1;\n\nconst b = 2;\n```\n\nBetween\n\n~~~\nplain fence\n~~~\n\nAfter',
  '| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\n> quote\nlazy continuation\n\n> [!note] Callout title\n> body line\n\nDone.',
  'Setext heading\n---\n\n***\n\n    indented code\n\n    more indented\n\nText with C:\\Users\\me\\file.ts and [[Note#Heading|alias]] and ![[image.png]].',
  'A [ref link][r] used after its definition.\n\n[r]: https://example.com/r\n\nAnd [r] again.',
  '<div>raw html block</div>\n\nInline <b>tag</b> and a<br>break.\n\n- [ ] task\n- [x] done task',
  '```mermaid\ngraph TD\nA-->B\n```\n\nText after the diagram.',
  'Windows line endings\r\n\r\n- a\r\n- b\r\n\r\nend',
  // A definition in an early block, used in later blocks that finish on their own.
  '[r]: https://example.com/r\n\nUse [r] here.\n\nAnd [r] there.\n\nA last line.',
  // A label defined twice: marked keeps the first and drops the second, text and all.
  '[1]: https://a.example\n\nSee [1].\n\n[1]: https://b.example\n\nAgain [1].\n\nEnd.\n\nAfter the end.',
];

function streamOut(text: string, cuts: number[], options: RenderOptions = {}): string {
  const s = new StreamBlocks();
  let out: string[] = [];
  let tail = '';
  const apply = (u: ReturnType<StreamBlocks['update']>) => {
    if (u.reset) out = [];
    out.push(...u.added);
    tail = u.tail;
  };
  for (const c of cuts) apply(s.update(text.slice(0, c), { ...options, streaming: true }));
  apply(s.update(text, options, true));
  return out.join('') + tail;
}

test('streamed in any chunks, the finished output equals a full render', () => {
  for (const text of CORPUS) {
    const full = renderToHtml(text);
    // Every character, then a few coarse chunkings.
    const every = Array.from({ length: text.length }, (_, i) => i + 1);
    assert.equal(streamOut(text, every), full, `char by char: ${JSON.stringify(text.slice(0, 30))}`);
    for (const step of [3, 7, 40]) {
      const cuts = [];
      for (let i = step; i < text.length; i += step) cuts.push(i);
      assert.equal(streamOut(text, cuts), full, `step ${step}: ${JSON.stringify(text.slice(0, 30))}`);
    }
  }
});

test('interactive options carry through (a closed Mermaid fence becomes a diagram)', () => {
  const text = CORPUS[7]!;
  const html = streamOut(text, [10, 25, 40], { interactive: true });
  assert.equal(html, renderToHtml(text, { interactive: true }));
  assert.match(html, /data-md-mermaid/);
});

test('only the last block is re-rendered; finished blocks come out once', () => {
  const s = new StreamBlocks();
  let u = s.update('First paragraph.', { streaming: true });
  assert.deepEqual(u.added, []);
  assert.match(u.tail, /First paragraph/);
  // A partial first line could still turn out to continue the block before it.
  u = s.update('First paragraph.\n\nSecond', { streaming: true });
  assert.deepEqual(u.added, []);
  u = s.update('First paragraph.\n\nSecond\n', { streaming: true });
  assert.equal(u.added.length, 1);
  assert.match(u.added[0]!, /First paragraph/);
  assert.doesNotMatch(u.tail, /First/);
  assert.match(u.tail, /Second/);
  u = s.update('First paragraph.\n\nSecond\ngrowing', { streaming: true });
  assert.deepEqual(u.added, []);
  assert.match(u.tail, /Second\ngrowing/);
  assert.equal(s.finished, 1);
});

test('an unclosed fence stays the tail, however many blank lines it holds', () => {
  const s = new StreamBlocks();
  const u = s.update('Intro\n\n```py\nx = 1\n\n\ny = 2\n', { streaming: true });
  assert.equal(u.added.length, 1);
  assert.match(u.tail, /x = [\s\S]*y = /);
});

test('a rewritten text starts again', () => {
  const s = new StreamBlocks();
  s.update('One.\n\nTwo.\n\nThree', { streaming: true });
  const u = s.update('Completely different.\n\nNext\n', { streaming: true });
  assert.equal(u.reset, true);
  assert.equal(u.added.length, 1);
  assert.match(u.added[0]!, /Completely different/);
});

test('frontmatter is rendered whole, as the tail', () => {
  const text = '---\ntitle: Note\n---\n\nBody text.';
  const s = new StreamBlocks();
  const u = s.update(text, { streaming: true });
  assert.deepEqual(u.added, []);
  assert.match(u.tail, /md-properties/);
  const end = s.update(text, {}, true);
  assert.deepEqual(end.added, []);
  assert.equal(end.tail, renderToHtml(text));
});

test('the work per update follows the last block, not the whole text', () => {
  // A 60 KB reply in 30-char snapshots: the tail stays small, so no update renders the whole text.
  const para = 'Some words in a paragraph that goes on for a while. '.repeat(4);
  let text = '';
  const s = new StreamBlocks();
  let longestTail = 0;
  while (text.length < 60_000) {
    text += text.length % 900 < 30 ? '\n\n' : para.slice(0, 30);
    const u = s.update(text, { streaming: true });
    longestTail = Math.max(longestTail, u.tail.length);
  }
  assert.ok(longestTail < 2_000, `the tail grew to ${longestTail} chars`);
  assert.ok(s.finished > 50);
});

test('words revealed after the stream ends still land in the last block', () => {
  // The pacer keeps revealing for a moment after the row completes: "done" must not freeze the
  // last block, or each later word becomes a paragraph of its own.
  const s = new StreamBlocks();
  s.update('Looking into it', { streaming: true });
  s.update("Looking into it. I will", {}, true);
  const u = s.update("Looking into it. I will check the folder.", {}, true);
  assert.deepEqual(u.added, []);
  assert.equal(u.tail, renderToHtml('Looking into it. I will check the folder.'));
});

test('done renders the last block as finished: an open Mermaid fence becomes a diagram', () => {
  const text = 'Intro\n\n```mermaid\ngraph TD\nA-->B\n';
  const s = new StreamBlocks();
  assert.doesNotMatch(s.update(text, { streaming: true, interactive: true }).tail, /data-md-mermaid/);
  assert.match(s.update(text, { interactive: true }, true).tail, /data-md-mermaid/);
});
