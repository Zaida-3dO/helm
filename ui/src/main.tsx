import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App.tsx';
import { createApi, type Api } from './api/client.ts';
import { EventBus } from './api/events.ts';
import { Flushers, installQuitHandling } from './shell/quit.ts';
import { installContextMenuPolicy } from './safety/contextMenu.ts';
import { installErrorReporting } from './safety/report.ts';
import './styles.css';
import './ui/ui.css';
import './ui/status/status.css';
import './chat.css';

async function boot(): Promise<{ api: Api; bus: EventBus }> {
  // Outside Tauri (vite dev in a plain browser) the dev fake host stands in. The whole branch is
  // dropped from production builds, where import.meta.env.DEV is false.
  if (import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)) {
    const { createFakeHost } = await import('./dev/fakeHost.ts');
    const bus = new EventBus();
    const seed = Number(new URLSearchParams(location.search).get('seed') ?? 0);
    const api = createApi(createFakeHost((p) => bus.dispatch(p), { seedRows: seed || undefined }));
    return { api, bus };
  }
  const tauri = await import('./api/tauri.ts');
  void tauri.connectEvents();
  return { api: tauri.api, bus: tauri.bus };
}

void boot().then(async ({ api, bus }) => {
  // The desktop pet's window shows its own small page instead of the app.
  if (new URLSearchParams(location.search).get('window') === 'pet') {
    const { PetApp } = await import('./pet/PetApp.tsx');
    const petRoot = document.getElementById('root');
    if (petRoot) createRoot(petRoot).render(<PetApp api={api} bus={bus} />);
    return;
  }
  const flushers = new Flushers();
  installErrorReporting(api);
  installContextMenuPolicy(document, import.meta.env.DEV);
  installQuitHandling(bus, api, flushers);
  const root = document.getElementById('root');
  if (root) {
    createRoot(root).render(
      <StrictMode>
        <App api={api} bus={bus} flushers={flushers} />
      </StrictMode>,
    );
  }
});
