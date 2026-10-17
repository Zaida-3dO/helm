// Transcript slots: other areas add message actions, row footers and selection
// actions without editing the transcript. Registration is module-level and happens once, at
// import time of the registering module; the transcript reads the lists on render.
//
// - registerMessageAction: a button in a row's hover actions (Save for later, Explain
//   plainly).
// - registerRowFooter: content drawn under a row (option buttons on the latest assistant
//   reply; the turn change card after a completed turn).
// - registerSelectionAction: a button in the selected-text toolbar (Side
//   chat and Quote; Save and Remind me).

import type { ReactNode } from 'react';
import type { MessageRow } from '../api/types.ts';

/** What a slot sees of the transcript around a row. */
export interface SlotContext {
  chatId: string;
  /** True for the newest assistant row of the chat. */
  isLatestAssistant: boolean;
  /** True while the chat runs a turn. */
  running: boolean;
  /** True while an interaction card (approval, question, plan) is open. */
  interactionOpen: boolean;
}

export interface MessageAction {
  id: string;
  label: string;
  appliesTo: (row: MessageRow) => boolean;
  run: (row: MessageRow, ctx: SlotContext) => void;
}

export interface RowFooter {
  id: string;
  appliesTo: (row: MessageRow, ctx: SlotContext) => boolean;
  render: (row: MessageRow, ctx: SlotContext) => ReactNode;
}

/** The selected text and where it came from. */
export interface Selection {
  chatId: string;
  text: string;
  /** The rows the selection touches, oldest first; the last is the anchor. */
  rowIds: number[];
}

export interface SelectionAction {
  id: string;
  label: string;
  appliesTo?: (sel: Selection) => boolean;
  run: (sel: Selection) => void;
}

const messageActions: MessageAction[] = [];
const rowFooters: RowFooter[] = [];
const selectionActions: SelectionAction[] = [];

function upsert<T extends { id: string }>(list: T[], item: T): () => void {
  const i = list.findIndex((x) => x.id === item.id);
  if (i >= 0) list[i] = item;
  else list.push(item);
  return () => {
    const j = list.indexOf(item);
    if (j >= 0) list.splice(j, 1);
  };
}

/** Registering the same id again replaces the earlier one (hot reload). Returns an unregister. */
export function registerMessageAction(a: MessageAction): () => void {
  return upsert(messageActions, a);
}

export function registerRowFooter(f: RowFooter): () => void {
  return upsert(rowFooters, f);
}

export function registerSelectionAction(a: SelectionAction): () => void {
  return upsert(selectionActions, a);
}

export function messageActionsFor(row: MessageRow): MessageAction[] {
  return messageActions.filter((a) => safe(() => a.appliesTo(row)));
}

export function rowFootersFor(row: MessageRow, ctx: SlotContext): RowFooter[] {
  return rowFooters.filter((f) => safe(() => f.appliesTo(row, ctx)));
}

export function selectionActionsFor(sel: Selection): SelectionAction[] {
  return selectionActions.filter((a) => !a.appliesTo || safe(() => a.appliesTo!(sel)));
}

/** A slot that throws is left out rather than breaking the transcript. */
function safe(f: () => boolean): boolean {
  try {
    return f();
  } catch (e) {
    console.error('a transcript slot failed', e);
    return false;
  }
}
