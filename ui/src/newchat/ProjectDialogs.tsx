// The New project form (Ctrl+O: a new board project with its folder), the Add folder dialog
// (Settings → Projects → Folders) and the Primary folder… dialog. All are drawn by `NewChatHost`
// from the controller's `dialog`.

import { useState } from 'react';
import { ApiError } from '../api/client.ts';
import type { ProjectNode } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { Button, Dialog, Segmented, TextArea, TextInput } from '../ui/index.ts';
import { pickFolder } from './api.ts';
import { createBoardProject } from './boards.ts';
import { newChat } from './controller.ts';
import { folderName } from './places.ts';
import { addExistingFolder, addNewFolder, currentFolder, setPrimaryFolder } from './projects.ts';

const message = (e: unknown) => (e instanceof ApiError || e instanceof Error ? e.message : String(e));

/**
 * New project: a new project on the task board (a title and one plain description), with the folder
 * its chats start in, so the board arrives ready for chats. Then a chat in it, unless opened from
 * Settings.
 */
export function NewProjectDialog() {
  const shell = useShell();
  const d = newChat.dialog?.kind === 'new-project' ? newChat.dialog : null;
  const [title, setTitle] = useState(d?.title ?? '');
  const [description, setDescription] = useState('');
  const [folder, setFolder] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => newChat.closeDialog();
  const ready = title.trim().length > 0;

  const submit = async () => {
    if (!ready || busy) return;
    setBusy(true);
    setError(null);
    try {
      const node = await createBoardProject(shell.api, { title, description, folder });
      close();
      if (!d?.noChat) newChat.show({ projectId: node.id });
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title="New project"
      onClose={close}
      busy={busy}
      testId="new-project"
      intro="A new project on the task board. Its chats start in the folder you choose; you can set or change it later."
      footer={
        <>
          <Button variant="text" onClick={close} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void submit()} disabled={!ready || busy} data-testid="new-project-submit">
            Create project
          </Button>
        </>
      }
    >
      <form
        className="nc-form"
        onSubmit={(e) => e.preventDefault()}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.target as HTMLElement).tagName === 'INPUT') {
            e.preventDefault();
            void submit();
          }
        }}
      >
        <label className="nc-field">
          <span>Title</span>
          <TextInput value={title} onChange={(e) => setTitle(e.target.value)} placeholder="What the project is" maxLength={200} autoFocus data-testid="new-project-title" />
        </label>
        <label className="nc-field">
          <span>Description</span>
          <TextArea value={description} onChange={(e) => setDescription(e.target.value)} placeholder="One plain paragraph: what it is for and what done looks like" rows={4} data-testid="new-project-description" />
        </label>
        <label className="nc-field">
          <span>Folder</span>
          <span className="nc-row">
            <TextInput value={folder} onChange={(e) => setFolder(e.target.value)} placeholder="C:\path\to\folder" spellCheck={false} data-testid="new-project-folder" />
            <Button
              onClick={async () => {
                try {
                  const p = await pickFolder(shell.api, { title: 'Where this project’s chats start', startIn: folder.trim() || undefined });
                  if (p) setFolder(p);
                } catch (e) {
                  setError(`Could not open the folder dialog: ${message(e)}`);
                }
              }}
            >
              Choose…
            </Button>
          </span>
        </label>
        {error && (
          <p className="nc-error" role="alert" data-testid="new-project-error">
            {error}
          </p>
        )}
      </form>
    </Dialog>
  );
}

/** Add folder: a folder chats can run in (an existing one, or a new one made by name). Folders are
 *  not where chats are shown, so nothing opens after. */
export function AddProjectDialog() {
  const shell = useShell();
  // Opened with a name: straight to the New folder tab, name filled in.
  const preset = newChat.dialog?.kind === 'add-project' ? newChat.dialog.newName : undefined;
  const [mode, setMode] = useState<'existing' | 'new'>(preset !== undefined ? 'new' : 'existing');
  const [path, setPath] = useState('');
  const [name, setName] = useState(preset ?? '');
  const [nameEdited, setNameEdited] = useState(false);
  const [parent, setParent] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => newChat.closeDialog();

  const shownName = mode === 'existing' && !nameEdited ? (path.trim() ? folderName(path.trim()) : '') : name;
  const ready = mode === 'existing' ? path.trim().length > 0 : name.trim().length > 0;

  const choose = async (title: string, startIn: string | undefined, then: (p: string) => void) => {
    try {
      const p = await pickFolder(shell.api, { title, startIn });
      if (p) then(p);
    } catch (e) {
      setError(`Could not open the folder dialog: ${message(e)}`);
    }
  };

  const submit = async () => {
    if (!ready || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (mode === 'existing') await addExistingFolder(shell.api, path, shownName);
      else await addNewFolder(shell.api, name, parent);
      close();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title="Add folder"
      onClose={close}
      busy={busy}
      testId="add-project"
      footer={
        <>
          <Button variant="text" onClick={close} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void submit()} disabled={!ready || busy} data-testid="add-project-submit">
            {mode === 'existing' ? 'Add folder' : 'Create folder'}
          </Button>
        </>
      }
    >
      <form
        className="nc-form"
        onSubmit={(e) => e.preventDefault()}
        onKeyDown={(e) => {
          // Enter in a field submits (the kit draws a hidden submit button, so there is none).
          if (e.key === 'Enter' && (e.target as HTMLElement).tagName === 'INPUT') {
            e.preventDefault();
            void submit();
          }
        }}
      >
        <Segmented
          value={mode}
          onChange={(m) => {
            setMode(m);
            setError(null);
          }}
          label="Add"
          testId="add-project-mode"
          options={[
            { value: 'existing', label: 'Existing folder' },
            { value: 'new', label: 'New folder' },
          ]}
        />
        {mode === 'existing' ? (
          <>
            <label className="nc-field">
              <span>Folder</span>
              <span className="nc-row">
                <TextInput value={path} onChange={(e) => setPath(e.target.value)} placeholder="C:\path\to\folder" spellCheck={false} autoFocus data-testid="add-project-path" />
                <Button onClick={() => void choose('Add a folder', path.trim() || undefined, setPath)}>Choose…</Button>
              </span>
            </label>
            <label className="nc-field">
              <span>Name</span>
              <TextInput
                value={shownName}
                onChange={(e) => {
                  setNameEdited(true);
                  setName(e.target.value);
                }}
                placeholder="The folder’s name"
                data-testid="add-project-name"
              />
            </label>
          </>
        ) : (
          <>
            <label className="nc-field">
              <span>Name</span>
              <TextInput value={name} onChange={(e) => setName(e.target.value)} placeholder="my-new-project" spellCheck={false} autoFocus data-testid="add-project-name" />
            </label>
            <div className="nc-field">
              <span>Created in</span>
              <span className="nc-row">
                <span className="nc-parent" title={parent ?? undefined}>
                  {parent ?? 'Helm Projects, in your home folder'}
                </span>
                <Button onClick={() => void choose('Make the new folder in', parent ?? undefined, setParent)}>Change…</Button>
              </span>
            </div>
          </>
        )}
        {error && (
          <p className="nc-error" role="alert" data-testid="add-project-error">
            {error}
          </p>
        )}
      </form>
    </Dialog>
  );
}

export function PrimaryFolderDialog({ project }: { project: ProjectNode }) {
  const shell = useShell();
  const [path, setPath] = useState(() => currentFolder(project, newChat.projects) ?? '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => newChat.closeDialog();

  const save = async () => {
    if (!path.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await setPrimaryFolder(shell.api, project, path, newChat.projects);
      close();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title="Primary folder"
      onClose={close}
      busy={busy}
      testId="primary-folder"
      intro={`Where new chats in ${project.name} start. Nothing is moved on disk, and existing chats pick up the new folder on their next turn.`}
      footer={
        <>
          <Button variant="text" onClick={close} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void save()} disabled={!path.trim() || busy} data-testid="primary-folder-save">
            Save
          </Button>
        </>
      }
    >
      <form
        className="nc-form"
        onSubmit={(e) => e.preventDefault()}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.target as HTMLElement).tagName === 'INPUT') {
            e.preventDefault();
            void save();
          }
        }}
      >
        <label className="nc-field">
          <span>Folder</span>
          <span className="nc-row">
            <TextInput value={path} onChange={(e) => setPath(e.target.value)} placeholder="C:\path\to\folder" spellCheck={false} autoFocus data-testid="primary-folder-path" />
            <Button
              onClick={async () => {
                try {
                  const p = await pickFolder(shell.api, { title: `Primary folder for ${project.name}`, startIn: path.trim() || undefined });
                  if (p) setPath(p);
                } catch (e) {
                  setError(`Could not open the folder dialog: ${message(e)}`);
                }
              }}
            >
              Choose…
            </Button>
          </span>
        </label>
        {error && (
          <p className="nc-error" role="alert" data-testid="primary-folder-error">
            {error}
          </p>
        )}
      </form>
    </Dialog>
  );
}
