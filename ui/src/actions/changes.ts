// A tiny signal so the top-bar row reloads after Settings saved the button set.

const target = new EventTarget();
const NAME = 'actions-changed';

export function notifyActionsChanged(): void {
  target.dispatchEvent(new Event(NAME));
}

export function onActionsChanged(f: () => void): () => void {
  target.addEventListener(NAME, f);
  return () => target.removeEventListener(NAME, f);
}
