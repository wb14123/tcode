import { parseStreamLine } from './messages';
import { timelineCache } from './timeline-cache';
import type { RawStreamEvent } from './types';

/**
 * Shared cache manager for SSE event stream history.
 *
 * Each component (session-view, subagent-view) owns one instance.
 * It tracks byte offset, line numbers, and raw event strings so the
 * component can reconnect SSE from the cached position after a reload.
 */
export class TimelineCacheManager {
  byteOffset = 0;
  lineNumber = 0;
  nextLineNumber = 0;

  private sessionId = '';
  private subagentId: string | undefined;
  private events: string[] = [];
  private saveTimer: number | null = null;

  get hasCache(): boolean {
    return this.sessionId !== '';
  }

  matches(sessionId: string, subagentId?: string): boolean {
    return this.sessionId === sessionId && this.subagentId === subagentId;
  }

  /**
   * Load cached events from IndexedDB.
   * Returns true on cache hit; appendEvents is called synchronously
   * with parsed events on hit (never called on miss).
   */
  async loadAndRestore(
    sessionId: string,
    subagentId: string | undefined,
    appendEvents: (events: RawStreamEvent[]) => void,
  ): Promise<boolean> {
    const entry = await timelineCache.load(sessionId, subagentId);
    this.sessionId = sessionId;
    this.subagentId = subagentId;
    if (entry !== null) {
      const parsed = entry.events
        .map((raw) => parseStreamLine(raw))
        .filter((e): e is RawStreamEvent => e !== null);
      if (parsed.length > 0) {
        appendEvents(parsed);
      }
      this.byteOffset = entry.byteOffset;
      this.lineNumber = entry.lastLineNumber;
      this.nextLineNumber = entry.lastLineNumber + 1;
      this.events = [...entry.events];
      return true;
    }
    this.byteOffset = 0;
    this.lineNumber = 0;
    this.nextLineNumber = 1;
    this.events = [];
    return false;
  }

  /** Track a newly-received SSE event for later persistence. */
  accumulateEvent(raw: string, sseLineNumber: number | null): void {
    if (sseLineNumber !== null && !Number.isNaN(sseLineNumber)) {
      this.lineNumber = sseLineNumber;
      this.nextLineNumber = sseLineNumber + 1;
    }
    this.events.push(raw);
    this.byteOffset += new TextEncoder().encode(raw).length + 1;
  }

  /** Schedule a debounced (2 s) cache save. */
  scheduleSave(): void {
    if (this.saveTimer !== null || !this.sessionId) return;
    this.saveTimer = window.setTimeout(() => {
      this.saveTimer = null;
      void timelineCache.save(this.sessionId, this.subagentId, {
        sessionId: this.sessionId,
        events: this.events,
        byteOffset: this.byteOffset,
        lastLineNumber: this.lineNumber,
        updatedAt: Date.now(),
      });
    }, 2000);
  }

  /** Persist immediately, cancelling any pending debounced save. */
  flushSave(): void {
    this.cancelSave();
    if (this.sessionId) {
      void timelineCache.save(this.sessionId, this.subagentId, {
        sessionId: this.sessionId,
        events: this.events,
        byteOffset: this.byteOffset,
        lastLineNumber: this.lineNumber,
        updatedAt: Date.now(),
      });
    }
  }

  /** Cancel a pending debounced save without persisting. */
  cancelSave(): void {
    if (this.saveTimer !== null) {
      window.clearTimeout(this.saveTimer);
      this.saveTimer = null;
    }
  }

  /** Reset all state for a new session / subagent. */
  reset(): void {
    this.sessionId = '';
    this.subagentId = undefined;
    this.byteOffset = 0;
    this.lineNumber = 0;
    this.nextLineNumber = 1;
    this.events = [];
    this.saveTimer = null;
  }
}
