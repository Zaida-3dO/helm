// The command that upgrades a Node that is too old, with a copy button. Helm shows it and never
// runs it: the user pastes it into their own terminal.

import { useState } from 'react';
import { Button } from '../../ui/controls.tsx';
import './runtime.css';

export function UpgradeCommand({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(command).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <div className="rt-upgrade" data-testid="runtime-upgrade">
      <span>Upgrade Node by running this in a terminal:</span>
      <code data-testid="runtime-upgrade-command">{command}</code>
      <Button variant="secondary" onClick={copy} data-testid="runtime-upgrade-copy">
        {copied ? 'Copied' : 'Copy'}
      </Button>
    </div>
  );
}
