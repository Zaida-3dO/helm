import assert from 'node:assert/strict';
import { test } from 'node:test';
import { optionCaption, optionDraftText, optionSendText, parseOptions } from './options.ts';

const labels = (t: string) => parseOptions(t)?.map((o) => o.label) ?? null;

test('a numbered list followed by a question is a set of choices', () => {
  assert.deepEqual(labels('Two ways forward:\n\n1. Rewrite the parser\n2. Patch the parser\n\nWhich do you prefer?'), ['1', '2']);
});

test('the item text becomes the tooltip title, without markdown', () => {
  const o = parseOptions('Pick one:\n1. **Rewrite** the [parser](http://x) in `rust`\n2. Patch it');
  assert.equal(o?.[0]?.title, 'Rewrite the parser in rust');
  assert.equal(o?.[1]?.title, 'Patch it');
});

test('long titles are cut at 80 characters', () => {
  const o = parseOptions(`Choose:\n1. ${'word '.repeat(40)}\n2. short`);
  assert.ok((o?.[0]?.title.length ?? 0) <= 80);
  assert.ok(o?.[0]?.title.endsWith('…'));
});

test('explicit "Option N" markers need no other signal', () => {
  assert.deepEqual(labels('Option 1: keep going\nOption 2: stop here'), ['1', '2']);
  assert.deepEqual(labels('**Option A** - fast\n**Option B** - safe'), ['A', 'B']);
});

test('letter markers: A) and **A.** forms', () => {
  assert.deepEqual(labels('Your call.\n\nA) tweak it\nB) rebuild it\nC) leave it'), ['A', 'B', 'C']);
  assert.deepEqual(labels('Pick one of these:\n**A.** one\n**B.** two'), ['A', 'B']);
});

test('lower-case letter markers are upper-cased', () => {
  assert.deepEqual(labels('Which approach?\n\na) first\nb) second'), ['A', 'B']);
});

test('a final question mark is itself the signal', () => {
  assert.deepEqual(labels('1) one thing\n2) another thing\n\nShall I go with the first?'), ['1', '2']);
});

test('trailing markdown after the question mark is ignored', () => {
  assert.deepEqual(labels('1. one\n2. two\n\n**Which one?**'), ['1', '2']);
});

test('bullets in front of the marker are allowed', () => {
  assert.deepEqual(labels('Which route would you take?\n- 1. north\n- 2. south'), ['1', '2']);
});

test('nine options are fine', () => {
  const nine = Array.from({ length: 9 }, (_, i) => `${i + 1}. item ${i + 1}`).join('\n');
  assert.equal(parseOptions(`Pick one:\n${nine}`)?.length, 9);
});

test('a tenth item is not a marker, so only nine are offered', () => {
  const ten = Array.from({ length: 10 }, (_, i) => `${i + 1}. item ${i + 1}`).join('\n');
  assert.equal(parseOptions(`Pick one:\n${ten}`)?.length, 9);
});

test('continuation lines belong to their item', () => {
  const o = parseOptions('Which one?\n\n1. First\n   with more detail\n2. Second\n   and more');
  assert.deepEqual(o?.map((x) => x.label), ['1', '2']);
});

test('CRLF text parses the same', () => {
  assert.deepEqual(labels('Pick:\r\n1. a\r\n2. b\r\n'), ['1', '2']);
});

test('the last list wins when a reply has two', () => {
  const o = parseOptions('Earlier ideas:\n1. a\n2. b\n\nNow, which of these do you want?\n\nA) x\nB) y');
  assert.deepEqual(o?.map((x) => x.label), ['A', 'B']);
});

// ---- false positives ----

test('completed steps are not choices', () => {
  assert.equal(parseOptions('I did the following:\n\n1. Edited the parser\n2. Ran the tests\n3. Committed the change'), null);
});

test('a plan of numbered steps with no question is not a set of choices', () => {
  assert.equal(parseOptions('Plan:\n\n1. Add the table\n2. Add the op\n3. Add the test\n\nStarting now.'), null);
});

test('a single item is not a set of choices', () => {
  assert.equal(parseOptions('Which one?\n1. Only this'), null);
});

test('a list that does not start at 1 or A is not offered', () => {
  assert.equal(parseOptions('Which one?\n2. second\n3. third'), null);
});

test('non-sequential markers are not a list', () => {
  assert.equal(parseOptions('Which one?\n1. a\n3. c'), null);
});

test('numbered lines inside a code fence are ignored', () => {
  assert.equal(parseOptions('Which one?\n```\n1. a\n2. b\n```\n'), null);
});

test('a long tail after the list means the list was not the ending', () => {
  const tail = 'More explanation follows here. '.repeat(30);
  assert.equal(parseOptions(`Which one?\n\n1. a\n2. b\n\n${tail}`), null);
});

test('text with no list yields nothing', () => {
  assert.equal(parseOptions('Done. The tests pass.'), null);
  assert.equal(parseOptions(''), null);
});

test('prose that starts lines with a letter and a full stop is not a list', () => {
  assert.equal(parseOptions('A. Smith wrote the paper.\nB. Jones replied to it.'), null);
});

test('inline numbering in a sentence is not a list', () => {
  assert.equal(parseOptions('Which one? 1) a 2) b'), null);
});

test('a numbered list later followed by an unrelated question still counts as signalled', () => {
  // The final question mark is the signal even when the intro is neutral; a reply that offers
  // numbered items and ends by asking is, in practice, asking which.
  assert.deepEqual(labels('Findings:\n1. one\n2. two\n\nWhat do you think?'), ['1', '2']);
});

test('captions and the texts a click and a pencil produce', () => {
  const o = { label: 'B', title: 'x' };
  assert.equal(optionCaption(o), 'Option B');
  assert.equal(optionSendText(o), 'Go with option B.');
  assert.equal(optionDraftText(o), 'Go with option B ');
});
