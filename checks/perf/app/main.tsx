// The perf bench's page: the real app, built for production (no StrictMode, no
// React development checks), wired to the in-memory fake host instead of Tauri.
import { createRoot } from 'react-dom/client';
import { App } from '../../../ui/src/App.tsx';
import { createApi } from '../../../ui/src/api/client.ts';
import { EventBus } from '../../../ui/src/api/events.ts';
import { createFakeHost } from '../../../ui/src/dev/fakeHost.ts';
import { Flushers, installQuitHandling } from '../../../ui/src/shell/quit.ts';
import '../../../ui/src/styles.css';
import '../../../ui/src/ui/ui.css';
import '../../../ui/src/ui/status/status.css';
import '../../../ui/src/chat.css';

const bus = new EventBus();
const seed = Number(new URLSearchParams(location.search).get('seed') ?? 0);
const api = createApi(createFakeHost((p) => bus.dispatch(p), { seedRows: seed || undefined }));
const flushers = new Flushers();
installQuitHandling(bus, api, flushers);
const root = document.getElementById('root');
if (root) createRoot(root).render(<App api={api} bus={bus} flushers={flushers} />);
