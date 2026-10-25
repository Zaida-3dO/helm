// Tool rows: one flat collapsed line per call (chevron, icon, verb,
// target, trailing status, Stop), opening to an output box, a clamped diff, or for host and MCP
// tools a key/value result grid. Identity comes from `details.toolSource` (see toolIdentity).

import { useEffect, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { useShell } from '../shell/context.ts';
import { IconCopy, IconWrap } from '../ui/icons.tsx';
import { Glyph, Spinner } from './icons.tsx';
import { parseResult, type Cell, type GridRow } from './resultGrid.ts';
import { TodoList } from './TodoList.tsx';
import { parseTodos } from './todos.ts';
import {
  changeLines,
  hostIconFor,
  isFileTool,
  readable,
  splitPath,
  toolIdentity,
  toolKind,
  toolStatus,
  toolTarget,
  toolVerb,
  type ToolKind,
  type ToolStatus,
} from './toolKinds.ts';
import { formatElapsed } from './workBlocks.ts';

export interface ToolCardProps {
  row: MessageRow;
  /** The Stop button shows only when the setting is on, the worker can stop one call, and this call is stoppable. */
  canStop: boolean;
  onStop: (toolCallId: string) => void;
}

function useElapsed(since: number, live: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [live]);
  return Math.max(0, now - since);
}

const STATUS_TITLE: Record<ToolStatus, string> = { running: 'Running', complete: 'Done', failed: 'Failed', interrupted: 'Stopped' };

export function ToolCard({ row, canStop, onStop }: ToolCardProps) {
  const kind = toolKind(row.toolName);
  const status = toolStatus(row);
  const id = toolIdentity(row);
  const target = toolTarget(row);
  const elapsed = useElapsed(row.timestamp, status === 'running');
  // A to-do update shows its checklist without a click.
  const [open, setOpen] = useState(() => row.toolName === 'TodoWrite');
  const rawName = row.toolName ?? '';

  const label = (() => {
    if (id.source === 'mcp') {
      return (
        <>
          <Glyph name="cable" className="tool-icon" />
          <span className="tool-server">{id.server}</span>
          <span className="tool-sep"> · </span>
          <span className="tool-action">{readable(id.tool)}</span>
          <span className="mcp-badge">MCP</span>
        </>
      );
    }
    if (id.source === 'host') {
      return (
        <>
          <Glyph name={hostIconFor(id.tool)} className="tool-icon tool-icon-host" />
          <span className="tool-host-action">{readable(id.tool)}</span>
          {target && (
            <>
              <span className="tool-sep"> · </span>
              <span className="tool-detail">{target}</span>
            </>
          )}
        </>
      );
    }
    const verb = toolVerb(row, status);
    const icon = <Glyph name={kind === 'shell' ? 'terminal' : kind === 'web' ? 'globe' : 'file-code'} className="tool-icon" />;
    if (target && isFileTool(kind)) {
      const { name, dir } = splitPath(target);
      return (
        <>
          {icon}
          <span className="tool-verb">{verb}</span>
          <span className="tool-filename">{name}</span>
          {dir && (
            <span className="tool-dir">
              <bdi>{dir}</bdi>
            </span>
          )}
        </>
      );
    }
    return (
      <>
        {icon}
        <span className="tool-verb">{verb}</span>
        {target && <span className="tool-target mono">{target.replace(/\r?\n/g, ' ↵ ')}</span>}
      </>
    );
  })();

  const title = id.source === 'native' ? (target ?? rawName) : `${id.source === 'host' ? 'Helm' : id.server} · ${readable(id.tool)} (${rawName})`;

  return (
    <div
      className={`tool-card tool-${kind} status-${status} source-${id.source}${open ? ' open' : ''}`}
      data-testid="tool-card"
      data-tool={rawName}
      data-status={status}
      data-source={id.source}
    >
      <div className="tool-summary">
        <button type="button" className="tool-toggle" aria-expanded={open} title={title} onClick={() => setOpen((o) => !o)}>
          <Glyph name="chevron" size={13} className="tool-chevron" />
          {label}
        </button>
        <span className={`tool-trail ${status}`} title={STATUS_TITLE[status]} data-testid="tool-status">
          {status === 'running' && <Spinner />}
          {status === 'failed' && <Glyph name="alert" size={12} />}
          {status === 'interrupted' && <Glyph name="pause" size={12} />}
          {status === 'complete' && id.source !== 'native' && <Glyph name="check" size={12} />}
        </span>
        {status === 'running' && <span className="tool-elapsed muted">{formatElapsed(elapsed)}</span>}
        {status === 'running' && canStop && row.toolCallId && (
          <button
            type="button"
            className="tool-stop"
            data-testid="tool-stop"
            title="Fail just this call; the task continues"
            onClick={() => onStop(row.toolCallId!)}
          >
            <span className="tool-stop-square" />
          </button>
        )}
      </div>
      {open && <ToolBody row={row} kind={kind} status={status} source={id.source} rawName={rawName} />}
    </div>
  );
}

function ToolBody({ row, kind, status, source, rawName }: { row: MessageRow; kind: ToolKind; status: ToolStatus; source: 'host' | 'mcp' | 'native'; rawName: string }) {
  if (kind === 'edit' && source === 'native') return <ChangeView row={row} />;
  const todos = source === 'native' ? parseTodos(row) : null;
  if (todos) return <TodoList todos={todos} />;
  const output = row.text;
  const grid = source !== 'native' && output ? parseResult(output) : null;
  const header = status === 'failed' ? 'Error' : source !== 'native' ? 'Result' : rawName === 'Read' ? 'File contents' : 'Output';
  return (
    <div className="tool-box" data-testid="tool-box">
      {source === 'mcp' && <div className="tool-id-strip">Tool ID {rawName}</div>}
      {grid ? <GridView rows={grid} header={header} /> : <OutputBox header={header} output={output} status={status} full={row.artifact ? { chatId: row.chatId, messageId: row.id } : null} />}
    </div>
  );
}

/** What saving the whole output needs when the row holds only the start of it. */
interface FullOutput {
  chatId: string;
  messageId: number;
}

function OutputBox({ header, output, status, full }: { header: string; output: string; status: ToolStatus; full: FullOutput | null }) {
  const { api } = useShell();
  const [wrap, setWrap] = useState(true);
  const [save, setSave] = useState<{ state: 'idle' | 'saving' | 'saved' | 'failed'; note?: string }>({ state: 'idle' });
  const copy = () => void navigator.clipboard?.writeText(output);
  // The row keeps only the start of a long output; the whole text is in a file the host holds.
  const saveFull = () => {
    if (!full) return;
    setSave({ state: 'saving' });
    api<{ path: string | null }>('message_output_save', full)
      .then((r) => setSave(r.path ? { state: 'saved', note: r.path } : { state: 'idle' }))
      .catch((e: unknown) => setSave({ state: 'failed', note: e instanceof Error ? e.message : String(e) }));
  };
  return (
    <>
      <div className="tool-box-head">
        <span className="tool-box-label">{header}</span>
        {full && (
          <span className="muted tool-saved" data-testid="tool-saved" role={save.state === 'failed' ? 'alert' : undefined}>
            {save.state === 'saved' ? `Saved to ${save.note}` : save.state === 'failed' ? `Not saved: ${save.note}` : 'The first part is shown'}
          </span>
        )}
        {full && (
          <button type="button" className="tool-box-btn" data-testid="tool-save-full" title="Save the whole output to a file" disabled={save.state === 'saving'} onClick={saveFull}>
            {save.state === 'saving' ? 'Saving…' : 'Save full output'}
          </button>
        )}
        <button
          type="button"
          className="tool-box-btn tool-box-icon"
          aria-pressed={wrap}
          aria-label="Wrap long lines"
          title={wrap ? 'Stop wrapping long lines' : 'Wrap long lines'}
          onClick={() => setWrap((w) => !w)}
        >
          <IconWrap size={13} />
        </button>
        <button type="button" className="tool-box-btn tool-box-icon" aria-label="Copy the output" title="Copy" onClick={copy} disabled={!output}>
          <IconCopy size={13} />
        </button>
      </div>
      <pre className={`tool-output mono ${wrap ? 'wrap' : ''}`}>{output || (status === 'running' ? 'Waiting for output…' : 'No text output.')}</pre>
    </>
  );
}

function GridView({ rows, header }: { rows: GridRow[]; header: string }) {
  return (
    <>
      <div className="tool-box-head">
        <span className="tool-box-label">{header}</span>
      </div>
      <dl className="result-grid" data-testid="result-grid">
        <GridRows rows={rows} />
      </dl>
    </>
  );
}

function GridRows({ rows }: { rows: GridRow[] }) {
  return (
    <>
      {rows.map((r, i) => (
        <div key={i} className="result-row" data-key={r.key}>
          <dt className="result-key">{r.key}</dt>
          <dd className="result-val">
            <CellView cell={r.cell} />
          </dd>
        </div>
      ))}
    </>
  );
}

function CellView({ cell }: { cell: Cell }) {
  switch (cell.t) {
    case 'bool':
      return <span className={cell.v ? 'result-on' : undefined}>{cell.v ? 'Yes' : 'No'}</span>;
    case 'color':
      return (
        <span className="result-color">
          <span className="swatch" style={{ background: cell.v }} />
          <code>{cell.v}</code>
        </span>
      );
    case 'empty':
      return <span className="result-empty">{cell.v}</span>;
    case 'chips':
      return (
        <span className="result-chips">
          {cell.v.map((c, i) => (
            <span key={i} className="result-chip">
              {c}
            </span>
          ))}
        </span>
      );
    case 'lines':
      return (
        <span className="result-lines">
          {cell.v.map((l, i) => (
            <span key={i} className="result-line">
              {l.name}
              {l.note && <span className="muted"> {l.note}</span>}
            </span>
          ))}
          {cell.more > 0 && <span className="muted">+{cell.more} more</span>}
        </span>
      );
    case 'group':
      return (
        <dl className="result-nested">
          <GridRows rows={cell.v} />
        </dl>
      );
    default:
      return <span>{cell.v}</span>;
  }
}

function ChangeView({ row }: { row: MessageRow }) {
  const { lines, cut } = changeLines(row);
  if (!lines.length) {
    return (
      <div className="tool-box">
        <div className="muted tool-note">No diff was recorded for this change.</div>
      </div>
    );
  }
  const height = Math.min(300, Math.max(84, lines.length * 21 + 16));
  return (
    <div className="tool-box" data-testid="tool-box">
      <div className="tool-box-head">
        <span className="tool-box-label">Changes</span>
      </div>
      <div className="change-view mono" style={{ height }}>
        {lines.map((l, i) => (
          <div key={i} className={`change-line sign-${l.sign === '+' ? 'add' : l.sign === '-' ? 'del' : 'ctx'}`}>
            <span className="sign">{l.sign}</span>
            {l.text}
          </div>
        ))}
      </div>
      {cut && <div className="muted tool-note">Excerpt: the change is longer than shown.</div>}
    </div>
  );
}
