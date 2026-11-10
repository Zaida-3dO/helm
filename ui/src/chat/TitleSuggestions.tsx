// Name suggestions under a chat's title field, in the sidebar's Rename dialog and the top bar's
// rename box. It asks the host as soon as it appears, shows a loading line meanwhile, then up to
// three options; one click hands the option to `onPick`. It never fills the field itself, goes away
// once the person has typed, and shows nothing at all when the host has no suggestion.

import { useEffect, useState } from 'react';
import type { Api } from '../api/client.ts';
import './titleSuggestions.css';
import { suggestionOptions, suggestionsVisible } from './titleOptions.ts';

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
  // null: still asking. An empty list, or a failure, shows nothing.
  const [options, setOptions] = useState<string[] | null>(null);
  useEffect(() => {
    let live = true;
    setOptions(null);
    api<unknown>('title_suggest', { chatId })
      .then((r) => live && setOptions(suggestionOptions(r, opened)))
      .catch(() => live && setOptions([]));
    return () => {
      live = false;
    };
    // The box is opened once for one title: a later `opened` is not a new question.
  }, [api, chatId]);
  if (!suggestionsVisible(value, opened) || (options !== null && options.length === 0)) return null;
  if (options === null) {
    return (
      <div className="title-suggest title-suggest-loading" role="status" aria-busy="true" data-testid="title-suggestions" data-state="loading">
        Thinking of names…
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
    </div>
  );
}
