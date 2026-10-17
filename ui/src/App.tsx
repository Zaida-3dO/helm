import { useEffect, useState } from 'react';
import type { Api } from './api/client.ts';

type AppInfo = {
  name: string;
  edition: string;
  version: string;
  schemaVersion: number;
  profileDir: string;
};

type Load = { state: 'loading' } | { state: 'ready'; info: AppInfo } | { state: 'error'; message: string };

/** A frame with the sidebar and main area that later versions fill in. */
export function App({ api }: { api: Api }) {
  const [load, setLoad] = useState<Load>({ state: 'loading' });

  useEffect(() => {
    let live = true;
    api<AppInfo>('app_info')
      .then((info) => live && setLoad({ state: 'ready', info }))
      .catch((e: unknown) => live && setLoad({ state: 'error', message: String((e as Error).message ?? e) }));
    return () => {
      live = false;
    };
  }, [api]);

  const quit = () => {
    void api('quit_request');
  };

  return (
    <div className="shell">
      <aside className="sidebar" aria-label="Sidebar">
        <div className="brand">Helm</div>
        <p className="muted">Projects and chats arrive later.</p>
      </aside>
      <main className="main">
        <header className="topbar">
          <h1>Workspace</h1>
          <button type="button" onClick={quit}>
            Quit
          </button>
        </header>
        <section className="card" data-testid="status">
          {load.state === 'loading' && <p>Connecting to the host…</p>}
          {load.state === 'error' && <p className="error">Host unreachable: {load.message}</p>}
          {load.state === 'ready' && (
            <dl>
              <dt>Host</dt>
              <dd data-testid="host-ok">
                {load.info.name} {load.info.version} ({load.info.edition} edition)
              </dd>
              <dt>Schema</dt>
              <dd>{load.info.schemaVersion}</dd>
              <dt>Profile</dt>
              <dd className="mono">{load.info.profileDir}</dd>
            </dl>
          )}
        </section>
      </main>
    </div>
  );
}
