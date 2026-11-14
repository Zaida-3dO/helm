// Whether the main area shows the schedules view. The sidebar's Schedules tile toggles it; Escape,
// its close button, opening a chat and the new-chat page close it.

type Listener = () => void;

class SchedulesView {
  private open = false;
  private readonly listeners = new Set<Listener>();

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  isOpen = (): boolean => this.open;

  show = (): void => this.set(true);

  close = (): void => this.set(false);

  toggle = (): void => this.set(!this.open);

  private set(open: boolean): void {
    if (open === this.open) return;
    this.open = open;
    for (const l of [...this.listeners]) l();
  }
}

export const schedulesView = new SchedulesView();
