// Typed wrapper over the backup, restore and export ops. Every call still goes through api(op, args).

import type { Api } from './client.ts';
import type { BackupList, Created, Exported, RestoreStaged } from './generated/backup/index.ts';

export type { BackupEntry, BackupList, Created, Exported, RestoreStaged } from './generated/backup/index.ts';

/** What the host demands before it stages a restore; the confirm button shows it. */
export const RESTORE_CONFIRM = 'replace chats and settings';

export function backupOps(api: Api) {
  return {
    /** Write a new archive (to the default Documents folder unless `destDir` is given). */
    create: (destDir?: string) => api<Created>('backup_create', destDir ? { destDir } : {}),
    list: (dir?: string) => api<BackupList>('backup_list', dir ? { dir } : {}),
    /** Queue a restore for the next start. Nothing changes until Helm restarts. */
    restore: (archiveDir: string, confirm: string) => api<RestoreStaged>('restore_prepare', { archiveDir, confirm }),
    /** One chat as a Markdown file under Documents (or `destDir`). */
    exportChat: (chatId: string, destDir?: string) => api<Exported>('chat_export_markdown', destDir ? { chatId, destDir } : { chatId }),
  };
}

export type BackupOps = ReturnType<typeof backupOps>;
