// Settings → Chat: the transcript element toggles. Each switch saves at
// once through the generic `preference` op, writing the whole object.

import { useEffect, useRef, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Checkbox, Picker, SettingRow, TextArea, toast } from '../../ui/index.ts';
import type { SendBehavior } from '../../api/generated/index.ts';
import { HOME_SEND_BEHAVIOR } from '../../api/types.ts';
import { parseSendShortcut } from '../../hosttools/sendShortcut.ts';
import type { SendShortcut } from '../../composer/keymap.ts';
import { registerSettingsPane } from '../registry.ts';
import { ELEMENTS, isShown, parseChatElements, type ChatElements } from './elements.ts';
import { LONG_COMMANDS, parseSendBehavior, SEND_MODES, sendBehaviorDiff, WAIT_LIMITS, waitLimitLabel, type LongCommands, type SendMode } from './sendBehavior.ts';
import { parseNameList } from '../applied.ts';

function ChatPane() {
  const shell = useShell();
  const [els, setEls] = useState<ChatElements>({});
  const [ac, setAc] = useState(false);
  const [sendKey, setSendKey] = useState<SendShortcut>('mod-enter');
  const [sb, setSb] = useState<SendBehavior>(HOME_SEND_BEHAVIOR);
  const [hiddenSkills, setHiddenSkills] = useState('');
  // The list as last saved, so a failed save can put the box and the window back.
  const savedSkills = useRef<string[]>([]);
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'sendShortcut' })
      .then((v) => setSendKey(parseSendShortcut(v)))
      .catch(() => undefined);
    void shell
      .api<unknown>('preference_get', { key: 'sendBehavior' })
      .then((v) => setSb(parseSendBehavior(v)))
      .catch(() => undefined);
    void shell
      .api<unknown>('preference_get', { key: 'hiddenSkills' })
      .then((v) => {
        savedSkills.current = parseNameList(v);
        setHiddenSkills(savedSkills.current.join('\n'));
      })
      .catch(() => undefined);
    void shell
      .api<unknown>('preference_get', { key: 'chatElements' })
      .then((v) => setEls(parseChatElements(v)))
      .catch(() => undefined);
    void shell
      .api<unknown>('preference_get', { key: 'autocomplete' })
      .then((v) => setAc(Boolean(v && typeof v === 'object' && (v as { enabled?: unknown }).enabled === true)))
      .catch(() => undefined);
  }, [shell]);
  const saveError = (err: unknown) => toast.show({ kind: 'error', text: `Could not save: ${String((err as Error).message ?? err)}` });
  // Each send-behaviour control saves the whole setting (only the keys that differ from the defaults).
  const setSendBehavior = (patch: Partial<SendBehavior>) => {
    const before = sb;
    const next = { ...sb, ...patch };
    setSb(next);
    void shell.api('preference', { key: 'sendBehavior', value: sendBehaviorDiff(next) }).catch((err: unknown) => {
      setSb(before);
      saveError(err);
    });
  };
  const saveHiddenSkills = () => {
    const before = savedSkills.current;
    const list = parseNameList(hiddenSkills.split('\n').map((n) => n.trim().replace(/^[$/]/, '')));
    if (list.join('\n') === before.join('\n')) return setHiddenSkills(list.join('\n'));
    savedSkills.current = list;
    setHiddenSkills(list.join('\n'));
    shell.bus.dispatch({ kind: 'preferences', changed: { hiddenSkills: list } });
    void shell.api('preference', { key: 'hiddenSkills', value: list }).catch((err: unknown) => {
      savedSkills.current = before;
      setHiddenSkills(before.join('\n'));
      shell.bus.dispatch({ kind: 'preferences', changed: { hiddenSkills: before } });
      saveError(err);
    });
  };
  return (
    <div data-testid="settings-chat">
      {/* The Send key. App.tsx follows the preference live. */}
      <SettingRow title="Send message with" description="Ctrl+Enter sends and Enter starts a new line, or Enter sends and Shift+Enter starts a new line.">
        <Picker
          settings
          label="Send message with"
          testId="settings-send-key"
          value={sendKey}
          options={[
            { value: 'mod-enter', title: 'Ctrl+Enter' },
            { value: 'enter', title: 'Enter' },
          ]}
          onChange={(v: SendShortcut) => {
            const before = sendKey;
            setSendKey(v);
            shell.bus.dispatch({ kind: 'preferences', changed: { sendShortcut: v } });
            void shell.api('preference', { key: 'sendShortcut', value: v }).catch((err: unknown) => {
              setSendKey(before);
              shell.bus.dispatch({ kind: 'preferences', changed: { sendShortcut: before } });
              toast.show({ kind: 'error', text: `Could not save: ${String((err as Error).message ?? err)}` });
            });
          }}
        />
      </SettingRow>
      <SettingRow
        title="A message sent while Claude is working"
        description="Queue waits for the current turn to finish. Send soon hands it to Claude as it works. Interrupt stops the turn and sends it at once."
        testId="setting-send-mode"
      >
        <Picker<SendMode>
          settings
          label="A message sent while Claude is working"
          testId="settings-send-mode"
          value={sb.mode}
          options={SEND_MODES.map((m) => ({ value: m, title: { queue: 'Queue it', asap: 'Send soon (default)', interrupt: 'Interrupt' }[m] }))}
          onChange={(mode) => setSendBehavior({ mode })}
        />
      </SettingRow>
      <SettingRow title="Show send choices" description="Offer Steer now and Send now on queued messages." testId="setting-send-choices">
        <Checkbox checked={sb.showChoices} onChange={(showChoices) => setSendBehavior({ showChoices })} />
      </SettingRow>
      <SettingRow
        title="Wait limit"
        description="How long a message sent mid-turn may wait to be picked up before it is sent right away."
        testId="setting-wait-limit"
      >
        <Picker<string>
          settings
          label="Wait limit"
          testId="settings-wait-limit"
          value={String(sb.waitLimitSeconds)}
          options={WAIT_LIMITS.map((s) => ({ value: String(s), title: waitLimitLabel(s) }))}
          onChange={(v) => setSendBehavior({ waitLimitSeconds: v === 'null' ? null : Number(v) })}
        />
      </SettingRow>
      <SettingRow title="Stop sends waiting messages" description="Stopping a task also sends the messages waiting behind it, instead of pausing the queue." testId="setting-stop-sends">
        <Checkbox checked={sb.stopSendsWaiting} onChange={(stopSendsWaiting) => setSendBehavior({ stopSendsWaiting })} />
      </SettingRow>
      <SettingRow title="Long commands" description="What happens when Claude starts a build, a test run, a server or a long sleep in the foreground." testId="setting-long-commands">
        <Picker<LongCommands>
          settings
          label="Long commands"
          testId="settings-long-commands"
          value={sb.longCommands}
          options={LONG_COMMANDS.map((m) => ({ value: m, title: { off: 'Allow them', warn: 'Remind Claude', redirect: 'Send to the background (default)' }[m] }))}
          onChange={(longCommands) => setSendBehavior({ longCommands })}
        />
      </SettingRow>
      <SettingRow title="Stop one tool" description="Show a Stop button on a running tool, to end it without ending the whole turn." testId="setting-tool-stop">
        <Checkbox checked={sb.toolStop} onChange={(toolStop) => setSendBehavior({ toolStop })} />
      </SettingRow>
      <SettingRow title="Autocomplete" description="Suggest the rest of the word you are typing; Tab accepts. Uses Claude Code's small model, at most 10 requests a minute.">
        <Checkbox
          checked={ac}
          onChange={(on) => {
            setAc(on);
            shell.bus.dispatch({ kind: 'preferences', changed: { autocomplete: { enabled: on } } });
            void shell.api('preference', { key: 'autocomplete', value: { enabled: on } }).catch((err: unknown) => {
              setAc(!on);
              shell.bus.dispatch({ kind: 'preferences', changed: { autocomplete: { enabled: !on } } });
              toast.show({ kind: 'error', text: `Could not save: ${String((err as Error).message ?? err)}` });
            });
          }}
        />
      </SettingRow>
      {ELEMENTS.map((e) => (
        <SettingRow key={e.id} title={e.title} description={e.description}>
          <Checkbox
            checked={isShown(els, e.id)}
            onChange={(on) => {
              const before = els;
              const next: ChatElements = { ...els, [e.id]: on ? 'shown' : 'hidden' };
              setEls(next);
              shell.bus.dispatch({ kind: 'preferences', changed: { chatElements: next } });
              void shell.api('preference', { key: 'chatElements', value: next }).catch((err: unknown) => {
                setEls(before);
                shell.bus.dispatch({ kind: 'preferences', changed: { chatElements: before } });
                toast.show({ kind: 'error', text: `Could not save: ${String((err as Error).message ?? err)}` });
              });
            }}
          />
        </SettingRow>
      ))}
      <SettingRow title="Hidden skills" description="Skills to leave out of the / menu, one name per line, without the slash." testId="setting-hidden-skills">
        <TextArea
          aria-label="Hidden skills"
          data-testid="settings-hidden-skills"
          rows={3}
          spellCheck={false}
          value={hiddenSkills}
          onChange={(e) => setHiddenSkills(e.target.value)}
          onBlur={saveHiddenSkills}
        />
      </SettingRow>
    </div>
  );
}

registerSettingsPane({ id: 'chat', title: 'Chat', order: 30, render: () => <ChatPane /> });
