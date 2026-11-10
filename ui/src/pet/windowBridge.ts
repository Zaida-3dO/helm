// The two window calls the pet needs (dragging itself and hearing where it was dropped). Loaded on
// demand so the pet's logic stays testable without Tauri; outside Tauri (the dev fake host) they
// do nothing.

type WindowApi = typeof import('@tauri-apps/api/window');

let loaded: Promise<WindowApi> | null = null;
const windowApi = (): Promise<WindowApi> => (loaded ??= import('@tauri-apps/api/window'));

/** Load the window API ahead of time, so a drag starts the instant the pointer moves. */
export function preloadWindowApi(): void {
  windowApi().catch(() => undefined);
}

/** Hand the pointer to the system, which moves the window until the button is released. */
export async function startDragging(): Promise<void> {
  try {
    await (await windowApi()).getCurrentWindow().startDragging();
  } catch {
    // Not in Tauri, or the pointer was already released.
  }
}

/** Call `report(x, y)` with the window's corner (screen pixels) whenever it moves. */
export async function watchMoves(report: (x: number, y: number) => void): Promise<() => void> {
  try {
    const w = (await windowApi()).getCurrentWindow();
    return await w.onMoved(({ payload }) => report(payload.x, payload.y));
  } catch {
    return () => undefined;
  }
}
