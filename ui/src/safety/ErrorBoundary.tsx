// Error boundaries. A pane that throws while rendering shows a short notice with Try again instead
// of taking the whole window down; a throw outside every pane shows the fatal screen, which offers
// to reload. Both report the error to the host's renderer error log.

import { Component, type ErrorInfo, type ReactNode } from 'react';
import { describe, reportError } from './report.ts';
import './safety.css';

interface Props {
  /** Names the part in the notice and in the log line. */
  name: string;
  /** The whole window: a full-screen notice that offers to reload. */
  fatal?: boolean;
  /** A change of this value clears a caught error (for example the selected chat). */
  resetKey?: unknown;
  children: ReactNode;
}

interface State {
  error: unknown;
  failed: boolean;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, failed: false };

  static getDerivedStateFromError(error: unknown): State {
    return { error, failed: true };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    reportError('pane', error, `${this.props.name}${info.componentStack ? ` ${info.componentStack.trim().split('\n')[0] ?? ''}` : ''}`);
  }

  componentDidUpdate(prev: Props): void {
    if (this.state.failed && !this.props.fatal && prev.resetKey !== this.props.resetKey) this.reset();
  }

  private reset = () => this.setState({ error: null, failed: false });

  render(): ReactNode {
    if (!this.state.failed) return this.props.children;
    const { message } = describe(this.state.error);
    if (this.props.fatal) {
      return (
        <div className="safety-fatal" role="alert" data-testid="fatal-error">
          <h2>Helm hit an unexpected error</h2>
          <p>Your chats are saved. Reloading usually fixes this.</p>
          <pre className="safety-detail">{message}</pre>
          <button type="button" className="ui-btn ui-btn-primary" onClick={() => location.reload()}>
            Reload Helm
          </button>
        </div>
      );
    }
    return (
      <div className="safety-pane" role="alert" data-testid="pane-error" data-pane={this.props.name}>
        <p>The {this.props.name} hit an error and stopped.</p>
        <pre className="safety-detail">{message}</pre>
        <button type="button" className="ui-btn ui-btn-secondary" onClick={this.reset}>
          Try again
        </button>
      </div>
    );
  }
}
