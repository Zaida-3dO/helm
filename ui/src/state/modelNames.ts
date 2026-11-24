// Where a model id's display name comes from: the name the user gave it in Settings → Local
// models, else the short name Helm makes from the model's own. One read feeds every label — the
// top bar chip, the composer's model line and the picker — and the same read reaches another PC:
// for a chat that runs elsewhere the labels come from that PC's own catalogue, so its aliases show
// there as they do there.
//
// A chat cannot rename a model: aliases live in the local-model settings, which only the Settings
// page saves (the agents' settings keys deliberately hold no alias map), so nothing else can
// relabel a model behind the user's back.

import { useEffect, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { LocalModelsState } from '../settings/localmodels/model.ts';
import { isLocalModelId, prettyLocalModelId } from './localModel.ts';

type Labels = Map<string, string>;

const caches = new Map<string, Labels>();
const loading = new Map<string, Promise<void>>();
let version = 0;
const listeners = new Set<() => void>();

function bump(): void {
  version += 1;
  for (const l of listeners) l();
}

export function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

export function snapshotVersion(): number {
  return version;
}

const keyOf = (deviceUid?: string): string => (deviceUid ? `device:${deviceUid}` : 'local');

/** Take in this PC's Local models settings: the alias a model has, else its default short name. */
export function primeLocalLabels(s: LocalModelsState): void {
  const m: Labels = new Map();
  for (const n of s.modelNames) {
    const label = n.alias ?? n.defaultName;
    if (label) m.set(n.id, label);
  }
  caches.set('local', m);
  loading.delete('local');
  bump();
}

/** This PC's labels went stale (an agent changed the local-model settings); read them again. */
export function invalidateLocalLabels(): void {
  caches.delete('local');
  loading.delete('local');
  bump();
}

/** Read one PC's labels once; repeats while it is loading share the read. A failed read leaves
 * the fallback (the name made from the model id) and tries again next time. */
export function ensureLabels(api: Api, deviceUid?: string): Promise<void> {
  const key = keyOf(deviceUid);
  const now = loading.get(key);
  if (now) return now;
  const p = (
    deviceUid
      ? api<{ local?: { id: string; name: string }[] }>('model_catalogue', { harness: 'claude', deviceUid }).then((c) => {
          const m: Labels = new Map();
          for (const r of c.local ?? []) if (r.name) m.set(r.id, r.name);
          caches.set(key, m);
        })
      : api<LocalModelsState>('local_models_state', {}).then(primeLocalLabels)
  ).then(
    () => bump(),
    () => {
      loading.delete(key);
    },
  );
  loading.set(key, p);
  return p;
}

/** The display name of a model id right now: the label on record if it has one, else (for a
 * local id) the short name made from the id. null when the name is only in a catalogue that is
 * not read yet (a Claude id). */
export function modelDisplayName(id: string | null | undefined, deviceUid?: string): string | null {
  if (!id) return null;
  const hit = caches.get(keyOf(deviceUid))?.get(id);
  if (hit) return hit;
  if (isLocalModelId(id)) return prettyLocalModelId(id);
  return null;
}

/** The React view of one model id's display name: the label on record (reading it in the first
 * time), upgraded in place once it arrives. */
export function useModelDisplayName(
  api: Api,
  bus: EventBus,
  model: string | null,
  deviceUid: string | null = null,
): string | null {
  useSyncExternalStore(subscribe, snapshotVersion);
  useEffect(() => {
    if (!model) return;
    void ensureLabels(api, deviceUid ?? undefined);
    if (deviceUid) return;
    // A chat changed the local-model settings: this PC's labels may have gone stale.
    return bus.on('local_models', () => {
      invalidateLocalLabels();
      void ensureLabels(api);
    });
  }, [api, bus, model, deviceUid]);
  return modelDisplayName(model, deviceUid ?? undefined);
}
