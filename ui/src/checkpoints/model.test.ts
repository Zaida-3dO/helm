import assert from 'node:assert/strict';
import { test } from 'node:test';
import { row } from '../dev/fixtures.ts';
import { canGoBackTo, checkpointNotes, filesHeading, imageCount, moreFilesLine, type CheckpointInfo } from './model.ts';

const info = (p: Partial<CheckpointInfo> = {}): CheckpointInfo => ({ prompt: 'p', messagesAfter: 1, changedFiles: [], moreFiles: 0, filesRestored: false, ...p });

test('who can be gone back to, and what the confirmation says', () => {
  assert.equal(canGoBackTo(row({ role: 'user' })), true);
  assert.equal(canGoBackTo(row({ role: 'user', status: 'saved' })), false);
  assert.equal(canGoBackTo(row({ role: 'assistant' })), false);
  assert.equal(imageCount(row({ role: 'user', details: { images: [{ id: 'a' }, { id: 'b' }] } as never })), 2);
  assert.equal(imageCount(row({ role: 'user' })), 0);

  const plain = checkpointNotes(info(), 0);
  assert.equal(plain.length, 2);
  assert.match(plain[1]!, /Your files are not restored/);
  const more = checkpointNotes(info({ messagesAfter: 4 }), 1);
  assert.match(more[1]!, /the 3 you sent after it/);
  assert.match(more[2]!, /picture on this message is not brought back/);

  assert.match(filesHeading(info()), /No file changes were recorded/);
  assert.match(filesHeading(info({ changedFiles: ['a.rs'] })), /changed these files/);
  assert.equal(moreFilesLine(info()), '');
  assert.equal(moreFilesLine(info({ moreFiles: 3 })), 'and 3 more');
});
