// Rules: Markdown files a layer adds to every chat's instructions. A list with one inline editor
// (title and text). A save carries the revision the rule was read at; if the file changed since
// (an agent saved to it, or another device), the host refuses and the editor offers to reload.

import { useEffect, useState } from 'react';
import { Button, TextArea, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import type { KnowledgeOps, RuleRow } from './api.ts';
import { errorCode, errorText, sizeLabel } from './common.ts';
import { MoveTo } from './LayerPicker.tsx';
import type { LayerOption } from './layers.ts';

export type RunAction = (action: () => Promise<string | void>) => Promise<void>;

interface Shared {
  ops: KnowledgeOps;
  layer: string;
  options: LayerOption[];
  busy: boolean;
  run: RunAction;
  confirm: (spec: ConfirmSpec) => void;
}

function RuleEditor({ ops, layer, options, busy, run, confirm, rule, onClose }: Shared & { rule: RuleRow | null; onClose: () => void }) {
  const [title, setTitle] = useState(rule?.title ?? '');
  const [text, setText] = useState('');
  const [revision, setRevision] = useState<string | undefined>(rule?.revision);
  const [loaded, setLoaded] = useState(rule === null);
  const [error, setError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);

  const load = () => {
    if (!rule) return;
    setLoaded(false);
    ops
      .ruleRead(layer, rule.path)
      .then((r) => {
        setTitle(r.rule.title);
        setText(r.text);
        setRevision(r.rule.revision);
        setStale(false);
        setError(null);
        setLoaded(true);
      })
      .catch((e: unknown) => setError(errorText(e)));
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(load, [rule?.path, layer]);

  const save = () =>
    run(async () => {
      try {
        await ops.ruleWrite({ layer, ...(rule ? { path: rule.path } : {}), title: title.trim(), text, ...(revision ? { revision } : {}) });
      } catch (e) {
        if (errorCode(e) === 'conflict') {
          setStale(true);
          setError(null);
          return;
        }
        throw e;
      }
      onClose();
      return rule ? `Saved ${title.trim()}.` : `Added ${title.trim()}.`;
    });

  const askDelete = () =>
    rule &&
    confirm({
      title: 'Delete the rule?',
      body: `${rule.title} is removed from this list. Chats stop getting it from their next start.`,
      confirm: 'Delete',
      danger: true,
      onConfirm: () =>
        run(async () => {
          await ops.remove(layer, 'rule', rule.path);
          onClose();
          return `Deleted ${rule.title}.`;
        }),
    });

  return (
    <div className="kn-editor" data-testid="knowledge-rule-editor">
      {error && (
        <div className="ui-field-error" role="alert" data-testid="knowledge-rule-error">
          {error}
        </div>
      )}
      {stale && (
        <div className="ui-field-error" role="alert" data-testid="knowledge-rule-stale">
          This rule changed since you opened it, so nothing was saved.{' '}
          <Button variant="text" onClick={load}>
            Load the newer text
          </Button>
        </div>
      )}
      <TextInput
        aria-label="Rule title"
        placeholder="Title"
        value={title}
        maxLength={120}
        disabled={!loaded}
        onChange={(e) => setTitle(e.target.value)}
        data-testid="knowledge-rule-title"
      />
      <TextArea
        aria-label="Rule text"
        placeholder="What every chat should do or know, in plain sentences."
        rows={8}
        value={text}
        disabled={!loaded}
        onChange={(e) => setText(e.target.value)}
        data-testid="knowledge-rule-text"
      />
      <div className="kn-actions">
        <Button variant="primary" disabled={busy || !loaded || !title.trim() || !text.trim()} data-testid="knowledge-rule-save" onClick={() => void save()}>
          Save
        </Button>
        <Button variant="secondary" disabled={busy} data-testid="knowledge-rule-cancel" onClick={onClose}>
          Cancel
        </Button>
        {rule && (
          <>
            <MoveTo
              options={options}
              from={layer}
              what={rule.title}
              testId="knowledge-rule-move"
              disabled={busy || !loaded}
              onMove={(to) =>
                void run(async () => {
                  await ops.move('rule', rule.path, layer, to);
                  onClose();
                  return `Moved ${rule.title}.`;
                })
              }
            />
            <Button variant="text" danger disabled={busy} data-testid="knowledge-rule-delete" onClick={askDelete}>
              Delete
            </Button>
          </>
        )}
      </div>
    </div>
  );
}

export function RulesSection(props: Shared & { rules: RuleRow[] }) {
  const { ops, layer, busy, run, rules } = props;
  // `null`: nothing open; `'new'`: a blank editor; otherwise the path of the rule being edited.
  const [open, setOpen] = useState<string | null>(null);
  useEffect(() => setOpen(null), [layer]);
  const editing = open && open !== 'new' ? (rules.find((r) => r.path === open) ?? null) : null;

  return (
    <section className="kn-section" data-testid="knowledge-rules">
      <div className="kn-head">
        <h3 className="hub-heading">Rules</h3>
        <div className="kn-actions">
          <Button variant="primary" disabled={busy} data-testid="knowledge-rule-new" onClick={() => setOpen('new')}>
            New rule
          </Button>
          <Button variant="secondary" disabled={busy} data-testid="knowledge-rules-open" onClick={() => void run(() => ops.reveal(layer, 'rule').then(() => undefined))}>
            Open folder
          </Button>
        </div>
      </div>
      <div className="setting-desc">
        Plain instructions added to every chat's starting prompt. The agent can also add one when you tell it to always or never do something.
      </div>
      {rules.length === 0 && open !== 'new' && (
        <div className="setting-desc" data-testid="knowledge-rules-empty">
          No rules in this list yet.
        </div>
      )}
      {rules.map((r) =>
        open === r.path ? (
          <RuleEditor key={r.path} {...props} rule={editing} onClose={() => setOpen(null)} />
        ) : (
          <div className="skills-row" key={r.path} data-testid={`knowledge-rule-${r.path}`}>
            <div className="skills-what">
              <div className="skills-name">{r.title}</div>
              <div className="skills-desc">
                {r.path.replace(/^rules\//, '')} · {sizeLabel(r.size)}
                {r.tooLarge && ' · too large to load into chats'}
              </div>
            </div>
            <Button variant="text" disabled={busy} data-testid={`knowledge-rule-edit-${r.path}`} onClick={() => setOpen(r.path)}>
              Edit
            </Button>
          </div>
        ),
      )}
      {open === 'new' && <RuleEditor {...props} rule={null} onClose={() => setOpen(null)} />}
    </section>
  );
}
