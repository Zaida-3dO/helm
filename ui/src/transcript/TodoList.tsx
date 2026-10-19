// The checklist body of a TodoWrite row: pending, in progress and done steps.

import { Glyph } from './icons.tsx';
import type { Todo } from './todos.ts';

const ICON = { pending: 'circle', in_progress: 'circle-dot', completed: 'circle-check' } as const;
const LABEL = { pending: 'To do', in_progress: 'In progress', completed: 'Done' } as const;

export function TodoList({ todos }: { todos: Todo[] }) {
  return (
    <div className="tool-box" data-testid="tool-box">
      <div className="tool-box-head">
        <span className="tool-box-label">Plan</span>
      </div>
      <ul className="todo-list" data-testid="todo-list">
        {todos.map((t, i) => (
          <li key={i} className={`todo todo-${t.state}`} data-state={t.state}>
            <Glyph name={ICON[t.state]} size={13} className="todo-icon" />
            <span className="visually-hidden">{LABEL[t.state]}: </span>
            <span className="todo-text">{t.text}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
