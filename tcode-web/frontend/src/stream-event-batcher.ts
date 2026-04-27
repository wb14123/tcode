import type { RawStreamEvent } from './types';

export class StreamEventBatcher {
  private pendingEvents: RawStreamEvent[] = [];
  private frameHandle: number | null = null;
  private readonly applyEvents: (events: RawStreamEvent[]) => void;

  constructor(applyEvents: (events: RawStreamEvent[]) => void) {
    this.applyEvents = applyEvents;
  }

  enqueue(event: RawStreamEvent): void {
    if (event.wire?.variant === 'AssistantMessageEnd') {
      this.flush();
      this.applyEvents([event]);
      return;
    }

    this.pendingEvents.push(event);
    if (this.frameHandle !== null) {
      return;
    }

    this.frameHandle = window.requestAnimationFrame(() => {
      this.frameHandle = null;
      this.flush();
    });
  }

  flush(): void {
    if (this.frameHandle !== null) {
      window.cancelAnimationFrame(this.frameHandle);
      this.frameHandle = null;
    }

    if (!this.pendingEvents.length) {
      return;
    }

    const events = this.pendingEvents;
    this.pendingEvents = [];
    this.applyEvents(events);
  }

  clear(): void {
    if (this.frameHandle !== null) {
      window.cancelAnimationFrame(this.frameHandle);
      this.frameHandle = null;
    }
    this.pendingEvents = [];
  }
}
