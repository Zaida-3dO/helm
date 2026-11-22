// Name suggestions under a chat's title field, in the sidebar's Rename dialog and the top bar's
// rename box. It asks the host as soon as it appears, shows a loading line meanwhile, then up to
// three options; one click hands the option to `onPick`. It never fills the field itself and goes
// away once the person has typed. It never waits on nothing: when the host cannot answer, or does
// not in time, a line says so, and a note under the options says when they are not the model's.

import { useEffect, useState } from 'react';
import type { Api } from '../api/client.ts';
import './titleSuggestions.css';
import { askForNames, suggestionsVisible, type SuggestionResult } from './titleOptions.ts';

export interface TitleSuggestionsProps {
  api: Api;
  chatId: string;
  /** What the field holds now. */
  value: string;
  /** What the field held when the box opened. */
  opened: string;
  onPick: (title: string) => void;
}

export function TitleSuggestions({ api, chatId, value, opened, onPick }: TitleSuggestionsProps) {
  // null: still asking.
  const [result, setResult] = useState<SuggestionResult | null>(null);
  useEffect(() => {
    let live = true;
    setResult(null);
    void askForNames(() => api<unknown>('title_suggest', { chatId }), opened).then((r) => live && setResult(r));
    return () => {
      live = false;
    };
    // The box is opened once for one title: a later `opened` is not a new question.
  }, [api, chatId]);
  if (!suggestionsVisible(value, opened)) return null;
  if (result === null) {
    return (
      <div className="title-suggest title-suggest-loading" role="status" aria-busy="true" data-testid="title-suggestions" data-state="loading">
        Thinking of names…
      </div>
    );
  }
  const { options, note } = result;
  if (options.length === 0) {
    return (
      <div className="title-suggest title-suggest-loading" role="status" data-testid="title-suggestions" data-state="failed">
        {note}
      </div>
    );
  }
  return (
    <div className="title-suggest" role="group" aria-label="Suggested names" data-testid="title-suggestions" data-state="ready">
      {options.map((t) => (
        // Pressing must not take focus from the field, or the top bar's rename would be saved first.
        <button key={t} type="button" className="title-suggest-option" title={t} data-testid="title-suggestion" onMouseDown={(e) => e.preventDefault()} onClick={() => onPick(t)}>
          {t}
        </button>
      ))}
      {note && <span className="title-suggest-note">{note}</span>}
    </div>
  );
}
