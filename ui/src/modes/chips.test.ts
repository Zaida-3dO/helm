import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ProjectNode } from '../api/projects.ts';
import type { ChatModes, ModeBrief, ModeFile } from './api.ts';
import { applyChange, defaultsOf, hasModes, menuModes, shown } from './chips.ts';
import { draftOf, formOf, parseList } from './form.ts';

const brief = (name: string): ModeBrief => ({ name, description: `About ${name}`, layer: 'everyone' });
const start: ChatModes = { available: [brief('garden'), brief('writing')], active: [], off: [] };
const names = (l: { name: string }[]) => l.map((m) => m.name);
const project = (id: string, defaultModes: string[], primaryFolderProjectId: string | null = null) => ({ id, defaultModes, primaryFolderProjectId }) as ProjectNode;

test('chips show what is on, the menu lists the rest, changes show at once, and an unstarted chat shows its defaults on', () => {
  // Which chips show: a started chat shows what the host says; an unstarted one promotes its project's
  // defaults that are only available, and leaves what the host already lists as on or off alone.
  assert.deepEqual(shown(start, true, ['writing']), start, 'a started chat is as the host says');
  const unstarted = shown(start, false, ['Writing', 'missing']);
  assert.deepEqual(names(unstarted.active), ['writing'], 'a default shows as on; an unknown name is ignored');
  assert.equal(unstarted.active[0]?.source, 'project');
  assert.deepEqual(names(unstarted.available), ['garden']);
  const turnedOff: ChatModes = { available: [brief('garden')], active: [], off: [brief('writing')] };
  assert.deepEqual(shown(turnedOff, false, ['writing']), turnedOff, 'a default the user turned off stays off');

  // The "+ mode" menu: available and switched-off modes in name order, the off ones marked.
  assert.deepEqual(menuModes({ available: [brief('garden')], active: [], off: [brief('errands')] }).map((m) => [m.name, m.off]), [['errands', true], ['garden', false]]);
  assert.equal(hasModes({ available: [], active: [], off: [] }), false, 'no mode files in reach: no row');
  assert.equal(hasModes(start), true);

  // Optimistic changes: add moves a mode to on (from available or off), remove moves it to off, unknown names change nothing.
  const added = applyChange(start, { add: ['writing'] });
  assert.deepEqual(added.active.map((m) => [m.name, m.source]), [['writing', 'user']]);
  assert.deepEqual(names(added.available), ['garden']);
  const removed = applyChange(added, { remove: ['writing', 'nope'] });
  assert.deepEqual([names(removed.active), names(removed.off)], [[], ['writing']]);
  assert.deepEqual(applyChange(removed, { add: ['writing'] }).off, [], 'a mode switched off can be added again');
  assert.deepEqual(applyChange(start, { add: ['nope'] }), start);
  // Removing a default shown on an unstarted chat lands in off.
  assert.deepEqual(names(applyChange(unstarted, { remove: ['writing'] }).off), ['writing']);

  // A project's defaults: its own list, else the folder project a board starts chats in.
  const folder = project('f', ['garden']);
  assert.deepEqual(defaultsOf(project('b', ['writing'], 'f'), [folder]), ['writing']);
  assert.deepEqual(defaultsOf(project('b', [], 'f'), [folder]), ['garden'], 'an empty board list falls back to its folder');
  assert.deepEqual(defaultsOf(project('b', [], null), [folder]), []);
  assert.deepEqual(defaultsOf(undefined, [folder]), []);

  // The mode form: lists typed as comma separated text.
  assert.deepEqual(parseList(' a, b ,, a,'), ['a', 'b']);
  const file: ModeFile = { name: 'writing', layer: 'everyone', description: 'Prose', triggers: ['draft', 'essay'], tools: [], mcp: ['maps'], skills: [], rules: [], builtins: [], model: null, body: 'Notes.', revision: '3' };
  const form = formOf(file);
  assert.equal(form.triggers, 'draft, essay');
  assert.deepEqual(draftOf({ ...form, model: ' fast ', mcp: 'maps, hub' }), { description: 'Prose', triggers: ['draft', 'essay'], tools: [], mcp: ['maps', 'hub'], skills: [], rules: [], builtins: [], model: 'fast', body: 'Notes.' });
  assert.equal(draftOf(form).model, null, 'a blank model is none');
});
