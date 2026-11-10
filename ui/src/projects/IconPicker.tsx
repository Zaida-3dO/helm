// The project icon picker: a library icon, an emoji or a picture of your own, and a colour. Every
// choice is saved at once; the sidebar follows through the host's project event.

import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';
import { ApiError } from '../api/client.ts';
import { projectOps, type ProjectNode } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { Button, Dialog, TextInput } from '../ui/index.ts';
import { EMOJI, firstEmoji, searchEmoji } from './emoji.ts';
import { ICON_LIBRARY, LibraryIcon } from './iconLibrary.tsx';
import { ACCEPTED_TYPES, ICON_PX, PNG_MAX_BYTES, base64Bytes, imageProblem, squareCrop } from './iconImage.ts';
import './projects.css';

type Tab = 'library' | 'emoji' | 'image';
const TABS: { id: Tab; label: string }[] = [
  { id: 'library', label: 'Library' },
  { id: 'emoji', label: 'Emoji' },
  { id: 'image', label: 'Image' },
];

const COLOURS = ['red', 'orange', 'yellow', 'green', 'teal', 'blue', 'purple', 'pink'];
const HEX = /^#[0-9a-fA-F]{6}$/;
const PREVIEW_PX = 160;
const MAX_ZOOM = 4;

const plain = (e: unknown): string => (e instanceof ApiError || e instanceof Error ? e.message : String(e));

export function IconPicker({ projectId, onClose }: { projectId: string; onClose: () => void }) {
  const shell = useShell();
  const ops = useMemo(() => projectOps(shell.api), [shell.api]);
  const [tab, setTab] = useState<Tab>('library');
  const [node, setNode] = useState<ProjectNode | null>(null);
  const nodeRef = useRef<ProjectNode | null>(null);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [hex, setHex] = useState('');

  const adopt = (n: ProjectNode) => {
    nodeRef.current = n;
    setNode(n);
  };

  /** The freshest copy of the project (archived and closed ones included). */
  const reload = async (): Promise<ProjectNode> => {
    const n = (await ops.list(true, true)).find((x) => x.id === projectId);
    if (!n) throw new ApiError('not_found', 'That project no longer exists.');
    adopt(n);
    return n;
  };

  useEffect(() => {
    let live = true;
    void reload()
      .then((n) => {
        if (live && n.colour?.startsWith('#')) setHex(n.colour);
      })
      .catch((e) => {
        if (live) setError(plain(e));
      });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, ops]);

  /** Run one save against the freshest node; a revision conflict reloads it and tries once more. */
  const run = async (call: (n: ProjectNode) => Promise<ProjectNode>): Promise<boolean> => {
    if (busyRef.current) return false;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      let cur = nodeRef.current ?? (await reload());
      let next: ProjectNode;
      try {
        next = await call(cur);
      } catch (e) {
        if (!(e instanceof ApiError) || e.code !== 'conflict') throw e;
        cur = await reload();
        next = await call(cur);
      }
      adopt(next);
      return true;
    } catch (e) {
      setError(plain(e));
      return false;
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  const setIcon = (icon: string | null) => run((n) => ops.update({ projectId, revision: n.revision, icon }));
  const setColour = async (colour: string | null) => {
    const ok = await run((n) => ops.update({ projectId, revision: n.revision, colour }));
    if (ok && colour?.startsWith('#')) setHex(colour);
  };
  const applyHex = () => {
    const v = hex.trim();
    if (!HEX.test(v)) {
      setError('Use a colour like #4a90d9: a hash and six hex digits.');
      return;
    }
    void setColour(v.toLowerCase());
  };

  const move = (dir: 1 | -1) => {
    const i = TABS.findIndex((t) => t.id === tab);
    const next = TABS[(i + dir + TABS.length) % TABS.length];
    if (next) setTab(next.id);
  };

  return (
    <Dialog
      title="Project icon"
      onClose={onClose}
      busy={busy}
      wide
      testId="icon-picker"
      footer={
        <>
          <Button variant="text" onClick={() => void setIcon(null)} disabled={busy || !node?.icon} data-testid="icon-remove">
            Remove icon
          </Button>
          <Button variant="primary" onClick={onClose} disabled={busy} data-testid="icon-done">
            Done
          </Button>
        </>
      }
    >
      <div className="pi-picker">
        <div
          className="ui-tabs"
          role="tablist"
          aria-label="Kind of icon"
          onKeyDown={(e) => {
            if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
            e.preventDefault();
            move(e.key === 'ArrowRight' ? 1 : -1);
          }}
        >
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              className="ui-tab"
              aria-selected={t.id === tab}
              tabIndex={t.id === tab ? 0 : -1}
              data-tab={t.id}
              data-autofocus={t.id === tab ? '' : undefined}
              data-testid={`icon-tab-${t.id}`}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>

        <div className="pi-pane" role="tabpanel" aria-label={TABS.find((t) => t.id === tab)?.label}>
          {tab === 'library' && (
            <div className="pi-grid" data-testid="icon-library">
              {ICON_LIBRARY.map((e) => (
                <button
                  key={e.name}
                  type="button"
                  className="pi-cell"
                  aria-label={e.label}
                  title={e.label}
                  aria-pressed={node?.icon === `lib:${e.name}`}
                  disabled={busy}
                  data-testid={`icon-lib-${e.name}`}
                  onClick={() => void setIcon(`lib:${e.name}`)}
                >
                  <LibraryIcon name={e.name} size={20} />
                </button>
              ))}
            </div>
          )}
          {tab === 'emoji' && <EmojiPane current={node?.icon ?? null} busy={busy} onPick={(c) => void setIcon(c)} />}
          {tab === 'image' && (
            <ImagePane
              busy={busy}
              onProblem={setError}
              onSave={(b64) => run(() => ops.iconImageSet(projectId, b64))}
            />
          )}
        </div>

        <div className="pi-colours" role="group" aria-label="Colour">
          <span className="pi-colours-label">Colour</span>
          <button
            type="button"
            className="pi-swatch pi-swatch-none"
            aria-label="No colour"
            title="No colour"
            aria-pressed={!node?.colour}
            disabled={busy}
            data-testid="icon-colour-none"
            onClick={() => void setColour(null)}
          />
          {COLOURS.map((c) => (
            <button
              key={c}
              type="button"
              className="pi-swatch"
              style={{ background: `var(--project-${c})` }}
              aria-label={c}
              title={c}
              aria-pressed={node?.colour === c}
              disabled={busy}
              data-testid={`icon-colour-${c}`}
              onClick={() => void setColour(c)}
            />
          ))}
          <TextInput
            className="pi-hex"
            value={hex}
            onChange={(e) => setHex(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                applyHex();
              }
            }}
            placeholder="#4a90d9"
            aria-label="Custom colour"
            spellCheck={false}
            maxLength={7}
            data-testid="icon-colour-hex"
          />
          <Button onClick={applyHex} disabled={busy || !HEX.test(hex.trim())} data-testid="icon-colour-hex-use">
            Use
          </Button>
        </div>

        {error && (
          <p className="pi-error" role="alert" data-testid="icon-error">
            {error}
          </p>
        )}
      </div>
    </Dialog>
  );
}

function EmojiPane({ current, busy, onPick }: { current: string | null; busy: boolean; onPick: (char: string) => void }) {
  const [query, setQuery] = useState('');
  const [typed, setTyped] = useState('');
  const found = useMemo(() => searchEmoji(EMOJI, query), [query]);
  const typedEmoji = firstEmoji(typed);
  return (
    <div className="pi-emoji-pane">
      <TextInput
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder="Search emoji"
        aria-label="Search emoji"
        spellCheck={false}
        data-testid="icon-emoji-search"
      />
      <div className="pi-grid pi-grid-emoji">
        {found.map((e) => (
          <button
            key={e.char}
            type="button"
            className="pi-cell"
            aria-label={e.words}
            title={e.words}
            aria-pressed={current === e.char}
            disabled={busy}
            data-testid="icon-emoji"
            onClick={() => onPick(e.char)}
          >
            <span className="pi-emoji" style={{ fontSize: 20 }} aria-hidden="true">
              {e.char}
            </span>
          </button>
        ))}
        {found.length === 0 && <p className="pi-none">No emoji match that. Type or paste one below.</p>}
      </div>
      <div className="pi-type">
        <TextInput
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              if (typedEmoji && !busy) onPick(typedEmoji);
            }
          }}
          placeholder="Type any emoji"
          aria-label="Type any emoji"
          spellCheck={false}
          data-testid="icon-emoji-type"
        />
        <Button disabled={busy || !typedEmoji} onClick={() => typedEmoji && onPick(typedEmoji)} data-testid="icon-emoji-use">
          {typedEmoji ? `Use ${typedEmoji}` : 'Use'}
        </Button>
      </div>
    </div>
  );
}

interface Source {
  el: HTMLImageElement;
  w: number;
  h: number;
}

function ImagePane({ busy, onProblem, onSave }: { busy: boolean; onProblem: (message: string | null) => void; onSave: (pngBase64: string) => Promise<boolean> }) {
  const [source, setSource] = useState<Source | null>(null);
  const [zoom, setZoom] = useState(1);
  const [cx, setCx] = useState(0.5);
  const [cy, setCy] = useState(0.5);
  const canvas = useRef<HTMLCanvasElement>(null);
  const drag = useRef<{ x: number; y: number; cx: number; cy: number } | null>(null);

  useEffect(() => {
    const c = canvas.current;
    const ctx = c?.getContext('2d');
    if (!c || !ctx || !source) return;
    const { sx, sy, side } = squareCrop(source.w, source.h, zoom, cx, cy);
    ctx.clearRect(0, 0, c.width, c.height);
    ctx.drawImage(source.el, sx, sy, side, side, 0, 0, c.width, c.height);
  }, [source, zoom, cx, cy]);

  const onFile = async (file: File | undefined) => {
    if (!file) return;
    const problem = imageProblem(file);
    if (problem) {
      setSource(null);
      onProblem(problem);
      return;
    }
    onProblem(null);
    const url = URL.createObjectURL(file);
    try {
      // A GIF is read as its first frame.
      const el = new Image();
      el.src = url;
      await el.decode();
      if (!el.naturalWidth || !el.naturalHeight) throw new Error('empty');
      setSource({ el, w: el.naturalWidth, h: el.naturalHeight });
      setZoom(1);
      setCx(0.5);
      setCy(0.5);
    } catch {
      setSource(null);
      onProblem('That file could not be read as a picture.');
    } finally {
      URL.revokeObjectURL(url);
    }
  };

  const clamp01 = (v: number) => Math.min(1, Math.max(0, v));
  const onMove = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    if (!d || !source) return;
    const shown = e.currentTarget.getBoundingClientRect().width || PREVIEW_PX;
    const { side } = squareCrop(source.w, source.h, zoom, d.cx, d.cy);
    const k = side / shown; // picture pixels per screen pixel
    setCx(clamp01(d.cx - ((e.clientX - d.x) * k) / source.w));
    setCy(clamp01(d.cy - ((e.clientY - d.y) * k) / source.h));
  };

  const save = async () => {
    if (!source) return;
    const out = document.createElement('canvas');
    out.width = ICON_PX;
    out.height = ICON_PX;
    const ctx = out.getContext('2d');
    if (!ctx) {
      onProblem('This window cannot draw pictures.');
      return;
    }
    ctx.imageSmoothingQuality = 'high';
    const { sx, sy, side } = squareCrop(source.w, source.h, zoom, cx, cy);
    ctx.drawImage(source.el, sx, sy, side, side, 0, 0, ICON_PX, ICON_PX);
    const url = out.toDataURL('image/png');
    const b64 = url.slice(url.indexOf(',') + 1);
    if (base64Bytes(b64) > PNG_MAX_BYTES) {
      onProblem('That picture is too detailed; try a simpler one.');
      return;
    }
    await onSave(b64);
  };

  return (
    <div className="pi-image-pane">
      <label className="pi-file">
        <span>Choose a picture (PNG, JPEG, WebP or GIF, up to 5 MB)</span>
        <input
          type="file"
          className="pi-file-input"
          accept={ACCEPTED_TYPES.join(',')}
          disabled={busy}
          data-testid="icon-image-file"
          onChange={(e) => {
            const f = e.target.files?.[0];
            e.target.value = '';
            void onFile(f);
          }}
        />
      </label>
      {source && (
        <div className="pi-crop">
          <canvas
            ref={canvas}
            className="pi-preview"
            width={PREVIEW_PX}
            height={PREVIEW_PX}
            aria-label="Picture preview; drag to move it"
            data-testid="icon-image-preview"
            onPointerDown={(e) => {
              e.currentTarget.setPointerCapture(e.pointerId);
              drag.current = { x: e.clientX, y: e.clientY, cx, cy };
            }}
            onPointerMove={onMove}
            onPointerUp={() => {
              drag.current = null;
            }}
            onPointerCancel={() => {
              drag.current = null;
            }}
          />
          <div className="pi-crop-side">
            <label className="pi-zoom">
              <span>Zoom</span>
              <input
                type="range"
                min={1}
                max={MAX_ZOOM}
                step={0.05}
                value={zoom}
                onChange={(e) => setZoom(Number(e.target.value))}
                data-testid="icon-image-zoom"
              />
            </label>
            <p className="pi-hint">Drag the picture to choose which part shows.</p>
            <Button variant="primary" onClick={() => void save()} disabled={busy} data-testid="icon-image-save">
              Save picture
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
