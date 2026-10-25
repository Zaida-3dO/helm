import assert from 'node:assert/strict';
import { test } from 'node:test';
import { answerLabel, answerText, defaultOption, optionCaption, optionDraftText, optionSendText, parseOptions, parseReplyQuestion } from './options.ts';

const labels = (t: string) => parseOptions(t)?.map((o) => o.label) ?? null;

test('a numbered list followed by a question is a set of choices', () => {
  assert.deepEqual(labels('Two ways forward:\n\n1. Rewrite the parser\n2. Patch the parser\n\nWhich do you prefer?'), ['1', '2']);
});

test('each item is kept in full, without markdown, and gets a short form for the chip', () => {
  const rows: [string, string, string][] = [
    ['Pick one:\n1. **Rewrite** the [parser](http://x) in `rust`\n2. Patch it', 'Rewrite the parser in rust', 'Rewrite the parser in rust'],
    ['Pick one:\n1. Split the stylesheet by area: faster to review\n2. x', 'Split the stylesheet by area: faster to review', 'Split the stylesheet by area'],
    [`Choose:\n1. ${'word '.repeat(40)}\n2. short`, 'word '.repeat(40).trim(), 'word word word word word word…'],
    ['Pick one:\n1. Keep going\n2. Stop here - safest', 'Keep going', 'Keep going'],
  ];
  for (const [text, full, short] of rows) {
    const o = parseOptions(text)?.[0];
    assert.equal(o?.text, full, text);
    assert.equal(o?.short.slice(0, 20), short.slice(0, 20), text);
    assert.ok((o?.short.length ?? 0) <= 36, text);
  }
  assert.equal(optionCaption(parseOptions('Pick one:\n1. Keep going\n2. Stop here')![1]!), 'Stop here');
  assert.equal(optionCaption({ label: 'B', text: '', short: '', recommended: false }), 'Option B');
});

test('an option keeps its continuation lines and sub-bullets, but not the closing question', () => {
  const o = parseOptions('Which one?\n\n1. First\n   with more detail\n   - and a point\n2. Second\n\nWhich do you prefer?');
  assert.equal(o?.[0]?.text, 'First with more detail and a point');
  assert.equal(o?.[1]?.text, 'Second');
});

test('a recommended or default marker is noticed and taken out of the text', () => {
  const rows: [string, number | null, string][] = [
    ['Which one?\n1. Rewrite it\n2. Patch it (Recommended)', 1, 'Patch it'],
    ['Which one?\n1. Rewrite it [default]\n2. Patch it', 0, 'Rewrite it'],
    ['Which one?\n1. Rewrite it\n2. Patch it - recommended', 1, 'Patch it'],
    ['Which one?\n1. Rewrite it\n2. Keep the default settings', null, 'Keep the default settings'],
  ];
  for (const [text, at, shown] of rows) {
    const opts = parseOptions(text)!;
    assert.equal(opts.findIndex((o) => o.recommended), at ?? -1, text);
    assert.equal(opts[at ?? 1]!.text, shown, text);
    assert.equal(defaultOption(opts), at === null ? null : opts[at]!);
  }
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

test('the popup question is the prose around the list, and multi-select is read from its wording', () => {
  const rows: [string, string, boolean][] = [
    ['Two ways forward:\n\n1. Rewrite the parser\n2. Patch the parser\n\nWhich do you prefer?', 'Two ways forward:\n\nWhich do you prefer?', false],
    ['Some notes first.\n\nWhich areas should I cover? Select all that apply.\n\n1. Docs\n2. Tests\n3. CI', 'Which areas should I cover? Select all that apply.', true],
    ['I looked at the code.\n\nOptions:\n\nOption A: tweak\nOption B: rebuild', 'I looked at the code.\n\nOptions:', false],
  ];
  for (const [text, question, multi] of rows) {
    const q = parseReplyQuestion(text);
    assert.equal(q?.question, question, text);
    assert.equal(q?.multi, multi, text);
  }
  assert.equal(parseReplyQuestion('Done. The tests pass.'), null);
});

test('what an answer sends names the option and writes it out; typed text is sent as typed', () => {
  const [a, b, c] = parseOptions('Which areas?\n1. Split the stylesheet by area\n2. Move the tokens (Recommended)\n3. Re-run checks.')!;
  const rows: [string, string][] = [
    [answerText([a!]), 'Go with option 1: Split the stylesheet by area.'],
    [answerText([b!]), 'Go with option 2: Move the tokens.'],
    [optionSendText(c!), 'Go with option 3: Re-run checks.'],
    [answerText([a!, c!]), 'Go with options 1 and 3: Split the stylesheet by area; Re-run checks.'],
    [answerText([a!, b!, c!]), 'Go with options 1, 2 and 3: Split the stylesheet by area; Move the tokens; Re-run checks.'],
    [answerText([b!], 'and keep the old names'), 'Go with option 2: Move the tokens. Also: and keep the old names.'],
    [answerText([], '  something else entirely '), 'something else entirely'],
    [answerText([{ label: 'B', text: '', short: '', recommended: false }]), 'Go with option B.'],
    [optionDraftText(a!), 'Go with option 1: Split the stylesheet by area '],
  ];
  for (const [got, want] of rows) assert.equal(got, want);
  assert.deepEqual([1, 2, 5].map(answerLabel), ['Answer question', 'Answer 2 questions', 'Answer 5 questions']);
});
