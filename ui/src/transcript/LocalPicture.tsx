// A picture on disk shown in a tool card (a Read of an image file): asked of the host by path.

import { useEffect, useState } from 'react';
import { loadLocalImage } from '../markdown/localImages.ts';

export function LocalPicture({ path }: { path: string }) {
  const [state, setState] = useState<{ url: string } | { failed: true } | null>(null);
  useEffect(() => {
    let live = true;
    setState(null);
    loadLocalImage(path)
      .then((url) => live && setState({ url }))
      .catch(() => live && setState({ failed: true }));
    return () => {
      live = false;
    };
  }, [path]);
  if (state && 'failed' in state) return <div className="tool-picture-missing muted">The picture could not be shown.</div>;
  if (!state) return <div className="tool-picture tool-picture-loading" aria-busy="true" />;
  return <img className="tool-picture" data-testid="tool-picture" src={state.url} alt={path} />;
}
