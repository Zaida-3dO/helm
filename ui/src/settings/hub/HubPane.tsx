// Settings → Hub: Helm's one outbound line to the hub on the home server, and the Telegram chat
// the hub's bot talks to. The address and token are write-only: the host answers only whether
// each is set, so the fields always start empty and Save sends only what was typed.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { LocalModelInfo } from '../../api/types.ts';
import { hubOps, type HubConnection, type HubConnectionSave, type LineState, type TelegramLink, type TelegramLinkOffer, type TelegramSettings } from '../../api/hub.ts';
import { profilesOps, type ProfileSummary } from '../../api/profiles.ts';
import { localModelChoices } from '../../composer/modelEffort.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Picker, SettingRow, TextInput } from '../../ui/index.ts';
import { SETTINGS_DIALOG } from '../SettingsDialog.tsx';
import { EFFORT_OPTIONS } from '../runtime/model.ts';
import { compactCount } from '../../transcript/rowMeta.ts';
import { defaultProfileId, linkLine, linkable, profileChoices } from './channelScope.ts';
import { HubPairing } from './HubPairing.tsx';
import { VoiceBlock } from './VoiceBlock.tsx';
import './hub.css';

/** How often the pane re-reads the line's state while it is open. */
const POLL_MS = 2000;
/** How often the pane asks the hub whether an offered Telegram link has been tapped. */
const LINK_POLL_MS = 4000;

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

/** Picker options for the model a new Telegram chat opens with: the app default first, then the
 *  model list (Claude's models and any local ones, as everywhere else). A saved model the list
 *  no longer carries still shows, so the picker never lies. */
function modelChoices(
  models: { id: string; name: string; description?: string }[],
  saved: string | null,
): { value: string; title: string; subtitle?: string }[] {
  const out: { value: string; title: string; subtitle?: string }[] = [
    { value: '', title: 'App default', subtitle: 'The same model any other new chat starts with' },
  ];
  for (const m of models) {
    if (!m.id || out.some((o) => o.value === m.id)) continue;
    out.push({ value: m.id, title: m.name || m.id, subtitle: m.description || undefined });
  }
  if (saved && !out.some((o) => o.value === saved)) out.push({ value: saved, title: saved });
  return out;
}

/** Picker options for the effort a new Telegram chat opens with: app default first, then the
 *  same levels every other effort picker offers. A saved effort the list no longer carries still
 *  shows, so the picker never lies (same promise as `modelChoices`). */
function effortChoices(saved: string | null): { value: string; title: string; subtitle?: string }[] {
  const out: { value: string; title: string; subtitle?: string }[] = [
    { value: '', title: 'App default', subtitle: 'The same effort any other new chat starts with' },
    ...EFFORT_OPTIONS,
  ];
  if (saved && !out.some((o) => o.value === saved)) out.push({ value: saved, title: saved });
  return out;
}

export function HubPane() {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const prof = useMemo(() => profilesOps(shell.api), [shell.api]);
  const [conn, setConn] = useState<HubConnection | null>(null);
  const [draft, setDraft] = useState<HubConnectionSave>({});
  const [tg, setTg] = useState<TelegramSettings | null>(null);
  const [limits, setLimits] = useState<{ hours: string; ktokens: string } | null>(null);
  const [models, setModels] = useState<{ id: string; name: string; description?: string }[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The Telegram and Voice sections speak for one profile at a time; every read and save below
  // names it, so each person on this PC sets up their own channels.
  const [people, setPeople] = useState<ProfileSummary[] | null>(null);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [link, setLink] = useState<TelegramLink | null>(null);
  const [offer, setOffer] = useState<TelegramLinkOffer | null>(null);
  const [pinNote, setPinNote] = useState<string | null>(null);

  const showTelegram = useCallback((t: TelegramSettings) => {
    setTg(t);
    setLimits({ hours: String(t.maxAgeHours), ktokens: String(Math.round(t.maxTokens / 1000)) });
  }, []);

  const readTelegram = useCallback(
    (pid: string) =>
      void ops
        .telegram({ profileId: pid })
        .then(showTelegram)
        .catch((e: unknown) => setError(errorText(e))),
    [ops, showTelegram],
  );

  useEffect(() => {
    let live = true;
    const read = () =>
      void prof
        .list()
        .then((v) => {
          if (!live) return;
          setPeople(v.profiles);
          setProfileId((cur) => (cur && v.profiles.some((p) => p.id === cur) ? cur : defaultProfileId(v.profiles, v.activeId)));
        })
        .catch(() => undefined);
    read();
    // A profile made, renamed or linked somewhere: the picker follows.
    const off = shell.bus.on('profiles_changed', read);
    return () => {
      live = false;
      off();
    };
  }, [prof, shell]);

  // Telegram and voice state, and the hub's answer about this profile's Telegram account.
  useEffect(() => {
    if (!profileId) return;
    let live = true;
    readTelegram(profileId);
    // A hub that cannot answer (no line, no bot) reads as not linked; the reason goes to the
    // error line below the pane.
    void ops
      .tgStatus({ profileId })
      .then((l) => live && setLink(l))
      .catch(() => live && setLink({ linked: false }));
    const off = shell.bus.on('relay_telegram', () => live && readTelegram(profileId));
    return () => {
      live = false;
      off();
    };
  }, [ops, shell, profileId, readTelegram]);

  // While a link offer is open, watch for the phone tapping it: the account links on the hub.
  useEffect(() => {
    if (!offer || !profileId) return;
    const t = setInterval(() => {
      void ops
        .tgStatus({ profileId })
        .then((l) => {
          setLink(l);
          if (l.linked) {
            setOffer(null);
            readTelegram(profileId);
          }
        })
        .catch(() => undefined);
    }, LINK_POLL_MS);
    return () => clearInterval(t);
  }, [ops, offer, profileId, readTelegram]);

  useEffect(() => {
    let live = true;
    const read = () =>
      void ops
        .connection()
        .then((c) => live && setConn(c))
        .catch((e: unknown) => live && setError(errorText(e)));
    read();
    const t = setInterval(read, POLL_MS);
    void shell
      .api<{ models: { id: string; name: string; description?: string }[]; local?: LocalModelInfo[] }>('model_catalogue', { harness: 'claude' })
      .then((c) => live && setModels([...c.models, ...localModelChoices(c.local)]))
      .catch(() => undefined);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, [ops, shell]);

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
  const telegramModels = useMemo(() => modelChoices(models, tg?.model ?? null), [models, tg?.model]);
  const current = people?.find((p) => p.id === profileId);
  const canLink = linkable(current);

  /** Save one of the new-chat defaults or a switch; empty string means back to unset (the app default). */
  const saveTelegram = (patch: Partial<Pick<TelegramSettings, 'model' | 'effort' | 'takeTelegram'>>) => {
    if (!tg || !profileId) return;
    void run(ops.saveTelegram({ ...tg, ...patch, profileId }), showTelegram);
  };

  const linkTelegram = () => {
    if (!profileId) return;
    void run(ops.tgLink({ profileId }), (r) => setOffer(r));
  };
  const unlinkTelegram = () => {
    if (!profileId) return;
    void run(
      ops.tgUnlink({ profileId }),
      () => void ops.tgStatus({ profileId }).then(setLink).catch(() => undefined),
    );
  };
  const pinTelegram = () => {
    if (!profileId) return;
    void run(ops.routePin({ profileId, channel: 'telegram' }), () => setPinNote('Your Telegram now starts on this PC.'));
  };

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

      <HubPairing />

      {people && people.length > 1 && (
        <SettingRow
          testId="hub-profile"
          title="Profile"
          description="Telegram and voice are set up per profile: each profile is its own person on the hub, with its own account and its own switches."
        >
          <Picker
            settings
            label="Profile"
            testId="hub-profile-picker"
            value={profileId ?? ''}
            options={profileChoices(people)}
            disabled={busy}
            onChange={(v) => {
              setProfileId(v);
              setLink(null);
              setOffer(null);
              setPinNote(null);
            }}
          />
        </SettingRow>
      )}

      <h3 className="hub-heading">Telegram</h3>
      <SettingRow
        testId="hub-telegram-link"
        title="Telegram account"
        description={
          canLink
            ? 'Link this profile’s person’s Telegram account so the bot knows whose messages are whose. One tap on the phone: Helm shows a link, you tap it there.'
            : 'This profile has no person on the hub yet: link it to a person first (Settings → Profiles).'
        }
      >
        <span data-testid="hub-telegram-link-state">{linkLine(link)}</span>
        {canLink && !link?.linked && (
          <Button variant="secondary" disabled={busy || !!offer} onClick={linkTelegram}>
            Link Telegram
          </Button>
        )}
        {canLink && link?.linked && (
          <Button variant="text" disabled={busy} onClick={unlinkTelegram}>
            Unlink
          </Button>
        )}
      </SettingRow>
      {offer && (
        <div className="hub-offer" data-testid="hub-telegram-link-offer">
          Open this on the phone holding the Telegram account — the code stops working at{' '}
          {new Date(offer.expiresAt).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })}:
          <code className="hub-offer-url">{offer.url}</code>
        </div>
      )}
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
            <Button variant="text" data-testid="hub-telegram-reset" disabled={busy} onClick={() => profileId && void run(ops.resetTelegram({ profileId }), showTelegram)}>
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
          <Button variant="secondary" disabled={busy || !limitsValid || !limitsChanged} onClick={() => tg && profileId && void run(ops.saveTelegram({ ...tg, maxAgeHours: hours, maxTokens: ktokens * 1000, profileId }), showTelegram)}>
            Save
          </Button>
        </SettingRow>
      )}
      {limits && !limitsValid && <div className="ui-field-error">Hours must be 1 to 720, and tokens 10k to 5,000k.</div>}
      {tg && (
        <SettingRow
          title="Model for new Telegram chats"
          description="The model a new Telegram chat opens with. App default opens it on the same model as any other new chat."
        >
          <Picker
            settings
            label="Model for new Telegram chats"
            testId="hub-telegram-model"
            value={tg.model ?? ''}
            options={telegramModels}
            searchable
            disabled={busy}
            onChange={(v) => saveTelegram({ model: v || null })}
          />
        </SettingRow>
      )}
      {tg && (
        <SettingRow
          title="Effort for new Telegram chats"
          description="How hard the agent thinks before answering in a new Telegram chat. App default uses the same effort as any other new chat."
        >
          <Picker
            settings
            label="Effort for new Telegram chats"
            testId="hub-telegram-effort"
            value={tg.effort ?? ''}
            options={effortChoices(tg.effort)}
            disabled={busy}
            onChange={(v) => saveTelegram({ effort: v || null })}
          />
        </SettingRow>
      )}
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
      {tg && (
        <SettingRow
          testId="hub-remote-answers"
          title="Allow answering waiting prompts from the Remote page"
          description="Permission prompts, questions and plan approvals that wait in a chat can be answered from the hub's Remote page. Off, the Remote page cannot answer them."
        >
          <Checkbox
            checked={tg.remoteAnswers}
            disabled={busy}
            aria-label="Allow answering waiting prompts from the Remote page"
            data-testid="hub-remote-answers-box"
            onChange={(on) => void run(ops.saveTelegram({ ...tg, remoteAnswers: on }), showTelegram)}
          />
        </SettingRow>
      )}
      {tg && (
        <SettingRow
          testId="hub-telegram-take"
          title="Take my Telegram on this PC"
          description="Off, this PC never receives this person's Telegram, even while it is the only one up. On, it joins the PCs that may carry it. Your own PC always does; this switch is for the others."
        >
          <Checkbox
            checked={tg.takeTelegram}
            disabled={busy}
            aria-label="Take my Telegram on this PC"
            data-testid="hub-telegram-take-box"
            onChange={(on) => saveTelegram({ takeTelegram: on })}
          />
        </SettingRow>
      )}
      {tg && (
        <SettingRow
          testId="hub-telegram-route"
          title="Where my Telegram goes"
          description="A conversation stays on the PC that took its last message while that PC is up. Pinning starts the next one here instead; the bot's /pc command shows and changes the same thing."
        >
          <Button variant="secondary" disabled={busy || !canLink} onClick={pinTelegram}>
            Use this PC
          </Button>
          {pinNote && (
            <span data-testid="hub-telegram-pinned" className="hub-state">
              {pinNote}
            </span>
          )}
        </SettingRow>
      )}
      <VoiceBlock profileId={profileId} />
      {error && (
        <div className="ui-field-error" role="alert" data-testid="hub-error">
          {error}
        </div>
      )}
    </div>
  );
}
