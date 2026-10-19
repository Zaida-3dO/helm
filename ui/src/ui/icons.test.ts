import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { ArrowUp, Square } from 'lucide-react';
import { kitIcon } from './iconBase.ts';

const Send = kitIcon(ArrowUp, 'arrow-up');
const Stop = kitIcon(Square, 'square');
const html = (el: Parameters<typeof renderToStaticMarkup>[0]) => renderToStaticMarkup(el);

test('a kit icon defaults to 14 px, stroke 2, currentColor, unfilled and decorative', () => {
  const out = html(createElement(Send));
  assert.match(out, /width="14"/);
  assert.match(out, /height="14"/);
  assert.match(out, /stroke-width="2"/);
  assert.match(out, /stroke="currentColor"/);
  assert.match(out, /fill="none"/);
  assert.match(out, /class="[^"]*\bui-icon\b/);
  assert.match(out, /aria-hidden="true"/);
  assert.match(out, /data-icon="arrow-up"/);
  assert.doesNotMatch(out, /role="img"/);
});

test('size, stroke, fill and class can be overridden; the kit class stays', () => {
  const out = html(createElement(Stop, { size: 12, strokeWidth: 2.4, filled: true, className: 'stop-glyph' }));
  assert.match(out, /width="12"/);
  assert.match(out, /stroke-width="2.4"/);
  assert.match(out, /fill="currentColor"/);
  assert.match(out, /class="[^"]*\bui-icon stop-glyph\b/);
});

test('a labelled icon is announced as an image and is not hidden', () => {
  const out = html(createElement(Send, { label: 'Send' }));
  assert.match(out, /role="img"/);
  assert.match(out, /aria-label="Send"/);
  assert.doesNotMatch(out, /aria-hidden/);
});
