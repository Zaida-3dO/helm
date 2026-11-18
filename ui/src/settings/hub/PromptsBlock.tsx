// Settings → Hub's Prompts block: what a new conversation on each channel (the speaker, the
// household speaker, the voice app, Telegram) is told about how its answers are heard or read.
// Each box starts on the default; saving replaces it for this profile, Reset puts the default back.
// A change applies to new conversations.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { hubOps, type ChannelPrompt, type ChannelPrompts } from '../../api/hub.ts';
import { useShell } from '../../shell/context.ts';
import { Button, SettingRow, TextArea } from '../../ui/index.ts';
import { promptState } from './channelScope.ts';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

function PromptBox({ prompt, max, busy, onSave }: { prompt: ChannelPrompt; max: number; busy: boolean; onSave: (text: string) => void }) {
  const [draft, setDraft] = useState(prompt.text);
  useEffect(() => setDraft(prompt.text), [prompt.text]);
  const st = promptState(draft, prompt, max);
  return (
    <SettingRow
      testId={`hub-prompt-${prompt.channel}`}
      title={`${prompt.label} prompt`}
      description={prompt.custom ? 'Your own wording is in use. It applies to new conversations.' : 'The standard wording. Change it to make it yours; it applies to new conversations.'}
    >
      <TextArea
        rows={6}
        aria-label={`${prompt.label} prompt`}
        data-testid={`hub-prompt-${prompt.channel}-text`}
        value={draft}
        disabled={busy}
        onChange={(e) => setDraft(e.target.value)}
      />
      <Button variant="secondary" data-testid={`hub-prompt-${prompt.channel}-save`} disabled={busy || !st.dirty || st.tooLong} onClick={() => onSave(draft)}>
        Save
      </Button>
      <Button variant="text" data-testid={`hub-prompt-${prompt.channel}-reset`} disabled={busy || !st.canReset} onClick={() => onSave('')}>
        Reset
      </Button>
      {st.tooLong && <div className="ui-field-error">At most {max} characters.</div>}
    </SettingRow>
  );
}

export function PromptsBlock({ profileId }: { profileId: string | null }) {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const [prompts, setPrompts] = useState<ChannelPrompts | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    // Another profile's prompts must not stay on screen, ready to be saved over this one's.
    setPrompts(null);
    setError(null);
    if (!profileId) return;
    let live = true;
    void ops
      .prompts({ profileId })
      .then((p) => live && setPrompts(p))
      .catch((e: unknown) => live && setError(errorText(e)));
    return () => {
      live = false;
    };
  }, [ops, profileId]);

  const save = useCallback(
    async (channel: ChannelPrompt['channel'], text: string) => {
      if (!profileId) return;
      setBusy(true);
      try {
        setPrompts(await ops.savePrompt({ profileId, channel, text }));
        setError(null);
      } catch (e) {
        setError(errorText(e));
      } finally {
        setBusy(false);
      }
    },
    [ops, profileId],
  );

  return (
    <div data-testid="hub-prompts">
      <h3 className="hub-heading">Prompts</h3>
      {prompts?.channels.map((p) => (
        <PromptBox key={p.channel} prompt={p} max={prompts.maxChars} busy={busy} onSave={(t) => void save(p.channel, t)} />
      ))}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="hub-prompts-error">
          {error}
        </div>
      )}
    </div>
  );
}
