// The sidebar's household block: other people's shared chats, read only, one group per computer
// ("<Person> · <computer>"). All profiles lists every computer on the hub; a profile linked to a
// person lists that person's. A computer that is off shows its last list greyed with "offline since";
// one that stopped sharing drops its rows. Polls every 30 s while it is on screen.

import { useMemo, useState } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { age } from '../sidebar/status.ts';
import { useHouseholdWatch, useProfiles } from './hooks.ts';
import { householdTargets, offlineText, orderPeerChats } from './model.ts';
import { RemoteChatDialog, type RemoteChat } from './RemoteChatDialog.tsx';

export function HouseholdSection({ api, bus }: { api: Api; bus: EventBus }) {
  const { store, snap } = useProfiles(api, bus);
  const ready = snap.loaded && snap.view?.homeId != null;
  useHouseholdWatch(store, ready);
  const targets = useMemo(() => householdTargets(snap.view, snap.people), [snap.view, snap.people]);
  const [shut, setShut] = useState<ReadonlySet<string>>(new Set());
  const [open, setOpen] = useState<RemoteChat | null>(null);
  const now = Date.now();

  const groups = targets
    .map((t) => ({ t, g: snap.groups.get(t.key) }))
    .filter(({ t, g }) => !t.machine.online || (g && (g.chats.length > 0 || g.note)));
  if (!ready || groups.length === 0) return null;

  return (
    <div className="pf-house" data-testid="household">
      {groups.map(({ t, g }) => {
        const collapsed = shut.has(t.key);
        const chats = orderPeerChats(g?.chats ?? []);
        const offline = !t.machine.online;
        return (
          <section key={t.key} className="pf-house-group" data-offline={offline ? 'true' : undefined} data-testid="household-group">
            <button
              type="button"
              className="pf-house-head"
              aria-expanded={!collapsed}
              onClick={() => setShut((s) => (s.has(t.key) ? new Set([...s].filter((k) => k !== t.key)) : new Set(s).add(t.key)))}
            >
              <span className="pf-dot" data-online={offline ? 'false' : 'true'} aria-hidden="true" />
              <span className="pf-house-title">{t.title}</span>
              {offline && <span className="pf-house-state">{offlineText(t.machine, now)}</span>}
            </button>
            {!collapsed && g?.note && !offline && <div className="pf-house-note">{g.note}</div>}
            {!collapsed &&
              chats.map((c) => (
                <button
                  key={c.chatId}
                  type="button"
                  className="pf-house-row"
                  data-testid="household-chat"
                  disabled={!t.machine.sharing.transcripts || offline}
                  title={!t.machine.sharing.transcripts ? `${t.person.name} has not allowed reading their chats.` : undefined}
                  onClick={() =>
                    setOpen({
                      personId: t.person.id,
                      machineId: t.machine.id,
                      chat: c,
                      personName: t.person.name,
                      machineName: t.machine.name,
                      canSend: t.machine.sharing.transcripts && t.machine.sharing.messages,
                    })
                  }
                >
                  <span className="pf-house-chat-title">{c.title || 'Untitled chat'}</span>
                  {c.needsAttention && <span className="pf-house-flag" role="img" aria-label="Needs attention" title="Needs attention" />}
                  <span className="pf-house-age">{age(c.updatedAt, now)}</span>
                </button>
              ))}
          </section>
        );
      })}
      {open && <RemoteChatDialog store={store} target={open} onClose={() => setOpen(null)} />}
    </div>
  );
}
