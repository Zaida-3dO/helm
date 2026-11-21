// Under a secret in the server editor: "Use a different value for <profile>". A chat of that
// profile reads its own value; every other chat reads the shared one above. Like the shared
// value it is write-only: only a saved value's presence shows, never its text.

import { useState } from 'react';
import type { ValueRef } from '../../api/mcp.ts';
import { useProfiles } from '../../profiles/index.ts';
import { useShell } from '../../shell/context.ts';
import { Button, TextInput } from '../../ui/index.ts';
import { withProfileValue } from './readiness.ts';

export function ProfileValues({ name, value, onChange }: { name: string; value: ValueRef; onChange: (r: ValueRef) => void }) {
  const shell = useShell();
  const { snap } = useProfiles(shell.api, shell.bus);
  const [adding, setAdding] = useState('');
  const known = snap.view?.profiles ?? [];
  const own = Object.entries(value.profiles ?? {});
  const free = known.filter((p) => !value.profiles?.[p.id]);
  // With one profile there is nobody to give a different value to.
  if (known.length < 2 && own.length === 0) return null;
  return (
    <div className="mcp-profile-values" data-testid={`mcp-profile-values-${name}`}>
      {own.map(([id, p]) => (
        <div className="mcp-profile-value" key={id} data-testid={`mcp-profile-value-${name}-${id}`}>
          <span className="mcp-help">{known.find((k) => k.id === id)?.name ?? 'A removed profile'}</span>
          <TextInput
            aria-label={`Value for ${known.find((k) => k.id === id)?.name ?? id}`}
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={p?.value ?? ''}
            placeholder={p?.secretId ? 'Saved; type to replace' : 'Not set'}
            onChange={(e) => onChange(withProfileValue(value, id, e.target.value))}
          />
          <Button variant="text" danger onClick={() => onChange(withProfileValue(value, id, null))}>
            Remove
          </Button>
        </div>
      ))}
      {free.length > 0 && (
        <div className="mcp-profile-add">
          <select
            className="ui-input"
            aria-label="Use a different value for"
            data-testid={`mcp-profile-add-${name}`}
            value={adding}
            onChange={(e) => {
              const id = e.target.value;
              setAdding('');
              if (id) onChange(withProfileValue(value, id, ''));
            }}
          >
            <option value="">Use a different value for…</option>
            {free.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </div>
      )}
    </div>
  );
}
