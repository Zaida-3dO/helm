// Settings → Chat: the transcript element toggles. Each switch saves at
// once through the generic `preference` op, writing the whole object.

import { useEffect, useRef, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Checkbox, Picker, SettingRow, TextArea, TextInput, toast } from '../../ui/index.ts';
import type { SendBehavior } from '../../api/generated/index.ts';
import { HOME_SEND_BEHAVIOR } from '../../api/types.ts';
import { parseSendShortcut } from '../../hosttools/sendShortcut.ts';
import { useScrollOnSend } from '../../composer/scrollOnSend.ts';
import { useDoubleShift } from '../../panel/quickopen/doubleShift.ts';
import type { SendShortcut } from '../../composer/keymap.ts';
import { registerSettingsPane } from '../registry.ts';
import { ELEMENTS, isShown, parseChatElements, type ChatElements } from './elements.ts';
import { LONG_COMMANDS, parseSendBehavior, SEND_MODES, sendBehaviorDiff, WAIT_LIMITS, waitLimitLabel, type LongCommands, type SendMode } from './sendBehavior.ts';
import { parseNameList } from '../applied.ts';

function ChatPane() {
  const shell = useShell();
  const [els, setEls] = useState<ChatElements>({});
  const [ac, setAc] = useState(false);
  // Whether agents may read the code editor's open file and selection (off by default).
  const [ide, setIde] = useState(false);
  // Read live (and updated through the preference event this page dispatches on a change).
  const scrollOnSend = useScrollOnSend();
  const doubleShift = useDoubleShift();
  const [sendKey, setSendKey] = useState<SendShortcut>('mod-enter');
  const [sb, setSb] = useState<SendBehavior>(HOME_SEND_BEHAVIOR);
  const [hiddenSkills, setHiddenSkills] = useState('');
  // The list as last saved, so a failed save can put the box and the window back.
  const savedSkills = useRef<string[]>([]);
  // The model background jobs ask, blank = the small default; same last-saved trick.
  const [bgClaude, setBgClaude] = useState('');
  const [bgLocal, setBgLocal] = useState('');
  const savedBg = useRef<{ claude: string | null; local: string | null }>({ claude: null, local: null });
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
      .api<unknown>('preference_get', { key: 'backgroundModel' })
      .then((v) => {
        const named = (x: unknown) => (typeof x === 'string' && x.trim() !== '' ? x.trim() : null);
        const o = v && typeof v === 'object' ? (v as { claude?: unknown; local?: unknown }) : {};
        savedBg.current = { claude: named(o.claude), local: named(o.local) };
        setBgClaude(savedBg.current.claude ?? '');
        setBgLocal(savedBg.current.local ?? '');
      })
      .catch(() => undefined);
    void shell
      .api<unknown>('preference_get', { key: 'ideContext' })
      .then((v) => setIde(v === true))
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
  const saveBackgroundModel = () => {
    const before = savedBg.current;
    const named = (s: string) => (s.trim() !== '' ? s.trim() : null);
    const next = { claude: named(bgClaude), local: named(bgLocal) };
    if (next.claude === before.claude && next.local === before.local) return;
    savedBg.current = next;
    setBgClaude(next.claude ?? '');
    setBgLocal(next.local ?? '');
    // Blank for both restores the default, which is stored as no value at all.
    const value = next.claude === null && next.local === null ? null : next;
    void shell.api('preference', { key: 'backgroundModel', value }).catch((err: unknown) => {
      savedBg.current = before;
      setBgClaude(before.claude ?? '');
      setBgLocal(before.local ?? '');
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
        description="Send soon hands it to Claude as it works, and Claude reads it at the next step. Interrupt stops the turn and sends it at once."
        testId="setting-send-mode"
      >
        <Picker<SendMode>
          settings
          label="A message sent while Claude is working"
          testId="settings-send-mode"
          value={sb.mode}
          options={SEND_MODES.map((m) => ({ value: m, title: { asap: 'Send soon (default)', interrupt: 'Interrupt' }[m] }))}
          onChange={(mode) => setSendBehavior({ mode })}
        />
      </SettingRow>
      <SettingRow title="Show send choices" description="Offer extra send keys and buttons for a message sent while Claude is working: Send now, or Steer in Interrupt mode." testId="setting-send-choices">
        <Checkbox checked={sb.showChoices} onChange={(showChoices) => setSendBehavior({ showChoices })} />
      </SettingRow>
      <SettingRow
        title="Scroll to the latest message when I send"
        description="Sending jumps the chat to the bottom. Off: the chat stays where you are reading, and the Latest button brings you back."
        testId="setting-scroll-on-send"
      >
        <Checkbox
          checked={scrollOnSend}
          aria-label="Scroll to the latest message when I send"
          data-testid="settings-scroll-on-send"
          onChange={(on) => {
            shell.bus.dispatch({ kind: 'preferences', changed: { scrollOnSend: on } });
            void shell.api('preference', { key: 'scrollOnSend', value: on }).catch((err: unknown) => {
              shell.bus.dispatch({ kind: 'preferences', changed: { scrollOnSend: !on } });
              saveError(err);
            });
          }}
        />
      </SettingRow>
      <SettingRow
        title="Tap Shift twice to go to a file"
        description="Opens Go to file from anywhere in the window, as Ctrl+P does. Typing a capital letter never counts."
        testId="setting-double-shift"
      >
        <Checkbox
          checked={doubleShift}
          aria-label="Tap Shift twice to go to a file"
          data-testid="settings-double-shift"
          onChange={(on) => {
            shell.bus.dispatch({ kind: 'preferences', changed: { doubleShift: on } });
            void shell.api('preference', { key: 'doubleShift', value: on }).catch((err: unknown) => {
              shell.bus.dispatch({ kind: 'preferences', changed: { doubleShift: !on } });
              saveError(err);
            });
          }}
        />
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
      <SettingRow
        title="Agents can read your editor selection"
        description="Lets agents ask for the file open in VS Code and the text selected in it. This needs Helm's small VS Code extension to be installed. Off until you turn it on."
        testId="setting-ide-context"
      >
        <Checkbox
          checked={ide}
          aria-label="Agents can read your editor selection"
          data-testid="settings-ide-context"
          onChange={(on) => {
            setIde(on);
            void shell.api('preference', { key: 'ideContext', value: on }).catch((err: unknown) => {
              setIde(!on);
              saveError(err);
            });
          }}
        />
      </SettingRow>
      <SettingRow
        title="Background model"
        description="The model quiet behind-the-scenes jobs ask: naming a chat, completing your sentence and the idle check. Leave blank for the small built-in model. For chats that run on a local model server, name a model on that server; it is only used there."
        testId="setting-background-model"
      >
        <div style={{ display: 'grid', gap: 6, minWidth: 240 }}>
          <TextInput
            aria-label="Background model for Claude chats"
            data-testid="settings-background-claude"
            placeholder="Claude model, e.g. haiku"
            spellCheck={false}
            value={bgClaude}
            onChange={(e) => setBgClaude(e.target.value)}
            onBlur={saveBackgroundModel}
          />
          <TextInput
            aria-label="Background model for chats on a local model server"
            data-testid="settings-background-local"
            placeholder="Local model, e.g. qwen2.5-coder-7b-instruct"
            spellCheck={false}
            value={bgLocal}
            onChange={(e) => setBgLocal(e.target.value)}
            onBlur={saveBackgroundModel}
          />
        </div>
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
