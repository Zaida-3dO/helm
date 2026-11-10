// The recap under a settled turn's final reply, worked out from the rows alone (no model call): how
// long the turn took, how many checklist steps got done, how many files it edited and which
// pictures it showed. A turn gets one when it kept a checklist or took three or more tool steps.

import type { MessageRow } from '../api/types.ts';
import { galleryRef } from './cards.ts';
import { stepsDone, turnChecklist } from './checklist.ts';
import type { Todo } from './todos.ts';
import { toolKind, toolStatus, toolTarget } from './toolKinds.ts';

/** Tool steps a turn needs for a recap when it kept no checklist. */
export const RECAP_MIN_STEPS = 3;

export interface Recap {
  elapsedMs: number;
  /** The checklist as the turn left it, with its unfinished steps. */
  checklist: Todo[] | null;
  /** Tool calls the turn made. */
  toolSteps: number;
  /** Different files the turn edited. */
  files: number;
  /** The picture galleries the turn showed, in order. */
  galleries: { galleryId: string; count: number }[];
}

/** The recap for one turn's rows (everything after the user's message), or null when it is too small to need one. */
export function buildRecap(work: readonly MessageRow[]): Recap | null {
  const tools = work.filter((r) => r.role === 'toolResult');
  const checklist = turnChecklist(work);
  if (!checklist && tools.length < RECAP_MIN_STEPS) return null;
  const edited = new Set<string>();
  for (const r of tools) {
    if (toolKind(r.toolName) === 'edit' && toolStatus(r) !== 'failed') edited.add(toolTarget(r) ?? `row:${r.id}`);
  }
  const galleries: Recap['galleries'] = [];
  for (const r of tools) {
    const g = galleryRef(r);
    if (g) galleries.push(g);
  }
  return {
    elapsedMs: work.length > 1 ? Math.max(0, work[work.length - 1]!.timestamp - work[0]!.timestamp) : 0,
    checklist,
    toolSteps: tools.length,
    files: edited.size,
    galleries,
  };
}

/** How many pictures the turn showed. */
export function pictureCount(recap: Pick<Recap, 'galleries'>): number {
  return recap.galleries.reduce((n, g) => n + g.count, 0);
}

/** The steps the recap reports: "5 of 6 steps" for a checklist, else the tool calls made. */
export function stepsLabel(recap: Pick<Recap, 'checklist' | 'toolSteps'>): string {
  if (recap.checklist) return `${stepsDone(recap.checklist)} of ${recap.checklist.length} steps`;
  return `${recap.toolSteps} ${recap.toolSteps === 1 ? 'step' : 'steps'}`;
}
