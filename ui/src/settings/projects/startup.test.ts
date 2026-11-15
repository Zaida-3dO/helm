import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ProjectNode } from '../../api/projects.ts';
import { changes, formOf, isAbsolutePath, parseLines, problem } from './startup.ts';

const project = { id: 'p1', revision: 4, startupInstructions: 'Make a worktree.', startupFiles: ['C:\\notes\\a.md'], startupServers: ['hub'] } as ProjectNode;

test('the startup form reads lines, checks what it holds and sends only what changed', () => {
  assert.deepEqual(parseLines(' C:\\a.md \r\n\n/b.md\nC:\\a.md\n'), ['C:\\a.md', '/b.md']);
  for (const [p, abs] of [['C:\\a.md', true], ['d:/x', true], ['/home/x', true], ['\\\\nas\\share\\x', true], ['notes.md', false], ['..\\x', false]] as const) {
    assert.equal(isAbsolutePath(p), abs, p);
  }
  const form = formOf(project);
  assert.deepEqual(form, { instructions: 'Make a worktree.', files: 'C:\\notes\\a.md', servers: ['hub'] });
  assert.equal(problem(form), null);
  assert.equal(changes(project, form), null, 'nothing changed');
  assert.deepEqual(changes(project, { ...form, instructions: ' ' }), { projectId: 'p1', revision: 4, startupInstructions: '' }, 'blank clears');
  assert.deepEqual(changes(project, { ...form, files: 'C:\\notes\\a.md\nC:\\b.md', servers: ['maps', 'hub'] }), {
    projectId: 'p1',
    revision: 4,
    startupFiles: ['C:\\notes\\a.md', 'C:\\b.md'],
    startupServers: ['maps', 'hub'],
  });
  assert.deepEqual(changes(project, { ...form, servers: [] }), { projectId: 'p1', revision: 4, startupServers: [] }, 'none ticked clears');
  assert.match(problem({ ...form, files: 'relative.md' }) ?? '', /absolute/);
  assert.match(problem({ ...form, files: Array.from({ length: 21 }, (_, i) => `C:\\f${i}.md`).join('\n') }) ?? '', /at most 20/);
  assert.match(problem({ ...form, instructions: 'x'.repeat(4001) }) ?? '', /4000/);
});
