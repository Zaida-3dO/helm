// Obsidian vault ops (host/crates/panel vault.rs).

import type { Api } from './client.ts';

export interface VaultInfo {
  name: string;
  path: string;
  source: 'obsidian' | 'user';
  isDefault: boolean;
}

export type VaultResolution = { vault: string; path: string } | { vault: string; candidates: string[] };

export interface VaultDoc {
  vault: string;
  path: string;
  title: string;
  fullPath: string;
  text?: string;
  /** Base64 image bytes (embeds). */
  bytes?: string;
  mime?: string;
  truncated: boolean;
}

export interface ResolveArgs {
  chatId?: string | null;
  note: string;
  vault?: string;
  fromVault?: string;
  fromPath?: string;
}

export function vaultOps(api: Api) {
  return {
    list: () => api<VaultInfo[]>('vault_list', {}),
    add: (path: string) => api<VaultInfo[]>('vault_add', { path }),
    remove: (path: string) => api<VaultInfo[]>('vault_remove', { path }),
    setDefault: (name: string) => api<VaultInfo[]>('vault_set_default', { name }),
    resolve: (a: ResolveArgs) => {
      const args: Record<string, unknown> = { note: a.note };
      if (a.chatId) args.chatId = a.chatId;
      if (a.vault) args.vault = a.vault;
      if (a.fromVault) args.fromVault = a.fromVault;
      if (a.fromPath) args.fromPath = a.fromPath;
      return api<VaultResolution>('vault_resolve', args);
    },
    read: (vault: string, path: string) => api<VaultDoc>('vault_read', { vault, path }),
    openInObsidian: (vault: string, path: string) => api<null>('vault_open_in_obsidian', { vault, path }),
  };
}

export type VaultOps = ReturnType<typeof vaultOps>;
