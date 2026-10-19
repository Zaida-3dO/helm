// Basic controls. Styles in ui.css; tokens in styles.css.

import { forwardRef, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type TextareaHTMLAttributes } from 'react';
import { IconAlert, IconCheck, IconChevronDown, IconChevronRight, IconInfo, IconSearch, IconSend, IconStop, IconX } from './icons.tsx';

/**
 * `ghost` is a quiet text button with an optional leading icon: no border, `--secondary`
 * text, `--hover` on hover (the top bar's action row). `send` and `stop` are the composer's square
 * buttons; with no children they draw their own glyph.
 */
export type ButtonVariant = 'primary' | 'secondary' | 'text' | 'icon' | 'row-action' | 'ghost' | 'send' | 'stop';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  /** Destructive: label and icon in the error colour (there is no red fill). */
  danger?: boolean;
  /** Icon buttons need a label for screen readers; it is also the native tooltip. */
  label?: string;
  pressed?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = 'secondary', danger, label, pressed, className, title, type, children, ...rest },
  ref,
) {
  const cls = ['ui-btn', `ui-btn-${variant}`, danger ? 'ui-danger' : '', className ?? ''].filter(Boolean).join(' ');
  return (
    <button
      ref={ref}
      type={type ?? 'button'}
      className={cls}
      aria-label={label}
      title={title ?? label}
      aria-pressed={pressed}
      {...rest}
    >
      {children ?? defaultGlyph(variant)}
    </button>
  );
});

function defaultGlyph(variant: ButtonVariant): ReactNode {
  if (variant === 'send') return <IconSend size={15} strokeWidth={2.2} />;
  if (variant === 'stop') return <IconStop size={12} filled />;
  return null;
}

export interface TextInputProps extends InputHTMLAttributes<HTMLInputElement> {
  invalid?: string | null;
}

export const TextInput = forwardRef<HTMLInputElement, TextInputProps>(function TextInput({ invalid, className, ...rest }, ref) {
  return (
    <>
      <input ref={ref} className={`ui-input${className ? ` ${className}` : ''}`} aria-invalid={invalid ? true : undefined} {...rest} />
      {invalid && (
        <span className="ui-field-error" role="alert">
          {invalid}
        </span>
      )}
    </>
  );
});

export const TextArea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(function TextArea({ className, ...rest }, ref) {
  return <textarea ref={ref} className={`ui-input ui-textarea${className ? ` ${className}` : ''}`} {...rest} />;
});

export interface CheckboxProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'type' | 'onChange'> {
  label?: ReactNode;
  onChange?: (checked: boolean) => void;
}

export function Checkbox({ label, onChange, className, ...rest }: CheckboxProps) {
  return (
    <label className={`ui-check${className ? ` ${className}` : ''}`}>
      <input type="checkbox" onChange={(e) => onChange?.(e.target.checked)} {...rest} />
      {label && <span>{label}</span>}
    </label>
  );
}

export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
  icon?: ReactNode;
  disabled?: boolean;
}

/** A segmented radio group: arrow keys move the choice. */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
  testId,
}: {
  value: T;
  options: SegmentedOption<T>[];
  onChange: (v: T) => void;
  label: string;
  testId?: string;
}) {
  const i = options.findIndex((o) => o.value === value);
  return (
    <div
      className="ui-segmented"
      role="radiogroup"
      aria-label={label}
      data-testid={testId}
      onKeyDown={(e) => {
        if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
        e.preventDefault();
        const step = e.key === 'ArrowRight' ? 1 : -1;
        for (let k = 1; k <= options.length; k++) {
          const o = options[(i + step * k + options.length * 2) % options.length];
          if (o && !o.disabled) {
            onChange(o.value);
            break;
          }
        }
      }}
    >
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          tabIndex={o.value === value ? 0 : -1}
          disabled={o.disabled}
          className="ui-segment"
          data-value={o.value}
          onClick={() => onChange(o.value)}
        >
          {o.icon}
          <span>{o.label}</span>
        </button>
      ))}
    </div>
  );
}

/** A chip: context tags, attachments, skills. */
export function Chip({
  children,
  icon,
  onClick,
  onRemove,
  title,
  testId,
}: {
  children: ReactNode;
  icon?: ReactNode;
  onClick?: () => void;
  onRemove?: () => void;
  title?: string;
  testId?: string;
}) {
  const body = (
    <>
      {icon}
      <span className="ui-chip-text">{children}</span>
    </>
  );
  return (
    <span className="ui-chip" title={title} data-testid={testId}>
      {onClick ? (
        <button type="button" className="ui-chip-main" onClick={onClick}>
          {body}
        </button>
      ) : (
        <span className="ui-chip-main">{body}</span>
      )}
      {onRemove && (
        <button type="button" className="ui-chip-remove" aria-label="Remove" title="Remove" onClick={onRemove}>
          ×
        </button>
      )}
    </span>
  );
}

/** A settings row: title and description on the left, the control on the right. */
export function SettingRow({ title, description, children, testId }: { title: ReactNode; description?: ReactNode; children?: ReactNode; testId?: string }) {
  return (
    <div className="setting-row" data-testid={testId}>
      <div className="setting-text">
        <div className="setting-title">{title}</div>
        {description && <div className="setting-desc">{description}</div>}
      </div>
      <div className="setting-control">{children}</div>
    </div>
  );
}

const KIT_ICONS = {
  x: IconX,
  check: IconCheck,
  'chevron-down': IconChevronDown,
  'chevron-right': IconChevronRight,
  alert: IconAlert,
  info: IconInfo,
  search: IconSearch,
} as const;

/** The kit's own few glyphs, by name (kept for existing callers). New code imports from icons.tsx. */
export function Icon({ name, size = 14 }: { name: keyof typeof KIT_ICONS; size?: number }) {
  const Glyph = KIT_ICONS[name];
  return <Glyph size={size} />;
}
