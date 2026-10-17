// Turns Claude Code's can_use_tool control requests into the unified interaction request, and the
// host's answers back into control responses.

import { randomUUID } from "node:crypto";
import type { Json, UiRequest, UiResponse } from "../../protocol/types.ts";

/** Details over this size cannot be approved: deny or stop only. */
export const MAX_APPROVABLE_DETAIL_BYTES = 64 * 1024;
export const MAX_QUESTIONS = 4;
export const MAX_OPTIONS = 32;

export const ALLOW_ONCE = "Allow once";
export const ALLOW_SESSION = "Allow for this session";
export const DENY = "Deny";
export const STOP_TURN = "Stop turn";
export const APPROVE_PLAN = "Approve plan";
export const KEEP_PLANNING = "Keep planning";

const QUESTION_TOOL = "AskUserQuestion";
const PLAN_TOOL = "ExitPlanMode";

type Behaviour = Json; // {behavior:"allow", updatedInput, updatedPermissions?} | {behavior:"deny", message, interrupt?}

interface Base {
  requestId: string;
  uiId: string;
  input: Json;
}
interface Approval extends Base {
  kind: "approval";
  options: string[];
  suggestions: unknown[];
}
interface Question extends Base {
  kind: "question";
  questions: { question: string; header?: string; options: { label: string; description?: string }[]; multiSelect?: boolean }[];
  answers: (string | undefined)[];
  step: number;
}
interface Plan extends Base {
  kind: "plan";
  options: string[];
}
type Pending = Approval | Question | Plan;

export interface InteractionHooks {
  emit: (e: Json) => void;
  /** Sends the control_response for a CLI request. */
  reply: (requestId: string, response: Behaviour) => void;
  /** The user picked Stop turn: the driver treats this as a user Stop. */
  onStopTurn: () => void;
  skipPermissions: boolean;
}

function str(v: unknown): string {
  if (typeof v === "string") return v;
  if (v === undefined) return "";
  return JSON.stringify(v, null, 2);
}

function byteLen(s: string): number {
  return Buffer.byteLength(s, "utf8");
}

/** Context rows for an approval card: command, file, content, replace/with, ... */
export function approvalContext(toolName: string, input: Json, req: Json): { label: string; text: string }[] {
  const rows: { label: string; text: string }[] = [];
  const used = new Set<string>();
  const take = (key: string, label: string) => {
    if (input[key] !== undefined) {
      rows.push({ label, text: str(input[key]) });
      used.add(key);
    }
  };
  take("command", "Command");
  take("file_path", "File");
  take("path", "Path");
  take("content", "Content");
  take("old_string", "Replace");
  take("new_string", "With");
  take("edits", "Edits");
  take("description", "Description");
  if (req.blocked_path !== undefined) rows.push({ label: "Blocked path", text: str(req.blocked_path) });
  if (req.decision_reason !== undefined) rows.push({ label: "Reason", text: str(req.decision_reason) });
  const rest: Json = {};
  for (const [k, v] of Object.entries(input)) if (!used.has(k)) rest[k] = v;
  if (Object.keys(rest).length > 0) rows.push({ label: "Other input", text: JSON.stringify(rest, null, 2) });
  if (rows.length === 0) rows.push({ label: "Tool", text: toolName });
  return rows;
}

/** Skip-permissions mode still asks before deleting a drive root, a home folder or similar. */
export function looksLikeCriticalRemoval(toolName: string, input: Json): boolean {
  if (toolName !== "Bash" && toolName !== "PowerShell") return false;
  const cmd = typeof input.command === "string" ? input.command : "";
  return /\b(rm|rmdir|del|Remove-Item)\b[^\n]*\s(\/|~\/?|[A-Za-z]:[\\/]?|\$HOME|%USERPROFILE%|\.\.?)(\s|$)/i.test(cmd);
}

export class Interactions {
  private readonly pending = new Map<string, Pending>(); // by requestId
  private readonly byUi = new Map<string, string>(); // uiId -> requestId
  private readonly h: InteractionHooks;

  constructor(hooks: InteractionHooks) {
    this.h = hooks;
  }

  get size(): number {
    return this.pending.size;
  }

  onCanUseTool(requestId: string, req: Json): void {
    const toolName = typeof req.tool_name === "string" ? req.tool_name : "tool";
    const input = (req.input && typeof req.input === "object" ? req.input : {}) as Json;
    if (toolName === QUESTION_TOOL) return this.startQuestion(requestId, input);
    if (toolName === PLAN_TOOL) return this.startPlan(requestId, input);

    if (this.h.skipPermissions && !looksLikeCriticalRemoval(toolName, input)) {
      this.h.reply(requestId, { behavior: "allow", updatedInput: input });
      return;
    }
    const suggestions = Array.isArray(req.permission_suggestions) ? req.permission_suggestions : [];
    const context = approvalContext(toolName, input, req);
    const detailBytes = context.reduce((n, r) => n + byteLen(r.text) + byteLen(r.label), 0);
    const tooBig = detailBytes > MAX_APPROVABLE_DETAIL_BYTES;
    const options = tooBig
      ? [DENY, STOP_TURN]
      : suggestions.length > 0
        ? [ALLOW_ONCE, ALLOW_SESSION, DENY, STOP_TURN]
        : [ALLOW_ONCE, DENY, STOP_TURN];
    const p: Approval = { kind: "approval", requestId, uiId: randomUUID(), input, options, suggestions };
    this.track(p);
    const ui: UiRequest = {
      type: "extension_ui_request",
      id: p.uiId,
      method: "select",
      attention: "approval",
      title: `Allow ${toolName}?`,
      context: tooBig ? context.map((r) => ({ label: r.label, text: r.text.slice(0, 4096) })) : context,
      options,
    };
    if (tooBig) ui.message = "The details of this request are over 64 KiB, so it cannot be approved from here. Deny it or stop the turn.";
    this.h.emit(ui as unknown as Json);
  }

  private startQuestion(requestId: string, input: Json): void {
    const raw = Array.isArray(input.questions) ? (input.questions as Json[]) : [];
    const questions = raw.map((q) => ({
      question: str(q.question),
      header: typeof q.header === "string" ? q.header : undefined,
      options: (Array.isArray(q.options) ? (q.options as Json[]) : []).map((o) =>
        typeof o === "string" ? { label: o } : { label: str(o.label), description: typeof o.description === "string" ? o.description : undefined },
      ),
      multiSelect: q.multiSelect === true,
    }));
    if (questions.length === 0 || questions.length > MAX_QUESTIONS || questions.some((q) => q.options.length > MAX_OPTIONS)) {
      this.h.reply(requestId, {
        behavior: "deny",
        message: `This question set cannot be shown: Helm allows 1 to ${MAX_QUESTIONS} questions with at most ${MAX_OPTIONS} options each.`,
      });
      this.h.emit({ type: "notice", text: "A question from Claude was refused because it was over the size limit." });
      return;
    }
    const p: Question = { kind: "question", requestId, uiId: "", input, questions, answers: questions.map(() => undefined), step: 0 };
    this.pending.set(requestId, p);
    this.askStep(p, 0);
  }

  private askStep(p: Question, step: number): void {
    if (p.uiId) this.byUi.delete(p.uiId);
    p.step = step;
    p.uiId = randomUUID();
    this.byUi.set(p.uiId, p.requestId);
    const q = p.questions[step];
    const ui: UiRequest = {
      type: "extension_ui_request",
      id: p.uiId,
      method: "select",
      attention: "question",
      title: q.question,
      header: q.header,
      options: q.options.map((o) => o.label),
      optionDescriptions: q.options.map((o) => o.description ?? ""),
      allowOther: true,
      step,
      steps: p.questions.length,
    };
    const prev = p.answers[step];
    if (prev !== undefined) ui.prefill = prev;
    this.h.emit(ui as unknown as Json);
  }

  private startPlan(requestId: string, input: Json): void {
    const plan = str(input.plan);
    const tooBig = byteLen(plan) > MAX_APPROVABLE_DETAIL_BYTES;
    const options = tooBig ? [KEEP_PLANNING] : [APPROVE_PLAN, KEEP_PLANNING];
    const p: Plan = { kind: "plan", requestId, uiId: randomUUID(), input, options };
    this.track(p);
    const ui: UiRequest = {
      type: "extension_ui_request",
      id: p.uiId,
      method: "confirm",
      attention: "plan",
      title: "Approve plan?",
      context: [{ label: "Plan", text: tooBig ? plan.slice(0, 4096) : plan, markdown: true }],
      options,
    };
    if (tooBig) ui.message = "The plan is over 64 KiB, so it cannot be approved from here.";
    this.h.emit(ui as unknown as Json);
  }

  private track(p: Pending): void {
    this.pending.set(p.requestId, p);
    this.byUi.set(p.uiId, p.requestId);
  }

  private finish(p: Pending, response: Behaviour): void {
    this.pending.delete(p.requestId);
    this.byUi.delete(p.uiId);
    this.h.reply(p.requestId, response);
  }

  onResponse(r: UiResponse): void {
    const requestId = this.byUi.get(r.id);
    if (!requestId) return; // stale or unknown: already answered or dismissed
    const p = this.pending.get(requestId);
    if (!p) return;
    if (p.kind === "approval") return this.answerApproval(p, r);
    if (p.kind === "plan") return this.answerPlan(p, r);
    return this.answerQuestion(p, r);
  }

  private answerApproval(p: Approval, r: UiResponse): void {
    const choice = r.cancelled ? DENY : r.value;
    // Anything not on the offered list is a deny: fail closed.
    const picked = choice && p.options.includes(choice) ? choice : DENY;
    if (picked === ALLOW_ONCE) return this.finish(p, { behavior: "allow", updatedInput: p.input });
    if (picked === ALLOW_SESSION) {
      // Narrow every suggestion to this session; never write a settings file.
      const updatedPermissions = p.suggestions.map((s) =>
        s && typeof s === "object" ? { ...(s as Json), destination: "session" } : s,
      );
      return this.finish(p, { behavior: "allow", updatedInput: p.input, updatedPermissions });
    }
    if (picked === STOP_TURN) {
      this.finish(p, { behavior: "deny", message: "The user stopped the turn.", interrupt: true });
      this.h.onStopTurn();
      return;
    }
    this.finish(p, { behavior: "deny", message: "The user denied this request." });
  }

  private answerPlan(p: Plan, r: UiResponse): void {
    const approve = !r.cancelled && p.options.includes(APPROVE_PLAN) && (r.confirmed === true || r.value === APPROVE_PLAN);
    if (approve) return this.finish(p, { behavior: "allow", updatedInput: p.input });
    this.finish(p, { behavior: "deny", message: "The user wants to keep planning." });
  }

  private answerQuestion(p: Question, r: UiResponse): void {
    if (r.cancelled) {
      return this.finish(p, { behavior: "deny", message: "The user did not answer the question." });
    }
    if (typeof r.goto === "number") {
      const target = Math.trunc(r.goto);
      if (target >= 0 && target < p.questions.length) {
        if (typeof r.value === "string") p.answers[p.step] = r.value; // keep typed text as a draft
        return this.askStep(p, target);
      }
      return this.askStep(p, p.step);
    }
    if (typeof r.value !== "string") return this.askStep(p, p.step);
    p.answers[p.step] = r.value;
    const next = p.answers.findIndex((a, i) => a === undefined && i !== p.step);
    if (next !== -1) return this.askStep(p, next);
    const answers: Record<string, string> = {};
    p.questions.forEach((q, i) => (answers[q.question] = p.answers[i] ?? ""));
    this.finish(p, { behavior: "allow", updatedInput: { ...p.input, answers } });
  }

  /** The CLI withdrew a request (control_cancel_request). */
  cancel(requestId: string): void {
    const p = this.pending.get(requestId);
    if (!p) return;
    this.pending.delete(requestId);
    this.byUi.delete(p.uiId);
    this.emitDismiss(p.uiId);
  }

  /** Turn stopped or settled: withdraw everything still shown. */
  dismissAll(): void {
    for (const p of [...this.pending.values()]) {
      this.pending.delete(p.requestId);
      this.byUi.delete(p.uiId);
      this.emitDismiss(p.uiId);
    }
  }

  private emitDismiss(uiId: string): void {
    this.h.emit({ type: "extension_ui_request", id: uiId, method: "dismiss", remaining: this.pending.size });
  }
}
