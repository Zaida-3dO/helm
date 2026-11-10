// The turn's checklist, pinned just above the activity dock while the turn runs and updated in
// place. A step that has just been done ticks with a short animation (none under reduced motion,
// by the stylesheet).

import { useEffect, useRef, useState } from 'react';
import { Glyph } from './icons.tsx';
import { stepsDone, tickedIndexes } from './checklist.ts';
import type { Todo, TodoState } from './todos.ts';
import './cards.css';

const ICON = { pending: 'circle', in_progress: 'circle-dot', completed: 'circle-check' } as const;
const LABEL = { pending: 'To do', in_progress: 'In progress', completed: 'Done' } as const;

/** The steps, one line each. `ticking` marks the ones that just finished; `final` flags the ones the turn left undone. */
export function ChecklistSteps({ todos, ticking, final }: { todos: readonly Todo[]; ticking?: ReadonlySet<number>; final?: boolean }) {
  return (
    <ul className="todo-list lc-steps" data-testid="checklist-steps">
      {todos.map((t, i) => (
        <li key={i} className={`todo todo-${t.state}${ticking?.has(i) ? ' todo-ticked' : ''}`} data-state={t.state}>
          <Glyph name={ICON[t.state]} size={13} className="todo-icon" />
          <span className="visually-hidden">{LABEL[t.state]}: </span>
          <span className="todo-text">{t.text}</span>
          {final && t.state !== 'completed' && <span className="todo-unfinished muted">not finished</span>}
        </li>
      ))}
    </ul>
  );
}

export function LiveChecklist({ todos }: { todos: readonly Todo[] }) {
  const [open, setOpen] = useState(true);
  const before = useRef<TodoState[] | null>(null);
  const ticking = tickedIndexes(before.current, todos);
  useEffect(() => {
    before.current = todos.map((t) => t.state);
  }, [todos]);
  const done = stepsDone(todos);
  return (
    <div className="live-checklist" data-testid="live-checklist" data-done={done} data-total={todos.length}>
      <button type="button" className="lc-head" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <Glyph name="chevron" size={12} className="lc-chevron" />
        <span className="lc-title">Plan</span>
        <span className="lc-count muted">
          {done} of {todos.length} done
        </span>
        <span className="lc-bar" aria-hidden="true">
          <span className="lc-bar-fill" style={{ width: `${todos.length ? Math.round((done / todos.length) * 100) : 0}%` }} />
        </span>
      </button>
      {open && <ChecklistSteps todos={todos} ticking={ticking} />}
    </div>
  );
}
