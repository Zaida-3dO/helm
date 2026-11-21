// The settings frame: a centred modal of fixed size on Ctrl+,. The title, the two-row tab bar
// (groups on the first row, the chosen group's panes on the second) and the search field stay put
// (the first Escape clears the search); only the selected pane's body scrolls. A search lists
// every matching row from every pane under its pane's name, whatever its group (see search.ts).
// Panes come from the registry; each pane decides whether it saves per control or with a Save.

import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react';
import { useDialog, useShell } from '../shell/context.ts';
import { Dialog, useLayer } from '../ui/index.ts';
import { IconSearch } from '../ui/icons.tsx';
import { applySearch, clearHighlight } from './search.ts';
import { SettingsWatchers } from './watchers.tsx';
import { settingsPanes, subscribeSettingsPanes, peekRequestedPane, clearRequestedPane } from './registry.ts';
import { settingsTabStrip, scrollEdges } from './layout.ts';

export const SETTINGS_DIALOG = 'settings';

export function SettingsDialog() {
  const { open, close } = useDialog(SETTINGS_DIALOG);
  return (
    <>
      <SettingsWatchers />
      {open && <SettingsBody close={close} />}
    </>
  );
}

function SettingsBody({ close }: { close: () => void }) {
  const panes = useSyncExternalStore(subscribeSettingsPanes, settingsPanes);
  const [tab, setTab] = useState<string | null>(peekRequestedPane);
  const [group, setGroup] = useState<string | null>(null);
  useEffect(() => clearRequestedPane, []);
  const [query, setQuery] = useState('');
  useLayer('search', true, () => setQuery(''), () => query.length > 0);
  // Searching mounts every pane (their rows are what gets searched), and each pane loads its own
  // data, so wait for a pause in typing before starting.
  const [applied, setApplied] = useState('');
  useEffect(() => {
    if (query.trim() === '') return setApplied('');
    const t = setTimeout(() => setApplied(query), 250);
    return () => clearTimeout(t);
  }, [query]);
  const searching = applied.trim().length > 0;
  const bodyRef = useRef<HTMLDivElement>(null);
  const [hits, setHits] = useState(0);
  const { groups, activeGroup, current } = settingsTabStrip(panes, group, tab);
  const version = useAppVersion();
  // Filter the rendered rows after every render and whenever a pane changes its own content
  // (data arriving late), so a search never shows a stale list.
  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    if (!searching) {
      applySearch(body, '');
      clearHighlight();
      return;
    }
    const run = () => setHits(applySearch(body, applied));
    run();
    let frame = 0;
    const watch = new MutationObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(run);
    });
    watch.observe(body, { childList: true, subtree: true, characterData: true });
    return () => {
      watch.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [applied, searching, panes]);
  useEffect(() => clearHighlight, []);
  // A soft fade on whichever edge of the scrolling pane has more rows beyond it (see styles.css).
  useEffect(() => {
    const body = bodyRef.current;
    const scroller = body?.parentElement;
    if (!body || !scroller) return;
    const update = () => {
      const edges = scrollEdges(scroller.scrollTop, scroller.clientHeight, scroller.scrollHeight);
      scroller.toggleAttribute('data-fade-top', edges.top);
      scroller.toggleAttribute('data-fade-bottom', edges.bottom);
    };
    update();
    scroller.addEventListener('scroll', update, { passive: true });
    const watch = new ResizeObserver(update);
    watch.observe(scroller);
    watch.observe(body);
    return () => {
      scroller.removeEventListener('scroll', update);
      watch.disconnect();
    };
  }, []);
  const footer = (
    <>
      <span className="settings-version" data-testid="settings-version">
        Helm <span className="muted">{version ?? ''}</span>
      </span>
      <span className="settings-foot-note">Runs Claude Code under your own login</span>
    </>
  );
  const picking = query.trim() === '';
  const tabs = (
    <div className="settings-tabs">
      <div className="settings-tab-row" role="tablist" aria-label="Settings groups">
        {groups.map((g) => (
          <button
            key={g.title}
            type="button"
            role="tab"
            className="settings-tab settings-group-tab"
            aria-selected={picking && g.title === activeGroup?.title}
            data-group={g.title}
            data-testid="settings-group"
            onClick={() => {
              setQuery('');
              setApplied('');
              setGroup(g.title);
              setTab(g.panes[0]?.id ?? null);
            }}
          >
            {g.title}
          </button>
        ))}
        <input
          type="search"
          className="settings-search"
          placeholder="Search settings"
          aria-label="Search settings"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          data-autofocus=""
        />
      </div>
      <div className="settings-tab-row" role="tablist" aria-label="Settings panes">
        {(activeGroup?.panes ?? []).map((p) => (
          <button
            key={p.id}
            type="button"
            role="tab"
            className="settings-tab"
            aria-selected={picking && p.id === current?.id}
            data-tab={p.id}
            onClick={() => {
              setQuery('');
              setApplied('');
              setTab(p.id);
            }}
          >
            {p.title}
          </button>
        ))}
      </div>
    </div>
  );
  return (
    <Dialog title="Settings" onClose={close} wide testId="settings" footer={footer} intro={tabs}>
      <div className="settings-body" ref={bodyRef}>
        {panes.length === 0 && <p className="muted">No settings yet.</p>}
        {searching && hits === 0 && (
          <div className="settings-nomatch" data-testid="settings-nomatch">
            <IconSearch size={22} />
            <span>No settings match "{applied.trim()}"</span>
          </div>
        )}
        {searching
          ? panes.map((p) => (
              <section key={p.id} data-testid="settings-pane" data-pane={p.id} data-title={p.title}>
                <h4 className="settings-pane-label">{p.title}</h4>
                {p.render()}
              </section>
            ))
          : current && (
              <section data-testid="settings-pane" data-pane={current.id} data-title={current.title}>
                {current.render()}
              </section>
            )}
      </div>
    </Dialog>
  );
}

/** The app's version from the host's `app_info`; null until it answers. */
function useAppVersion(): string | null {
  const shell = useShell();
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    shell
      .api<{ version?: unknown }>('app_info')
      .then((i) => live && setVersion(typeof i?.version === 'string' ? i.version : null))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [shell]);
  return version;
}
