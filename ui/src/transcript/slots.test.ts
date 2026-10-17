import assert from 'node:assert/strict';
import { test } from 'node:test';
import { row } from '../dev/fixtures.ts';
import {
  messageActionsFor,
  registerMessageAction,
  registerRowFooter,
  registerSelectionAction,
  rowFootersFor,
  selectionActionsFor,
} from './slots.ts';
import {
  accessoriesAt,
  collectSendContributions,
  composerApi,
  mountComposer,
  registerComposerAccessory,
  registerSendContributor,
} from '../composer/slots.ts';

const sctx = { chatId: 'c1', isLatestAssistant: true, running: false, interactionOpen: false };

test('message actions and footers apply by their predicates; a throwing slot is left out', () => {
  const offA = registerMessageAction({ id: 'save', label: 'Save', appliesTo: (r) => r.role === 'assistant', run: () => undefined });
  const offB = registerMessageAction({ id: 'boom', label: 'Boom', appliesTo: () => { throw new Error('x'); }, run: () => undefined });
  const offF = registerRowFooter({ id: 'opts', appliesTo: (_r, c) => c.isLatestAssistant, render: () => null });
  try {
    assert.deepEqual(messageActionsFor(row({ role: 'assistant' })).map((a) => a.id), ['save']);
    assert.deepEqual(messageActionsFor(row({ role: 'user' })), []);
    assert.equal(rowFootersFor(row({ role: 'assistant' }), sctx).length, 1);
    assert.equal(rowFootersFor(row({ role: 'assistant' }), { ...sctx, isLatestAssistant: false }).length, 0);
  } finally {
    offA();
    offB();
    offF();
  }
  assert.deepEqual(messageActionsFor(row({ role: 'assistant' })), [], 'unregistered');
});

test('registering an id again replaces it', () => {
  const off1 = registerSelectionAction({ id: 'quote', label: 'Quote', run: () => undefined });
  const off2 = registerSelectionAction({ id: 'quote', label: 'Quote it', run: () => undefined });
  const sel = { chatId: 'c1', text: 'x', rowIds: [1] };
  assert.deepEqual(selectionActionsFor(sel).map((a) => a.label), ['Quote it']);
  off2();
  off1();
  assert.equal(selectionActionsFor(sel).length, 0);
});

test('composer slots: accessories by placement, contributions merged, insert reaches the mounted composer', () => {
  const offA = registerComposerAccessory({ id: 'eye', placement: 'toolbar', render: () => null });
  const offC = registerSendContributor((chatId) => (chatId === 'c1' ? { context: { path: 'a.rs' } } : undefined));
  const got: string[] = [];
  const unmount = mountComposer('c1', { insert: (t) => got.push(t), focus: () => undefined });
  try {
    assert.deepEqual(accessoriesAt('toolbar').map((a) => a.id), ['eye']);
    assert.deepEqual(accessoriesAt('above'), []);
    assert.deepEqual(collectSendContributions('c1'), { context: { path: 'a.rs' } });
    assert.deepEqual(collectSendContributions('c2'), {});
    assert.equal(composerApi('c1').insert('hello'), true);
    assert.deepEqual(got, ['hello']);
    assert.equal(composerApi('c2').insert('x'), false, 'no composer for that chat');
  } finally {
    offA();
    offC();
    unmount();
  }
  assert.equal(composerApi('c1').insert('late'), false);
});
