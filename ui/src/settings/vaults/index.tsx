// Settings → Vaults: the vaults Obsidian knows (read only) plus folders added here,
// and which one is the default for note links.

import { useEffect, useMemo, useState } from 'react';
import type { VaultInfo } from '../../api/vault.ts';
import { vaultOps } from '../../api/vault.ts';
import { useShell } from '../../shell/context.ts';
import { Button, SettingRow, TextInput } from '../../ui/index.ts';
import { registerSettingsPane } from '../registry.ts';

function VaultsPane() {
  const shell = useShell();
  const ops = useMemo(() => vaultOps(shell.api), [shell.api]);
  const [vaults, setVaults] = useState<VaultInfo[] | null>(null);
  const [path, setPath] = useState('');
  const [error, setError] = useState<string | null>(null);
  const run = (p: Promise<VaultInfo[]>) =>
    p.then((v) => {
      setVaults(v);
      setError(null);
    }).catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));

  useEffect(() => void run(ops.list()), [ops]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="vaults-pane" data-testid="settings-vaults">
      <SettingRow title="Vaults" description="Notes from these Obsidian vaults open in the side panel when a chat links to them. Vaults Obsidian knows about are listed automatically.">
        {null}
      </SettingRow>
      {vaults === null && !error && <div className="setting-desc">Loading…</div>}
      {vaults?.length === 0 && <div className="setting-desc">No vaults yet. Add a vault folder below.</div>}
      {vaults?.map((v) => (
        <SettingRow key={v.path} testId="vault-row" title={v.name} description={`${v.path}${v.source === 'obsidian' ? ' · from Obsidian' : ''}`}>
          {v.isDefault ? (
            <span className="setting-desc">Default</span>
          ) : (
            <Button variant="text" onClick={() => void run(ops.setDefault(v.name))}>
              Make default
            </Button>
          )}
          {v.source === 'user' && (
            <Button variant="text" danger onClick={() => void run(ops.remove(v.path))}>
              Remove
            </Button>
          )}
        </SettingRow>
      ))}
      <SettingRow title="Add a vault" description="The full path of a vault folder.">
        <TextInput value={path} placeholder="C:\Notes\My Vault" aria-label="Vault folder" onChange={(e) => setPath(e.target.value)} />
        <Button
          variant="secondary"
          disabled={!path.trim()}
          onClick={() =>
            void run(ops.add(path.trim())).then(() => {
              setPath('');
            })
          }
        >
          Add
        </Button>
      </SettingRow>
      {error && (
        <div className="ui-field-error" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}

registerSettingsPane({ id: 'vaults', title: 'Vaults', order: 40, render: () => <VaultsPane /> });
