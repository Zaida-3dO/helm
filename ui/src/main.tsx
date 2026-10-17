import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App.tsx';
import { api, bus, connectEvents } from './api/tauri.ts';
import { Flushers, installQuitHandling } from './shell/quit.ts';
import './styles.css';

const flushers = new Flushers();
installQuitHandling(bus, api, flushers);
void connectEvents();

const root = document.getElementById('root');
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App api={api} />
    </StrictMode>,
  );
}
