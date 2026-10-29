// Settings → Web search: Helm's own web search (Brave Search) for chats. The key field is
// write-only (the host answers only whether a key is set, so the field always starts empty),
// Test runs one search and shows OK or Brave's refusal, and "Import from OpenClaw" reads the key
// from that assistant's settings file once. The switch offers the search tool to Claude chats too;
// chats on a local model have it whenever a key is set.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, SettingRow, TextInput } from '../../ui/index.ts';
import { webSearchOps, type WebSearchTest, type WebSearchView } from './api.ts';
import './websearch.css';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export function WebSearchPane() {
  const shell = useShell();
  const ops = useMemo(() => webSearchOps(shell.api), [shell.api]);
  const [view, setView] = useState<WebSearchView | null>(null);
  const [keyDraft, setKeyDraft] = useState('');
  /** The settings file path as typed; `null` follows the host's value until the user edits it. */
  const [pathDraft, setPathDraft] = useState<string | null>(null);
  const [tested, setTested] = useState<WebSearchTest | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(
    () =>
      ops
        .get()
        .then(setView)
        .catch((e: unknown) => setError(errorText(e))),
    [ops],
  );

  useEffect(() => {
    void load();
    return shell.bus.on('web_search', () => void load());
  }, [shell, load]);

  /** Run one action: clear the old messages, show its error if it fails. */
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const saveKey = () =>
    run(async () => {
      setView(await ops.setKey(keyDraft));
      setKeyDraft('');
      setTested(null);
      setNote('Key saved.');
    });

  const forgetKey = () =>
    run(async () => {
      setView(await ops.forgetKey());
      setTested(null);
      setNote('Key forgotten.');
    });

  const test = () =>
    run(async () => {
      setTested(null);
      setTested(await ops.test());
    });

  const importKey = () =>
    run(async () => {
      await ops.importKey(pathDraft ?? view?.importPath ?? '');
      setPathDraft(null);
      setTested(null);
      setNote('Key imported.');
      await load();
    });

  const setOffer = (on: boolean) => run(async () => setView(await ops.save(on)));

  const hasKey = view?.hasKey ?? false;
  const path = pathDraft ?? view?.importPath ?? '';
  return (
    <div className="websearch-pane" data-testid="settings-websearch">
      <SettingRow
        testId="websearch-key"
        title="Brave Search key"
        description={`Gives chats a web search tool that uses your own Brave Search key (the free plan is enough for light use). Kept in the Windows credential store and never shown again. ${hasKey ? 'A key is set.' : 'No key is set.'}`}
      >
        <div className="websearch-field">
          <TextInput
            type="password"
            autoComplete="off"
            spellCheck={false}
            aria-label="Brave Search key"
            data-testid="websearch-key-input"
            placeholder={hasKey ? 'Paste a new key to replace it' : 'Paste the key'}
            value={keyDraft}
            onChange={(e) => setKeyDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && keyDraft.trim() && !busy) void saveKey();
            }}
          />
          <Button variant="primary" data-testid="websearch-key-save" disabled={busy || !keyDraft.trim()} onClick={() => void saveKey()}>
            {hasKey ? 'Replace' : 'Save'}
          </Button>
          {hasKey && (
            <Button variant="text" danger data-testid="websearch-key-forget" disabled={busy} onClick={() => void forgetKey()}>
              Forget
            </Button>
          )}
        </div>
      </SettingRow>
      <SettingRow
        testId="websearch-test"
        title="Test"
        description={hasKey ? 'Runs one search with the saved key and shows whether Brave answered. It uses one search of your allowance.' : 'Add a key first.'}
      >
        {tested && (
          <span
            className={`websearch-result ${tested.ok ? 'websearch-result-ok' : 'websearch-result-bad'}`}
            role="status"
            data-testid="websearch-test-result"
            data-ok={tested.ok}
          >
            {tested.message}
          </span>
        )}
        <Button variant="secondary" data-testid="websearch-test-button" disabled={busy || !hasKey} onClick={() => void test()}>
          Test
        </Button>
      </SettingRow>
      <SettingRow
        testId="websearch-import"
        title="Import from OpenClaw"
        description="Reads the Brave Search key from OpenClaw's settings file once and keeps it here. Nothing is shown and the file is not changed. Change the path if your file is somewhere else."
      >
        <div className="websearch-field websearch-path">
          <TextInput
            aria-label="OpenClaw settings file"
            data-testid="websearch-path"
            spellCheck={false}
            value={path}
            onChange={(e) => setPathDraft(e.target.value)}
          />
          <Button variant="secondary" data-testid="websearch-import-button" disabled={busy || !path.trim()} onClick={() => void importKey()}>
            Import
          </Button>
        </div>
      </SettingRow>
      <SettingRow
        testId="websearch-offer"
        title="Offer in Claude chats"
        description="Chats on a local model always get web search while a key is set. Turn this on to give Claude chats the search tool too; each search spends your Brave allowance. A chat sees the change from its next start."
      >
        <Checkbox
          checked={view?.offerInClaudeChats ?? false}
          disabled={busy || !view}
          aria-label="Offer in Claude chats"
          data-testid="websearch-offer-box"
          onChange={(on) => void setOffer(on)}
        />
      </SettingRow>
      {note && (
        <div className="setting-desc" role="status" data-testid="websearch-note">
          {note}
        </div>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="websearch-error">
          {error}
        </div>
      )}
    </div>
  );
}
