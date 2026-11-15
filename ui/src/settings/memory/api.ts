// The memory index ops (host crate `memindex`) that Settings → Memory uses. Every one answers with
// the whole state; changes also come in on the `memindex` event, from the indexer's own thread.

import type { Api } from '../../api/client.ts';
import type { MemindexSettingsChanges, MemindexSourceInput, MemindexState } from './model.ts';

export function memindexOps(api: Api) {
  return {
    state: () => api<MemindexState>('memindex_state', {}),
    /** Changes the switches; only the fields you name move, the rest keep. A refusal is an error with a plain message. */
    saveSettings: (changes: MemindexSettingsChanges) => api<MemindexState>('memindex_settings_save', changes),
    /**
     * A source with no id is added; with an id it changes. Fields left out of `patch` keep their
     * value — the host reads an absent or null field as "keep".
     */
    saveSource: (input: { id: string | null; kind?: MemindexSourceInput['kind']; path?: string | null; personId?: string | null; scope?: MemindexSourceInput['scope'] | null; include?: string[] | null; exclude?: string[] | null; enabled?: boolean | null }) =>
      api<MemindexState>('memindex_sources_save', {
        id: input.id,
        kind: input.kind ?? null,
        path: input.path ?? null,
        personId: input.personId ?? null,
        scope: input.scope ?? null,
        include: input.include ?? null,
        exclude: input.exclude ?? null,
        enabled: input.enabled ?? null,
      } satisfies MemindexSourceInput),
    /** Takes a source off this PC and its docs off the hub. */
    removeSource: (id: string) => api<MemindexState>('memindex_sources_remove', { id }),
    /** Looks over the sources now instead of waiting for the watcher or the nightly pass. */
    reindex: (a: { sourceId?: string; force?: boolean } = {}) => api<{ started: boolean }>('memindex_reindex', a),
  };
}
