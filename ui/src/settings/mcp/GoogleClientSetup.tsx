// Settings → MCP, inside the Google accounts dialog: the one-time sign-in client. A short
// click-through of the Google Cloud console steps, each opening its own console page in the
// system browser, then the two values the last step hands out. The secret is write-only: once
// saved it never comes back, only the knowledge that a client is set.

import { useState } from 'react';
import { Button, TextInput } from '../../ui/index.ts';
import { CONSOLE_STEPS, UNVERIFIED_NOTE, clientLooksSet } from './google.ts';

export function GoogleClientSetup({
  configured,
  busy,
  onSave,
  onOpenLink,
}: {
  /** Whether a client is already saved for this profile. The values themselves never appear. */
  configured: boolean | null;
  busy: boolean;
  onSave: (clientId: string, secret: string) => void;
  onOpenLink: (url: string) => void;
}) {
  const [clientId, setClientId] = useState('');
  const [secret, setSecret] = useState('');
  // Once saved, the form folds away; a change re-opens it over the saved client.
  const [changing, setChanging] = useState(false);
  const showForm = configured !== true || changing;
  return (
    <section className="google-client" data-testid="google-client" data-state={configured === null ? 'loading' : configured ? 'saved' : 'unset'}>
      <p className="mcp-help">
        Google needs a sign-in client of your own, made once on this account. Work through the six steps — each opens the Google page for it — and paste the
        last step’s two values back here.
      </p>
      <ol className="google-steps" data-testid="google-steps">
        {CONSOLE_STEPS.map((s, i) => (
          <li key={s.url} data-testid={`google-step-${i + 1}`}>
            <span>{s.label}</span>{' '}
            <Button variant="text" onClick={() => onOpenLink(s.url)}>
              Open the page
            </Button>
          </li>
        ))}
      </ol>
      {configured === true && !changing && (
        <p className="mcp-help" data-testid="google-client-saved">
          A sign-in client is saved. <Button variant="text" data-testid="google-client-change" onClick={() => setChanging(true)}>Change client</Button>
        </p>
      )}
      {showForm && (
        <div className="google-client-fields" data-testid="google-client-form">
          <label className="mcp-field">
            <span>Client id</span>
            <TextInput
              data-testid="google-client-id"
              spellCheck={false}
              autoComplete="off"
              placeholder="1234567890-abcdef.apps.googleusercontent.com"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
            />
          </label>
          <label className="mcp-field">
            <span>Client secret</span>
            <TextInput
              data-testid="google-client-secret"
              type="password"
              spellCheck={false}
              autoComplete="off"
              placeholder={configured === true ? 'Saved; type to replace' : 'GOCSPX-…'}
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && clientLooksSet(clientId, secret) && !busy) onSave(clientId, secret);
              }}
            />
          </label>
          <div className="mcp-actions">
            <Button variant="primary" data-testid="google-client-save" disabled={busy || !clientLooksSet(clientId, secret)} onClick={() => onSave(clientId, secret)}>
              Save client
            </Button>
            {changing && <Button onClick={() => setChanging(false)}>Never mind</Button>}
          </div>
        </div>
      )}
      <p className="mcp-help" data-testid="google-unverified-note">
        {UNVERIFIED_NOTE}
      </p>
    </section>
  );
}
