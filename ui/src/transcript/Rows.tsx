// One transcript row per kind: user, assistant (with thinking preview), tool result,
// context rows, and error/retry rows. Every row carries the stable `transcript-row` hook with its
// status and role.

import { memo, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { EditMessage, type EditState } from './EditMessage.tsx';
import { boardRef, forwardRef, galleryRef, panelRef, type ForwardRef } from './cards.ts';
import { GalleryCard } from './GalleryCard.tsx';
import { Markdown, type LinkTarget } from './Markdown.tsx';
import { MessageImages } from './MessageImages.tsx';
import { MessageRefs } from './MessageRefs.tsx';
import { OptionBoardCard } from './OptionBoard.tsx';
import { RecapCard } from './RecapCard.tsx';
import { agentNote, type AgentNote } from './agentNote.ts';
import { bubbleText, fromTelegram, pageMessage, typedOn } from './origin.ts';
import { messageTimeLabel } from './rowMeta.ts';
import { IconMonitor, IconPaperPlane } from '../ui/icons.tsx';
import { ReplyActions, UserActions } from './MessageActions.tsx';
import { rowFootersFor, type SlotContext } from './slots.ts';
import { Glyph } from './icons.tsx';
import { SignInAgain } from './SignInAgain.tsx';
import { isSignInError } from './signin.ts';
import { ToolCard, type ToolAction } from './ToolCard.tsx';
import { GatekeeperReviewLink } from '../gatekeeper/ReviewLink.tsx';
import { stopCheckLine, stopClass, stopKindLabel } from '../gatekeeper/model.ts';
import { usePacedText, useThinkingPreview } from './usePaced.ts';
import { foldLabel, type RowFlags } from './workBlocks.ts';

export interface RowContext {
  canStopTool: (row: MessageRow) => boolean;
  onStopTool: (toolCallId: string) => void;
  onLink: (target: LinkTarget) => void;
  /** `owner/repo` of the chat's GitHub repository, so a bare `#12` in a reply links to its pull request. */
  mentionRepo?: string;
  /** For the transcript slots (slots.ts). */
  chatId: string;
  latestAssistantId: number | null;
  running: boolean;
  interactionOpen: boolean;
  /** Offer "Sign in again" under a sign-in failure. False for a chat on a local model; missing means offer. */
  canSignIn?: boolean;
  /** Transcript element toggles; a missing key is shown. */
  elements?: { tokens?: 'shown' | 'hidden'; turnTime?: 'shown' | 'hidden'; fileChanges?: 'shown' | 'hidden'; timeline?: 'shown' | 'hidden' };
  /** The chat runs on another device (its name): Allow requests wait there, not here. */
  remoteDevice?: string;
  /** Edit and resend: the message being edited, if any. */
  editing?: EditState | null;
  /** A turn end is being checked against the working mode: the quiet line under the latest reply. */
  stopChecking?: boolean;
  onEdit?: (row: MessageRow) => void;
  onResend?: (text: string) => Promise<void>;
  onCancelEdit?: () => void;
  /** Forward a port an agent offered to this PC; set only for a chat that runs on another PC. */
  onForward?: (ref: ForwardRef) => void;
}

/** The button a tool row offers for what its call asked the PC showing the chat to do. */
function toolAction(row: MessageRow, ctx: RowContext): ToolAction | undefined {
  const panel = panelRef(row);
  if (panel) {
    return {
      label: 'Open',
      title: panel.kind === 'page' ? `Show the page "${panel.label}" in the side panel` : `Show ${panel.label} in the side panel`,
      testId: 'tool-open-panel',
      run: () => ctx.onLink(panel.target),
    };
  }
  const fwd = ctx.onForward ? forwardRef(row) : null;
  if (fwd && ctx.onForward) {
    const forward = ctx.onForward;
    return { label: 'Forward', title: `Open port ${fwd.port} of ${ctx.remoteDevice ?? 'that PC'} on this PC`, testId: 'tool-forward-port', run: () => forward(fwd) };
  }
  return undefined;
}

function slotContext(row: MessageRow, ctx: RowContext, hasRecap = false): SlotContext {
  return {
    chatId: ctx.chatId,
    isLatestAssistant: row.id === ctx.latestAssistantId,
    running: ctx.running,
    interactionOpen: ctx.interactionOpen,
    ...(hasRecap ? { hasRecap } : {}),
    ...(ctx.remoteDevice ? { remote: true } : {}),
  };
}

/** Registered footers for one row (the reply's change card). */
function Footers({ row, ctx, hasRecap }: { row: MessageRow; ctx: RowContext; hasRecap?: boolean }) {
  const sctx = slotContext(row, ctx, hasRecap);
  const footers = rowFootersFor(row, sctx);
  if (!footers.length) return null;
  return (
    <>
      {footers.map((f) => (
        <div key={f.id} className="row-footer" data-footer={f.id}>
          {f.render(row, sctx)}
        </div>
      ))}
    </>
  );
}

const RETRY_TEXT: Record<string, string> = {
  retrying: 'Request failed, retrying…',
  waiting: 'Waiting for the connection to retry…',
  recovered: 'Recovered after a retry.',
  retried: 'Retried.',
  cancelled: 'Retry cancelled.',
  failed: 'Request failed.',
};

const CONTEXT_LABEL: Record<string, string> = {
  compaction: 'Context compacted',
  modelChange: 'Model changed',
  effortChange: 'Effort changed',
  notice: 'Notice',
  restart: 'Helm restarted',
  fork: 'Fork',
  edit: 'Edited',
  orchestrator: 'Orchestrator',
  telegram: 'Telegram',
  afk: 'AFK',
  gatekeeper: 'Answered for you',
  interruption: 'Interruption',
  signIn: 'Sign-in',
  startup: 'Startup',
};

function Thinking({ text, streaming, onLink }: { text: string; streaming: boolean; onLink: RowContext['onLink'] }) {
  const preview = useThinkingPreview(text);
  const [open, setOpen] = useState(false);
  if (!text) return null;
  return (
    <div className={`thinking${open ? ' open' : ''}`} data-testid="thinking">
      <button type="button" className="thinking-toggle" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        <Glyph name="chevron" size={12} className="thinking-chevron" />
        <span className={`thinking-label${streaming ? ' live' : ''}`}>{streaming ? 'Thinking' : 'Thought process'}</span>
        {!open && preview && <span className="thinking-preview">· {preview}</span>}
      </button>
      {open && (
        <div className="thinking-body">
          <Markdown text={text} onLink={onLink} />
        </div>
      )}
    </div>
  );
}

/** A message a page wrote: a neat card naming its page and summarising it, collapsed by
 * default, expanding to the whole text the agent received. */
function PageCard({ page, row, ctx }: { page: NonNullable<ReturnType<typeof pageMessage>>; row: MessageRow; ctx: RowContext }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={`bubble page-card${open ? ' open' : ''}`} data-testid="page-card" data-page={page.title || undefined}>
      <div className="page-card-head">
        <IconMonitor size={12} /> From page: {page.title || 'a page'}
      </div>
      {page.overview && !open && <div className="page-card-overview" data-testid="page-card-overview">{page.overview}</div>}
      {open && <Markdown text={row.text} onLink={ctx.onLink} />}
      <button type="button" className="page-card-toggle" onClick={() => setOpen((o) => !o)} aria-expanded={open} data-testid="page-card-toggle">
        <Glyph name="chevron" size={12} className="page-card-chevron" />
        {open ? 'Hide what the agent received' : 'Show what the agent received'}
      </button>
      <div className="page-card-note muted">The page wrote this, not the user.</div>
    </div>
  );
}

const NOTE_GLYPH: Record<AgentNote['kind'], 'chat' | 'clock' | 'bell'> = { peer: 'chat', schedule: 'clock', app: 'bell' };
const NOTE_HEAD: Record<AgentNote['kind'], string> = { peer: 'Message from ', schedule: 'Scheduled: ', app: 'From ' };

/** A message from another agent or from the app: one compact card — who sent it (linked to the
 * sending chat when a chat did) and a one-line digest, the whole text behind the chevron. */
function NoteCard({ note, ctx }: { note: AgentNote; ctx: RowContext }) {
  const [open, setOpen] = useState(false);
  const link = (
    <button type="button" className="note-card-from" data-testid="note-card-from" onClick={() => ctx.onLink({ kind: 'chat', href: `helm://chats/${note.chatId}` })}>
      {note.sender}
    </button>
  );
  return (
    <div className={`bubble note-card note-${note.kind}${open ? ' open' : ''}`} data-testid="note-card" data-kind={note.kind}>
      <div className="note-card-head">
        <button type="button" className="note-card-toggle" data-testid="note-card-toggle" onClick={() => setOpen((o) => !o)} aria-expanded={open} aria-label={open ? 'Hide the full message' : 'Show the full message'}>
          <Glyph name="chevron" size={12} className="note-card-chevron" />
        </button>
        <Glyph name={NOTE_GLYPH[note.kind]} size={12} className="note-card-glyph" />
        <span className="note-card-title" data-testid="note-card-title">
          {NOTE_HEAD[note.kind]}
          {note.senderIsLink && note.chatId ? link : note.sender}
          {!note.senderIsLink && note.chatId && (
            <>
              {' by '}
              <button type="button" className="note-card-from" data-testid="note-card-by" onClick={() => ctx.onLink({ kind: 'chat', href: `helm://chats/${note.chatId}` })}>
                {note.chatTitle || 'the chat that made it'}
              </button>
            </>
          )}
        </span>
      </div>
      {!open && note.digest && <div className="note-card-digest" data-testid="note-card-digest">{note.digest}</div>}
      {open && (
        <>
          {/* The summary stays first when the card opens, so opening never loses the one line
              that says what the message is; the rule separates it from the full text. */}
          {note.digest && (
            <div className="note-card-summary" data-testid="note-card-summary">
              {note.digest}
            </div>
          )}
          <div className="note-card-body" data-testid="note-card-body">
            <Markdown text={note.body} onLink={ctx.onLink} />
            <div className="note-card-note muted">{note.note}</div>
          </div>
        </>
      )}
    </div>
  );
}

function AssistantBody({ row, ctx, hideText }: { row: MessageRow; ctx: RowContext; hideText?: boolean }) {
  const streaming = row.status === 'streaming';
  const text = usePacedText(row.text, streaming);
  return (
    <>
      {/* A note's own thinking sits with the note, behind its own collapsed toggle: never hidden
          outright, just folded until the reader opens it. */}
      <Thinking text={row.thinking} streaming={streaming} onLink={ctx.onLink} />
      {text && !hideText && <Markdown text={text} streaming={streaming} interactive mentionRepo={ctx.mentionRepo} onLink={ctx.onLink} />}
    </>
  );
}

export const Row = memo(function Row({ row, ctx, final, afterTool, indent, afterFold, beforeTool, noticeLive, recap, turnMs, skills }: { row: MessageRow; ctx: RowContext; turnMs?: number; skills?: string } & RowFlags) {
  const retry = row.details?.retry;
  const common = { 'data-testid': 'transcript-row', 'data-status': row.status, 'data-role': row.role, 'data-row-id': row.id };
  const place = `${afterTool ? ' after-tool' : ''}${final ? ' final-answer' : ''}${indent ? ' indented' : ''}${afterFold ? ' after-fold' : ''}${beforeTool ? ' before-tool' : ''}`;
  if (row.role === 'user') {
    const note = agentNote(row);
    const page = pageMessage(row);
    const pc = typedOn(row);
    return (
      <div className={`row row-user status-${row.status}`} {...common}>
        {fromTelegram(row) && (
          <div className="row-origin muted" data-testid="row-origin" data-origin="telegram" title="Sent from Telegram">
            <IconPaperPlane size={12} /> Telegram
          </div>
        )}
        {pc && (
          <div className="row-origin muted" data-testid="row-origin" data-origin="remote" title={`Typed on ${pc}`}>
            <IconMonitor size={12} /> from {pc}
          </div>
        )}
        {ctx.editing?.rowId === row.id && ctx.onResend && ctx.onCancelEdit ? (
          <EditMessage edit={ctx.editing} onResend={ctx.onResend} onCancel={ctx.onCancelEdit} />
        ) : page ? (
          <PageCard page={page} row={row} ctx={ctx} />
        ) : note ? (
          <NoteCard note={note} ctx={ctx} />
        ) : (
          <div className="bubble" title={messageTimeLabel(row.timestamp, Date.now())} data-testid="user-bubble">
            <MessageImages chatId={ctx.chatId} images={row.details?.images} />
            <MessageRefs refs={row.details?.refs} />
            <Markdown text={bubbleText(row)} onLink={ctx.onLink} />
          </div>
        )}
        {row.status !== 'complete' && <div className="row-meta muted">{row.status}</div>}
        {ctx.editing?.rowId !== row.id && (
          <UserActions
            row={row}
            sctx={slotContext(row, ctx)}
            onEdit={row.status === 'complete' && !ctx.running && !note && !page && ctx.onEdit ? () => ctx.onEdit?.(row) : undefined}
          />
        )}
        <Footers row={row} ctx={ctx} />
      </div>
    );
  }
  if (row.role === 'toolResult') {
    // An agent's gallery or option board is drawn as a card, not as a tool row.
    const gallery = galleryRef(row);
    const board = gallery ? null : boardRef(row);
    if (gallery || board) {
      return (
        <div className={`row row-card${place}`} {...common}>
          {gallery ? <GalleryCard chatId={ctx.chatId} galleryId={gallery.galleryId} /> : <OptionBoardCard chatId={ctx.chatId} boardId={board!.boardId} />}
        </div>
      );
    }
    return (
      <div className={`row row-tool${place}`} {...common}>
        <ToolCard row={row} canStop={ctx.canStopTool(row)} onStop={ctx.onStopTool} remoteDevice={ctx.remoteDevice} action={toolAction(row, ctx)} />
      </div>
    );
  }
  if (row.role === 'context') {
    const kind = row.details?.contextKind ?? 'notice';
    // A checked stop that was legitimate stands on its own line: the class word (Blocker, Plan
    // limit, Failure, Stopped to ask) on a chip, with the one-line reason Helm filed.
    const stop = stopKindLabel(kind);
    const label = stop ?? CONTEXT_LABEL[kind] ?? 'Notice';
    // A notice says why something did not happen (a local model that cannot load, say): its words are
    // plain text, not the muted side note the other context rows are.
    const notice = kind === 'notice';
    return (
      // Only the newest notice is a live region (noticeLive, set by the layout): the list mounts rows
      // as the reader scrolls, and a live region is announced when it appears, so marking every old
      // notice would re-read the chat's history of refusals each time a row scrolls into view.
      <div className={`row row-context${notice ? ' row-notice' : ''}${place}`} {...common} role={notice && noticeLive ? 'status' : undefined}>
        <span className={`context-label${stop ? ' stop-chip' : ''}`} data-testid={stop ? 'stop-row' : undefined} data-class={stop ? stopClass(kind) : undefined}>{label}</span>
        {row.text && <span className={notice ? 'context-notice-text' : 'muted'}> · {row.text}</span>}
        {kind === 'signIn' && ctx.canSignIn !== false && <SignInAgain message="Claude Code could not refresh its sign-in for about half an hour. Sign in again; Helm carries on by itself once it works." />}
        {/* A decision taken for you can be reviewed or reversed from Settings → Gatekeeper. */}
        {kind === 'gatekeeper' && (
          <span className="muted">
            {' ('}
            <GatekeeperReviewLink />
            {')'}
          </span>
        )}
      </div>
    );
  }
  return (
    <div className={`row row-assistant status-${row.status}${place}`} {...common} data-final={final ? 'true' : undefined}>
      {/* An old error row wrote its error into the reply text as well; showing both reads as the
          same line twice, so a body that repeats the error below is hidden. */}
      <AssistantBody
        row={row}
        ctx={ctx}
        hideText={row.status === 'error' && !!row.text && row.text === row.details?.errorMessage}
      />
      {row.status === 'error' && (
        <div className="error-row" data-testid="error-row">
          <Glyph name="alert" size={14} />
          <span>{row.details?.errorMessage ?? 'The request failed.'}</span>
        </div>
      )}
      {row.status === 'error' && ctx.canSignIn !== false && isSignInError(row.details?.errorMessage) && <SignInAgain />}
      {row.status === 'interrupted' && (
        <div className="interrupted-row muted" data-testid="interrupted-row">
          <Glyph name="pause" size={14} />
          <span>Interrupted</span>
        </div>
      )}
      {retry && (
        <div className={`retry-row retry-${retry.state}`} data-testid="retry-row" data-state={retry.state}>
          {RETRY_TEXT[retry.state]} {retry.attempt > 1 ? `(attempt ${retry.attempt})` : ''}
        </div>
      )}
      {/* Run to completion checks every stop: while that check runs, this line sits under the
          reply that ended the turn, and no "finished" notice has been given yet. */}
      {ctx.stopChecking && row.id === ctx.latestAssistantId && row.status === 'complete' && (
        <div className="stop-check-row muted" data-testid="stop-check-line">
          {stopCheckLine()}
        </div>
      )}
      <ReplyActions
        row={row}
        sctx={slotContext(row, ctx, recap !== undefined)}
        turnMs={turnMs}
        skills={skills}
        showTurnTime={ctx.elements?.turnTime !== 'hidden'}
        showTokens={ctx.elements?.tokens !== 'hidden'}
      />
      {recap && <RecapCard recap={recap} row={row} ctx={slotContext(row, ctx, true)} />}
      <Footers row={row} ctx={ctx} hasRecap={recap !== undefined} />
    </div>
  );
});

/** One compact row per run of work: what it holds, and how many calls failed inside it. */
export function FoldRow({ tools, thoughts, messages, elapsedMs, failed, open, onToggle }: { tools: number; thoughts: number; messages: number; elapsedMs: number; failed: number; open: boolean; onToggle: () => void }) {
  const label = foldLabel({ tools, thoughts, messages, elapsedMs });
  return (
    <div className={`row row-fold${open ? ' open' : ''}`}>
      <button
        type="button"
        className="fold-toggle"
        // The closed variant keeps the original hook the e2e harness clicks to unfold; the open one is separate so that a loop clicking "the first fold" cannot close it again.
        data-testid={open ? 'work-fold-open' : 'work-fold'}
        aria-expanded={open}
        title={open ? 'Hide this work' : 'Show this work'}
        onClick={onToggle}
      >
        <Glyph name="chevron" size={14} className="fold-chevron" />
        <span>{label}</span>
        {failed > 0 && (
          <span className="fold-failed" data-testid="work-failed">
            {failed} failed
          </span>
        )}
      </button>
    </div>
  );
}
