// Rules: Markdown files a layer adds to chats' instructions. A list with one inline editor (title,
// text, and a "When it applies" block that says which chats and which moments the rule reaches).
// A save carries the revision the rule was read at; if the file changed since (an agent saved to
// it, or another device), the host refuses and the editor offers to reload.

import { useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import type { RuleWhen } from '../../api/generated/skills/RuleWhen.ts';
import { Button, Checkbox, Chip, Segmented, TextArea, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import type { KnowledgeOps, RulePreviewRow, RulesBudget, RuleRow } from './api.ts';
import { errorCode, errorText, sizeLabel } from './common.ts';
import { MoveTo } from './LayerPicker.tsx';
import {
  filterRules,
  layerLabel,
  localToday,
  ruleFlags,
  ruleHasTrigger,
  RULE_FILTERS,
  rulesBudgetNote,
  type RuleFilter,
  type LayerOption,
} from './layers.ts';

export type RunAction = (action: () => Promise<string | void>) => Promise<void>;

interface Shared {
  ops: KnowledgeOps;
  layer: string;
  options: LayerOption[];
  busy: boolean;
  run: RunAction;
  confirm: (spec: ConfirmSpec) => void;
  /** The name of the person whose list this is, when it is not the viewer's own; null otherwise. */
  other: string | null;
}

/** The editor's trigger fields, before they become a `RuleWhen`. Empty strings mean "no value". */
interface WhenDraft {
  mode: 'always' | 'when';
  /** '' means automatic: whole text up to the inline limit, else one line. */
  show: '' | 'full' | 'line';
  otherwise: '' | 'line';
  before: '' | 'stop';
  folders: string[];
  hosts: string[];
  pcs: string[];
  tools: string[];
  commands: string[];
  kinds: string[];
  people: string[];
  from: string;
  until: string;
  review: string;
}

const emptyWhen = (rule: RuleRow | null): WhenDraft => {
  const w = rule?.when;
  return {
    mode: ruleHasTrigger(w) ? 'when' : 'always',
    show: w?.show === 'line' ? 'line' : w?.show === 'full' || rule?.always ? 'full' : '',
    otherwise: w?.otherwise === 'line' ? 'line' : '',
    before: w?.before === 'stop' ? 'stop' : '',
    folders: [...(w?.folders ?? [])],
    hosts: [...(w?.hosts ?? [])],
    pcs: [...(w?.pcs ?? [])],
    tools: [...(w?.tools ?? [])],
    commands: [...(w?.commands ?? [])],
    kinds: [...(w?.kinds ?? [])],
    people: [...(w?.people ?? [])],
    from: w?.from ?? '',
    until: w?.until ?? '',
    review: w?.review ?? '',
  };
};

/** Trim, drop blanks, keep order, no repeats. */
function cleanList(values: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const v of values.map((s) => s.trim())) {
    if (v && !seen.has(v.toLowerCase())) {
      seen.add(v.toLowerCase());
      out.push(v);
    }
  }
  return out;
}

/** The header's trigger keys the draft states. `mode: 'always'` keeps only the show-level choice. */
function buildWhen(d: WhenDraft): RuleWhen {
  const base: RuleWhen = {
    show: d.show === 'line' ? 'line' : null,
    otherwise: null,
    before: null,
    folders: [],
    hosts: [],
    pcs: [],
    tools: [],
    commands: [],
    kinds: [],
    people: [],
    from: null,
    until: null,
    review: null,
    problems: [],
  };
  if (d.mode === 'always') return base;
  return {
    ...base,
    otherwise: d.otherwise || null,
    before: d.before || null,
    folders: cleanList(d.folders),
    hosts: cleanList(d.hosts),
    pcs: cleanList(d.pcs),
    tools: cleanList(d.tools),
    commands: cleanList(d.commands),
    kinds: d.kinds,
    people: d.people,
    from: d.from || null,
    until: d.until || null,
    review: d.review || null,
  };
}

/** One list of words (machines, PCs, tools, commands): chips plus a field that commits on Enter. */
function WordList({ testIdBase, values, onChange, placeholder, ariaLabel, disabled }: {
  testIdBase: string;
  values: string[];
  onChange: (next: string[]) => void;
  placeholder: string;
  ariaLabel: string;
  disabled: boolean;
}) {
  const [text, setText] = useState('');
  const commit = () => {
    const parts = text.split(',').map((s) => s.trim()).filter(Boolean);
    if (parts.length) onChange(cleanList([...values, ...parts]));
    setText('');
  };
  return (
    <div className="kn-when-list">
      {values.map((v, i) => (
        <Chip key={`${v}-${i}`} testId={`${testIdBase}-chip-${i}`} onRemove={disabled ? undefined : () => onChange(values.filter((_, j) => j !== i))}>
          {v}
        </Chip>
      ))}
      <TextInput
        aria-label={ariaLabel}
        placeholder={placeholder}
        spellCheck={false}
        value={text}
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            commit();
          }
        }}
        data-testid={`${testIdBase}-input`}
      />
    </div>
  );
}

/** Folder paths take a field and Browse each, because they may hold commas the plain lists split on. */
function FolderList({ testIdBase, run, values, onChange, disabled }: {
  testIdBase: string;
  run: RunAction;
  values: string[];
  onChange: (next: string[]) => void;
  disabled: boolean;
}) {
  const shell = useShell();
  const browse = (i: number) =>
    run(async () => {
      const picked = await shell.api<{ path: string | null }>('folder_pick', { title: 'Folder this rule covers' });
      if (picked.path) onChange(values.map((f, j) => (j === i ? picked.path as string : f)));
    });
  return (
    <div className="kn-when-list kn-when-folders">
      {values.map((f, i) => (
        <div className="kn-field" key={i}>
          <TextInput
            aria-label={`Folder ${i + 1} this rule covers`}
            placeholder="Folder path"
            spellCheck={false}
            value={f}
            disabled={disabled}
            onChange={(e) => onChange(values.map((x, j) => (j === i ? e.target.value : x)))}
            data-testid={`${testIdBase}-${i}`}
          />
          <Button variant="text" disabled={disabled} data-testid={`${testIdBase}-browse-${i}`} onClick={() => void browse(i)}>
            Browse
          </Button>
          <Button variant="text" danger disabled={disabled} data-testid={`${testIdBase}-remove-${i}`} onClick={() => onChange(values.filter((_, j) => j !== i))}>
            Remove
          </Button>
        </div>
      ))}
      <div>
        <Button variant="secondary" disabled={disabled} data-testid={`${testIdBase}-add`} onClick={() => onChange([...values, ''])}>
          Add folder
        </Button>
      </div>
    </div>
  );
}

function RuleEditor({ ops, layer, options, busy, run, confirm, other, rule, onClose }: Shared & { rule: RuleRow | null; onClose: () => void }) {
  const [title, setTitle] = useState(rule?.title ?? '');
  const [text, setText] = useState('');
  const [revision, setRevision] = useState<string | undefined>(rule?.revision);
  const [loaded, setLoaded] = useState(rule === null);
  const [error, setError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [when, setWhen] = useState<WhenDraft>(() => emptyWhen(rule));

  const load = () => {
    if (!rule) return;
    setLoaded(false);
    ops
      .ruleRead(layer, rule.path)
      .then((r) => {
        setTitle(r.rule.title);
        setText(r.text);
        setRevision(r.rule.revision);
        setWhen(emptyWhen(r.rule));
        setStale(false);
        setError(null);
        setLoaded(true);
      })
      .catch((e: unknown) => setError(errorText(e)));
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(load, [rule?.path, layer]);

  const set = <K extends keyof WhenDraft>(key: K, value: WhenDraft[K]) => setWhen((w) => ({ ...w, [key]: value }));

  const save = () =>
    run(async () => {
      try {
        await ops.ruleWrite({
          layer,
          ...(rule ? { path: rule.path } : {}),
          title: title.trim(),
          text,
          ...(revision ? { revision } : {}),
          always: when.show === 'full',
          when: buildWhen(when),
        });
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
      body: `${rule.title} is removed from ${other ? `${other}'s` : 'this'} list. Chats stop getting it from their next message.`,
      confirm: 'Delete',
      danger: true,
      onConfirm: () =>
        run(async () => {
          await ops.remove(layer, 'rule', rule.path);
          onClose();
          return `Deleted ${rule.title}.`;
        }),
    });

  const toggle = (list: string[], value: string) => (list.includes(value) ? list.filter((v) => v !== value) : [...list, value]);

  return (
    <div className="kn-editor" data-testid="knowledge-rule-editor">
      {other && (
        <div className="setting-desc" data-testid="knowledge-rule-whose">
          This rule is in {other}'s list: saving, moving or deleting it changes {other}'s profile.
        </div>
      )}
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
      <div className="kn-when" data-testid="knowledge-rule-when">
        <Segmented
          label="When it applies"
          value={when.mode}
          options={[
            { value: 'always', label: 'Every chat (as now)' },
            { value: 'when', label: 'Only when...' },
          ]}
          onChange={(mode) => set('mode', mode)}
          testId="knowledge-rule-when-mode"
        />
        <div className="setting-desc">
          A rule with no When reaches every chat, as always. One with a When reaches only the chats and moments it names: every box you fill must match, and values inside one box are alternatives.
        </div>
        {when.mode === 'always' && (
          <div className="kn-field">
            <label className="kn-choice">
              In a chat, show
              <select
                value={when.show}
                disabled={!loaded}
                onChange={(e) => set('show', e.target.value as WhenDraft['show'])}
                data-testid="knowledge-rule-always"
              >
                <option value="">Automatic: whole text when short, else one line</option>
                <option value="full">Whole text, however long it is</option>
                <option value="line">One line only</option>
              </select>
            </label>
            <span className="setting-desc">Use Whole text for short, essential rules; chats follow a one-line rule by its help topic.</span>
          </div>
        )}
        {when.mode === 'when' && (
          <>
            <div className="kn-when-grid">
              <div className="kn-field kn-field-col">
                <span className="kn-label">Folders whose work it covers</span>
                <FolderList testIdBase="knowledge-rule-when-folders" run={run} values={when.folders} onChange={(v) => set('folders', v)} disabled={!loaded} />
              </div>
              <div className="kn-field kn-field-col">
                <span className="kn-label">Machines the work reaches</span>
                <WordList testIdBase="knowledge-rule-when-hosts" ariaLabel="Machines the work reaches" placeholder="e.g. nas1, 192.0.2.10" values={when.hosts} onChange={(v) => set('hosts', v)} disabled={!loaded} />
              </div>
              <div className="kn-field kn-field-col">
                <span className="kn-label">Only on these PCs</span>
                <WordList testIdBase="knowledge-rule-when-pcs" ariaLabel="PC names this rule's chats run on" placeholder="e.g. pc-a" values={when.pcs} onChange={(v) => set('pcs', v)} disabled={!loaded} />
              </div>
              <div className="kn-field kn-field-col">
                <span className="kn-label">Tools to watch for</span>
                <WordList testIdBase="knowledge-rule-when-tools" ariaLabel="Tool names to watch for" placeholder="e.g. Bash, PowerShell, Read, Edit, mcp__github__*" values={when.tools} onChange={(v) => set('tools', v)} disabled={!loaded} />
              </div>
              <div className="kn-field kn-field-col">
                <span className="kn-label">Commands to watch for</span>
                <WordList testIdBase="knowledge-rule-when-commands" ariaLabel="Commands to watch for" placeholder="e.g. taskkill, pkill, git push" values={when.commands} onChange={(v) => set('commands', v)} disabled={!loaded} />
              </div>
            </div>
            <div className="kn-field">
              <span className="kn-label">Kind of chat</span>
              {(['coding', 'orchestrating', 'household'] as const).map((k) => (
                <Checkbox
                  key={k}
                  label={k === 'coding' ? 'Coding' : k === 'orchestrating' ? 'Orchestrating' : 'Household'}
                  checked={when.kinds.includes(k)}
                  disabled={!loaded}
                  onChange={() => set('kinds', toggle(when.kinds, k))}
                  data-testid={`knowledge-rule-when-kind-${k}`}
                />
              ))}
            </div>
            {options.some((o) => o.key !== 'everyone') && (
              <div className="kn-field">
                <span className="kn-label">People whose chats it reaches</span>
                {options
                  .filter((o) => o.key !== 'everyone')
                  .map((o) => (
                    <Checkbox
                      key={o.key}
                      label={o.label}
                      checked={when.people.includes(o.label)}
                      disabled={!loaded}
                      onChange={() => set('people', toggle(when.people, o.label))}
                      data-testid={`knowledge-rule-when-person-${o.testSuffix}`}
                    />
                  ))}
              </div>
            )}
            <div className="kn-field">
              <span className="kn-label">Dates</span>
              <TextInput type="date" aria-label="Start on" value={when.from} disabled={!loaded} onChange={(e) => set('from', e.target.value)} data-testid="knowledge-rule-when-from" />
              <TextInput type="date" aria-label="End on" value={when.until} disabled={!loaded} onChange={(e) => set('until', e.target.value)} data-testid="knowledge-rule-when-until" />
              <TextInput type="date" aria-label="Review on" value={when.review} disabled={!loaded} onChange={(e) => set('review', e.target.value)} data-testid="knowledge-rule-when-review" />
            </div>
            <div className="kn-field">
              <label className="kn-choice">
                When it applies, show
                <select value={when.show} disabled={!loaded} onChange={(e) => set('show', e.target.value as WhenDraft['show'])} data-testid="knowledge-rule-when-show">
                  <option value="">Automatic</option>
                  <option value="full">Whole text</option>
                  <option value="line">One line</option>
                </select>
              </label>
              <label className="kn-choice">
                Otherwise (until it fires)
                <select value={when.otherwise} disabled={!loaded} onChange={(e) => set('otherwise', e.target.value as WhenDraft['otherwise'])} data-testid="knowledge-rule-when-otherwise">
                  <option value="">Hidden</option>
                  <option value="line">One line</option>
                </select>
              </label>
              <label className="kn-choice">
                The first matching call is
                <select value={when.before} disabled={!loaded} onChange={(e) => set('before', e.target.value as WhenDraft['before'])} data-testid="knowledge-rule-when-before">
                  <option value="">Answered with the rule as a note</option>
                  <option value="stop">Stopped once, with the rule as the reason</option>
                </select>
              </label>
            </div>
          </>
        )}
        <div className="setting-desc" data-testid="knowledge-rule-when-note">
          A change to a rule reaches an open chat on its next message. A When that watches tools, commands or folders only takes hold at that chat's next start.
        </div>
      </div>
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
              verb="Copy"
              options={options}
              from={layer}
              what={rule.title}
              testId="knowledge-rule-copy"
              disabled={busy || !loaded}
              onMove={(to) =>
                void run(async () => {
                  await ops.copy('rule', rule.path, layer, to);
                  onClose();
                  return `Copied ${rule.title} to ${layerLabel(options, to)}.`;
                })
              }
            />
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

const DECISION_LABEL: Record<RulePreviewRow['decision'], string> = { full: 'Whole text', line: 'One line', hidden: 'Not shown' };

/** The small "which rules would a chat get?" panel; one call to the host, a list of answers. */
function PreviewPanel({ ops, layer, options, busy }: { ops: KnowledgeOps; layer: string; options: LayerOption[]; busy: boolean }) {
  const [folder, setFolder] = useState('');
  const [kind, setKind] = useState('');
  const [person, setPerson] = useState('');
  const [tool, setTool] = useState('');
  const [command, setCommand] = useState('');
  const [rows, setRows] = useState<RulePreviewRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const ask = () => {
    setError(null);
    ops
      .rulesPreview({
        layer,
        ...(folder.trim() ? { folder: folder.trim() } : {}),
        ...(kind ? { kind } : {}),
        ...(person ? { person } : {}),
        ...(tool.trim() ? { tool: tool.trim() } : {}),
        ...(command.trim() ? { command: command.trim() } : {}),
      })
      .then(setRows)
      .catch((e: unknown) => setError(errorText(e)));
  };

  return (
    <div className="kn-preview" data-testid="knowledge-rules-preview">
      <div className="kn-field">
        <TextInput aria-label="Preview folder" placeholder="A chat in this folder…" spellCheck={false} value={folder} onChange={(e) => setFolder(e.target.value)} data-testid="knowledge-rules-preview-folder" />
        <select aria-label="Preview kind" value={kind} onChange={(e) => setKind(e.target.value)} data-testid="knowledge-rules-preview-kind">
          <option value="">Any kind of chat</option>
          <option value="coding">Coding</option>
          <option value="orchestrating">Orchestrating</option>
          <option value="household">Household</option>
        </select>
        {options.some((o) => o.key !== 'everyone') && (
          <select aria-label="Preview person" value={person} onChange={(e) => setPerson(e.target.value)} data-testid="knowledge-rules-preview-person">
            <option value="">This list's own chats</option>
            {options.filter((o) => o.key !== 'everyone').map((o) => (<option key={o.key} value={o.label}>{o.label}</option>))}
          </select>
        )}
      </div>
      <div className="kn-field">
        <TextInput aria-label="Preview tool" placeholder="A tool call… (optional)" spellCheck={false} value={tool} onChange={(e) => setTool(e.target.value)} data-testid="knowledge-rules-preview-tool" />
        <TextInput aria-label="Preview command" placeholder="…with this command (optional)" spellCheck={false} value={command} onChange={(e) => setCommand(e.target.value)} data-testid="knowledge-rules-preview-command" />
        <Button variant="secondary" disabled={busy} data-testid="knowledge-rules-preview-ask" onClick={ask}>
          Show
        </Button>
      </div>
      {error && (
        <div className="ui-field-error" role="alert">{error}</div>
      )}
      {rows && (
        <ul className="kn-preview-list" data-testid="knowledge-rules-preview-rows">
          {rows.map((r) => (
            <li key={`${r.layer}/${r.path}`} data-testid={`knowledge-rules-preview-row-${r.path}`}>
              <span className="skills-name">{r.title}</span>
              <span className="kn-preview-decision">{DECISION_LABEL[r.decision]}{r.fires ? ' · this call fires it' : ''}</span>
              <div className="skills-desc">{r.why}</div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function RulesSection(props: Shared & { rules: RuleRow[]; budget?: RulesBudget }) {
  const { ops, layer, busy, run, rules, budget } = props;
  // `null`: nothing open; `'new'`: a blank editor; otherwise the path of the rule being edited.
  const [open, setOpen] = useState<string | null>(null);
  const [filter, setFilter] = useState<RuleFilter>('all');
  const [query, setQuery] = useState('');
  const [preview, setPreview] = useState(false);
  useEffect(() => {
    setOpen(null);
    setFilter('all');
    setQuery('');
  }, [layer]);
  const editing = open && open !== 'new' ? (rules.find((r) => r.path === open) ?? null) : null;
  const budgetNote = rulesBudgetNote(budget);
  const today = localToday();
  const shown = filterRules(rules, filter, query, today);

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
          <Button variant="text" disabled={busy} data-testid="knowledge-rules-preview-open" onClick={() => setPreview((p) => !p)}>
            Which rules would a chat get?
          </Button>
        </div>
      </div>
      <div className="setting-desc">
        Plain instructions for chats. A rule with no When reaches every chat; one with a When reaches only the chats and moments it names. The agent can also add one when you tell it to always or never do something.
      </div>
      {budgetNote && (
        <div className="setting-desc" data-testid="knowledge-rules-budget">
          {budgetNote}
        </div>
      )}
      {preview && <PreviewPanel ops={ops} layer={layer} options={props.options} busy={busy} />}
      <div className="kn-field kn-filters">
        <TextInput aria-label="Search rules" placeholder="Search…" value={query} onChange={(e) => setQuery(e.target.value)} data-testid="knowledge-rules-filter" />
        {RULE_FILTERS.map((f) => (
          <button
            key={f.value}
            type="button"
            className={`kn-flt${filter === f.value ? ' on' : ''}`}
            data-testid={`knowledge-rules-chip-${f.value}`}
            onClick={() => setFilter(f.value)}
          >
            {f.label}
          </button>
        ))}
      </div>
      {shown.length === 0 && open !== 'new' && (
        <div className="setting-desc" data-testid="knowledge-rules-empty">
          {rules.length === 0 ? 'No rules in this list yet.' : 'Nothing here matches the search or filter.'}
        </div>
      )}
      {shown.map((r) =>
        open === r.path ? (
          <RuleEditor key={r.path} {...props} rule={editing} onClose={() => setOpen(null)} />
        ) : (
          <div className="skills-row" key={r.path} data-testid={`knowledge-rule-${r.path}`}>
            <div className="skills-what">
              <div className="skills-name">
                {r.title}
                {ruleFlags(r, today).map((b) => (
                  <span key={b.kind} className={`kn-badge kn-badge-${b.kind}`} title={b.title} data-testid={`knowledge-rule-${b.kind}-${r.path}`}>
                    {b.label}
                  </span>
                ))}
              </div>
              <div className="skills-desc">
                {r.path.replace(/^rules\//, '')} · {sizeLabel(r.size)}
                {r.tooLarge && ' · too large to load into chats'}
              </div>
              {r.whenSummary && (
                <div className="skills-desc" data-testid={`knowledge-rule-when-${r.path}`}>
                  {r.whenSummary}
                </div>
              )}
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
