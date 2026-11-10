// A project's icon wherever one is drawn: its emoji, a library line icon, its own small picture, or
// the default board or folder glyph. Also the one place that opens the icon picker.

import { useEffect, useState, useSyncExternalStore, type ReactElement } from 'react';
import type { Api } from '../api/client.ts';
import type { ProjectNode } from '../api/projects.ts';
import { projectOps } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { IconBoard, IconFolder } from '../ui/icons.tsx';
import { IconPicker } from './IconPicker.tsx';
import { LibraryIcon, isLibraryName } from './iconLibrary.tsx';
import { iconChoice, libName } from './iconImage.ts';
import './projects.css';

export type ProjectIconSource = Pick<ProjectNode, 'id' | 'kind' | 'icon' | 'revision'>;

// ---- Pictures: loaded once per project revision ------------------------------------------------

const MAX_CACHED = 200;
/** `${id}:${revision}` -> data URL, or null when the host had no picture or failed. */
const pictures = new Map<string, string | null>();
const loading = new Map<string, Promise<string | null>>();

function loadPicture(api: Api, id: string, key: string): Promise<string | null> {
  const running = loading.get(key);
  if (running) return running;
  const p = projectOps(api)
    .iconImageGet(id)
    .then((r) => (r?.pngBase64 ? `data:image/png;base64,${r.pngBase64}` : null))
    .catch(() => null)
    .then((url) => {
      // A newer revision replaces older pictures of the same project.
      for (const k of [...pictures.keys()]) if (k.startsWith(`${id}:`)) pictures.delete(k);
      pictures.set(key, url);
      while (pictures.size > MAX_CACHED) {
        const oldest = pictures.keys().next().value;
        if (oldest === undefined) break;
        pictures.delete(oldest);
      }
      loading.delete(key);
      return url;
    });
  loading.set(key, p);
  return p;
}

function PictureIcon({ id, revision, size, fallback }: { id: string; revision: number; size: number; fallback: ReactElement }) {
  const shell = useShell();
  const key = `${id}:${revision}`;
  const [url, setUrl] = useState<string | null>(() => pictures.get(key) ?? null);
  useEffect(() => {
    const hit = pictures.get(key);
    if (hit !== undefined) {
      setUrl(hit);
      return;
    }
    let live = true;
    setUrl(null);
    void loadPicture(shell.api, id, key).then((u) => {
      if (live) setUrl(u);
    });
    return () => {
      live = false;
    };
  }, [shell.api, id, key]);
  if (!url) return fallback;
  return <img className="pi-picture" src={url} width={size} height={size} alt="" draggable={false} />;
}

/** The icon of a project: `size` is in pixels. */
export function ProjectIcon({ project, size = 16 }: { project: ProjectIconSource; size?: number }) {
  const glyph = project.kind === 'board' ? <IconBoard size={size} strokeWidth={1.8} /> : <IconFolder size={size} strokeWidth={1.8} />;
  switch (iconChoice(project.icon)) {
    case 'emoji':
      return (
        <span className="pi-emoji" style={{ fontSize: size }} aria-hidden="true">
          {project.icon}
        </span>
      );
    case 'lib': {
      const name = libName(project.icon);
      return name && isLibraryName(name) ? <LibraryIcon name={name} size={size} /> : glyph;
    }
    case 'img':
      return <PictureIcon id={project.id} revision={project.revision} size={size} fallback={glyph} />;
    default:
      return glyph;
  }
}

// ---- Opening the picker --------------------------------------------------------------------------

let openFor: string | null = null;
const listeners = new Set<() => void>();

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

function setOpen(projectId: string | null): void {
  if (openFor === projectId) return;
  openFor = projectId;
  for (const l of [...listeners]) l();
}

/** Open the icon picker for a project (its menu's Change icon). */
export function openIconPicker(projectId: string): void {
  setOpen(projectId);
}

/** Draws the picker while it is open. Mounted once, by the app frame. */
export function IconPickerHost() {
  const projectId = useSyncExternalStore(subscribe, () => openFor);
  if (!projectId) return null;
  return <IconPicker key={projectId} projectId={projectId} onClose={() => setOpen(null)} />;
}
