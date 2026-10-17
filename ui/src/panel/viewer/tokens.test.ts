import assert from 'node:assert/strict';
import { test } from 'node:test';
import { rangesFromHtml, tokenRanges } from './tokens.ts';

test('nested spans and entities map back onto the source text', () => {
  const source = 'a < "b" & c';
  const html = 'a &lt; <span class="hljs-string">&quot;<span class="x">b</span>&quot;</span> &amp; c';
  assert.deepEqual(rangesFromHtml(html, source), [
    { from: 5, to: 6, className: 'x' },
    { from: 4, to: 7, className: 'hljs-string' },
  ]);
});

test('markup that does not match the source is rejected, not misdrawn', () => {
  assert.equal(rangesFromHtml('<span class="k">ab</span>', 'abc'), null);
  assert.equal(rangesFromHtml('<span class="k">ab', 'ab'), null);
  assert.equal(rangesFromHtml('<b>ab</b>', 'ab'), null);
  assert.equal(rangesFromHtml('&nbsp;', ' '), null);
});

test('real highlighter output lines up with the text it highlighted', () => {
  const source = 'const x = "<b>" && 1; // done\n';
  const ranges = tokenRanges(source, 'typescript');
  const keyword = ranges.find((r) => r.className === 'hljs-keyword');
  assert.ok(keyword);
  assert.equal(source.slice(keyword.from, keyword.to), 'const');
  const str = ranges.find((r) => r.className === 'hljs-string');
  assert.equal(source.slice(str!.from, str!.to), '"<b>"');
  const comment = ranges.find((r) => r.className === 'hljs-comment');
  assert.equal(source.slice(comment!.from, comment!.to), '// done');
  assert.deepEqual(tokenRanges(source, 'no-such-language'), []);
  assert.deepEqual(tokenRanges(source, undefined), []);
});
