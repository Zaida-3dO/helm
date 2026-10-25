// Settings → Hub: Helm's one outbound line to the hub on the home server, and the Telegram chat
// the hub's bot talks to. The address and token are write-only: the host answers only whether
// each is set, so the fields always start empty and Save sends only what was typed.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { hubOps, type HubConnection, type HubConnectionSave, type LineState, type TelegramSettings } from '../../api/hub.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, SettingRow, TextInput } from '../../ui/index.ts';
import { SETTINGS_DIALOG } from '../SettingsDialog.tsx';
import { compactCount } from '../../transcript/rowMeta.ts';
import './hub.css';

/** How often the pane re-reads the line's state while it is open. */
const POLL_MS = 2000;

const STATE_TEXT: Record<LineState, string> = {
  off: 'Not set up',
  connecting: 'Connecting…',
  online: 'Connected',
  offline: 'Offline: retrying',
};

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

type Field = keyof HubConnectionSave;

const FIELDS: { key: Field; title: string; description: string; placeholder: string; secret?: boolean; isSet: (c: HubConnection) => boolean }[] = [
  {
    key: 'lanUrl',
    title: 'Hub address',
    description: "The hub's address on the home network. A LAN or Tailscale IP address, never a public one.",
    placeholder: 'ws://192.168.1.20:8000',
    isSet: (c) => c.hasAddress,
  },
  {
    key: 'fallbackUrl',
    title: 'Fallback address',
    description: 'Tried when the first address does not answer, for example its Tailscale address.',
    placeholder: 'ws://100.64.0.20:8000',
    isSet: (c) => c.hasFallback,
  },
  {
    key: 'token',
    title: 'Relay token',
    description: "The hub's relay token for this PC. Kept in the Windows credential store; it is never shown again.",
    placeholder: 'Paste the token',
    secret: true,
    isSet: (c) => c.hasToken,
  },
];

export function HubPane() {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const [conn, setConn] = useState<HubConnection | null>(null);
  const [draft, setDraft] = useState<HubConnectionSave>({});
  const [tg, setTg] = useState<TelegramSettings | null>(null);
  const [limits, setLimits] = useState<{ hours: string; ktokens: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const showTelegram = useCallback((t: TelegramSettings) => {
    setTg(t);
    setLimits({ hours: String(t.maxAgeHours), ktokens: String(Math.round(t.maxTokens / 1000)) });
  }, []);

  useEffect(() => {
    let live = true;
    const read = () =>
      void ops
        .connection()
        .then((c) => live && setConn(c))
        .catch((e: unknown) => live && setError(errorText(e)));
    read();
    const t = setInterval(read, POLL_MS);
    void ops
      .telegram()
      .then((t) => live && showTelegram(t))
      .catch((e: unknown) => live && setError(errorText(e)));
    return () => {
      live = false;
      clearInterval(t);
    };
  }, [ops, showTelegram]);

  const run = async <T,>(p: Promise<T>, then: (v: T) => void) => {
    setBusy(true);
    try {
      then(await p);
      setError(null);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const typed = Object.fromEntries(Object.entries(draft).filter(([, v]) => v && v.trim())) as HubConnectionSave;
  const save = (a: HubConnectionSave) =>
    void run(ops.saveConnection(a), (c) => {
      setConn(c);
      setDraft({});
    });

  const hours = Number(limits?.hours);
  const ktokens = Number(limits?.ktokens);
  const limitsValid = Number.isInteger(hours) && hours >= 1 && hours <= 720 && Number.isInteger(ktokens) && ktokens >= 10 && ktokens <= 5000;
  const limitsChanged = tg !== null && (hours !== tg.maxAgeHours || ktokens * 1000 !== tg.maxTokens);

  return (
    <div className="hub-pane" data-testid="settings-hub">
      <SettingRow
        title="Connection"
        description="Helm keeps one outbound connection to the hub on your home server. Through it the hub's Telegram bot reaches Helm, and agents outside Helm can use some of Helm's tools. This PC listens for nothing."
      >
        <span className={`hub-state hub-state-${conn?.state ?? 'off'}`} data-testid="hub-state" data-state={conn?.state ?? ''}>
          {conn ? STATE_TEXT[conn.state] : 'Loading…'}
        </span>
      </SettingRow>
      {FIELDS.map((f) => {
        const set = conn ? f.isSet(conn) : false;
        return (
          <SettingRow key={f.key} testId={`hub-field-${f.key}`} title={f.title} description={`${f.description} ${set ? 'Set.' : 'Not set.'}`}>
            <TextInput
              type={f.secret ? 'password' : 'text'}
              autoComplete="off"
              spellCheck={false}
              aria-label={f.title}
              placeholder={set ? 'Leave empty to keep it' : f.placeholder}
              value={draft[f.key] ?? ''}
              onChange={(e) => setDraft((d) => ({ ...d, [f.key]: e.target.value }))}
            />
            {set && (
              <Button variant="text" danger disabled={busy} onClick={() => save({ [f.key]: '' })}>
                Clear
              </Button>
            )}
          </SettingRow>
        );
      })}
      <div className="hub-actions">
        <Button variant="primary" data-testid="hub-save" disabled={busy || Object.keys(typed).length === 0} onClick={() => save(typed)}>
          Save
        </Button>
      </div>

      <h3 className="hub-heading">Telegram</h3>
      <SettingRow
        testId="hub-telegram-chat"
        title="Telegram chat"
        description={
          tg?.chat
            ? `Messages from the hub's bot go to this chat. It became the Telegram chat ${new Date(tg.chat.startedAt).toLocaleString()} and has used ${compactCount(tg.chat.tokensUsed)} tokens since.`
            : 'No Telegram chat yet: the next message from the bot starts one.'
        }
      >
        {tg?.chat && (
          <>
            <Button
              variant="secondary"
              onClick={() => {
                if (!tg.chat) return;
                shell.select(tg.chat.id);
                shell.closeDialog(SETTINGS_DIALOG);
              }}
            >
              Open {tg.chat.title}
            </Button>
            <Button variant="text" data-testid="hub-telegram-reset" disabled={busy} onClick={() => void run(ops.resetTelegram(), showTelegram)}>
              Start a new chat
            </Button>
          </>
        )}
      </SettingRow>
      {limits && (
        <SettingRow
          testId="hub-telegram-limits"
          title="Start a new Telegram chat after"
          description="When the Telegram chat reaches either limit, or is archived, the next message starts a fresh chat."
        >
          <label className="hub-limit">
            <input
              type="number"
              min={1}
              max={720}
              aria-label="Hours"
              value={limits.hours}
              onChange={(e) => setLimits({ ...limits, hours: e.target.value })}
            />
            hours
          </label>
          <label className="hub-limit">
            or
            <input
              type="number"
              min={10}
              max={5000}
              aria-label="Thousand tokens"
              value={limits.ktokens}
              onChange={(e) => setLimits({ ...limits, ktokens: e.target.value })}
            />
            k tokens
          </label>
          <Button variant="secondary" disabled={busy || !limitsValid || !limitsChanged} onClick={() => tg && void run(ops.saveTelegram({ ...tg, maxAgeHours: hours, maxTokens: ktokens * 1000 }), showTelegram)}>
            Save
          </Button>
        </SettingRow>
      )}
      {limits && !limitsValid && <div className="ui-field-error">Hours must be 1 to 720, and tokens 10k to 5,000k.</div>}
      {tg && (
        <SettingRow
          testId="hub-away-alerts"
          title="Telegram alerts while you're away"
          description="While AFK is on, a chat that newly needs you (a question, an approval, a stall) or finishes a reply sends you one line on Telegram. Chats can also message you there when you ask them to."
        >
          <Checkbox
            checked={tg.awayAlerts}
            disabled={busy}
            aria-label="Telegram alerts while you're away"
            data-testid="hub-away-alerts-box"
            onChange={(on) => void run(ops.saveTelegram({ ...tg, awayAlerts: on }), showTelegram)}
          />
        </SettingRow>
      )}
      {tg && (
        <SettingRow
          testId="hub-approvals"
          title="Approve from Telegram"
          description="Permission prompts and plan approvals that wait while you're away, or in the Telegram chat, come to Telegram with a button per option. Only a tap from your own Telegram account on that request's button answers it; a typed reply never approves anything."
        >
          <Checkbox
            checked={tg.approvals}
            disabled={busy}
            aria-label="Approve from Telegram"
            data-testid="hub-approvals-box"
            onChange={(on) => void run(ops.saveTelegram({ ...tg, approvals: on }), showTelegram)}
          />
        </SettingRow>
      )}
      {error && (
        <div className="ui-field-error" role="alert" data-testid="hub-error">
          {error}
        </div>
      )}
    </div>
  );
}
