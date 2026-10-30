import assert from 'node:assert/strict';
import { test } from 'node:test';
import { localModelsFakeOps } from '../../dev/fake-localmodels.ts';
import { localModelsOps } from './api.ts';
import { clampTo, draftProblem, newDraft, rangeText, reachText, sameDrafts, savePayload, toDraft, type ServerView } from './model.ts';

const view = (p: Partial<ServerView> = {}): ServerView => ({ id: 's1', name: 'Strata', kind: 'strata', baseUrl: 'http://127.0.0.1:8081', enabled: true, autoStart: true, hasKey: false, ...p });

test('server drafts: what is refused before asking the host, what changed, and what is saved', () => {
  // An address must be plain http to this machine; the host refuses the same on save.
  const ok = toDraft(view());
  assert.equal(draftProblem(ok), null);
  assert.deepEqual([ok.autoStart, ok.folder, ok.preset], [true, '', ''], 'start-on-use is on by default, folder and preset fall back to the standards');
  const problems: [Partial<typeof ok>, RegExp][] = [
    [{ name: ' ' }, /name/],
    [{ baseUrl: '' }, /address/],
    [{ baseUrl: 'not a url' }, /valid URL/],
    [{ baseUrl: 'https://127.0.0.1:8081' }, /plain http/],
    [{ baseUrl: 'http://192.168.1.20:8081' }, /on this machine/],
    [{ baseUrl: 'http://localhost.evil.com:8081' }, /on this machine/],
  ];
  for (const [patch, want] of problems) assert.match(draftProblem({ ...ok, ...patch }) ?? '', want, JSON.stringify(patch));
  for (const url of ['http://[::1]:8081', 'http://localhost:9000/', 'http://127.0.0.1']) assert.equal(draftProblem({ ...ok, baseUrl: url }), null, url);
  // A new server starts with no id and no address; changing any field makes the lists differ, the card key does not.
  const added = newDraft('ollama');
  assert.deepEqual([added.id, added.baseUrl, added.kind], ['', '', 'ollama']);
  assert.equal(sameDrafts([ok], [{ ...ok, key: 'other' }]), true);
  assert.equal(sameDrafts([ok], [{ ...ok, enabled: false }]), false);
  assert.equal(sameDrafts([ok], []), false);
  // The save replaces the list: trimmed, the model, folder and preset only for Strata and only when given.
  assert.deepEqual(savePayload([{ ...ok, name: ' Strata ', baseUrl: ' http://127.0.0.1:8081 ', model: ' qwen ', folder: ' D:/Strata ', preset: ' Fast ' }, { ...added, name: 'Ollama', baseUrl: 'http://localhost:9000', model: 'ignored', folder: 'ignored', preset: 'ignored', autoStart: false }]), [
    { id: 's1', name: 'Strata', kind: 'strata', baseUrl: 'http://127.0.0.1:8081', enabled: true, autoStart: true, model: 'qwen', folder: 'D:/Strata', preset: 'Fast' },
    { id: '', name: 'Ollama', kind: 'ollama', baseUrl: 'http://localhost:9000', enabled: true, autoStart: false },
  ]);
  // What a card says about the server, and numbers kept inside their range.
  assert.deepEqual(reachText(view()), { text: 'Not checked yet', tone: 'idle' });
  assert.deepEqual(reachText(view({ reachable: true })), { text: 'Answering', tone: 'ok' });
  assert.deepEqual(reachText(view({ reachable: false, note: 'Connection refused.' })), { text: 'Connection refused.', tone: 'bad' });
  const range = { min: 8192, max: 262144, default: 65536 };
  assert.deepEqual(['1', '1,000,000', '65 536', '100000.4', 'abc', ''].map((t) => clampTo(range, t)), [8192, 262144, 65536, 100000, null, null]);
  assert.equal(rangeText(range, ' tokens'), 'Between 8,192 tokens and 262,144 tokens (default 65,536 tokens)');
});

test('the pane\'s flow against the dev host: opening asks no server, only Check servers and Test do; keys are write-only; saves are refused with a plain message', async () => {
  const fake = localModelsFakeOps();
  const events: unknown[] = [];
  const api = async <T,>(op: string, args: unknown = {}): Promise<T> => fake.handler(op, args as Record<string, unknown>, { emit: (e: unknown) => events.push(e) } as never) as T;
  const ops = localModelsOps(api);

  // Opening reads the state, however often: no server has answered and the catalogue holds no models.
  // Only "Check servers" (the refresh) fills them in.
  for (let i = 0; i < 3; i++) await ops.state();
  const first = await ops.state();
  assert.equal(first.servers.some((s) => s.reachable !== undefined), false);
  assert.deepEqual(fake.catalogue.local(), []);
  const { models } = await ops.refresh();
  assert.equal(models.length, fake.catalogue.local().length);
  const after = await ops.state();
  assert.deepEqual(after.servers.map((s) => [s.id, s.reachable]), [['strata', true], ['ollama', true], ['office', false]]);
  assert.match(after.servers.find((s) => s.id === 'office')?.note ?? '', /Nothing answered/);
  assert.equal(models.find((m) => m.id === 'local/office/qwen3:30b-a3b')?.reachable, false);

  // A key goes in and only a flag comes out; forgetting clears it.
  const withKey = await ops.setKey('strata', 'fake-strata-key');
  assert.equal(withKey.servers.find((s) => s.id === 'strata')?.hasKey, true);
  assert.equal(JSON.stringify(withKey).includes('fake-strata-key'), false);
  assert.equal((await ops.forgetKey('strata')).servers.find((s) => s.id === 'strata')?.hasKey, false);

  // Saves replace the list; an address off this machine, a kind change and an out-of-range number are refused with a sentence.
  const drafts = after.servers.map(toDraft);
  await assert.rejects(ops.saveServers([{ ...drafts[0]!, baseUrl: 'http://example.com:8081' }]), /on this machine/);
  await assert.rejects(ops.saveServers([{ ...drafts[0]!, kind: 'ollama' }]), /cannot be changed/);
  await assert.rejects(ops.saveSettings({ ollamaContext: 5 }), /between 8192 and 262144/);
  const saved = await ops.saveServers([drafts[0]!, { ...newDraft('ollama'), name: 'Spare', baseUrl: 'http://localhost:9000' }]);
  assert.deepEqual(saved.servers.map((s) => s.name), ['Strata', 'Spare']);
  const tuned = await ops.saveSettings({ ollamaContext: 32768, keepAliveMinutes: 30, strataNeedGb: 20, tools: 'full', compareWith: 'opus' });
  assert.deepEqual([tuned.ollamaContext, tuned.keepAliveMinutes, tuned.strataNeedGb, tuned.tools, tuned.compareWith], [32768, 30, 20, 'full', 'opus']);
  assert.equal((await ops.test('strata')).ok, true);
  assert.equal(events.every((e) => (e as { kind: string }).kind === 'local_models'), true);
});
