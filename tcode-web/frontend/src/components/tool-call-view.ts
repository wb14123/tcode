import { LitElement, html, nothing } from 'lit';

import { ReplayAwareBuffer, api, openEventStream } from '../api';
import { buildConversationTimeline, extractSystemNotification, parseStreamLine, rawVariant, renderTimelineItem } from '../messages';
import { hrefForRoute } from '../router';
import type { RawStreamEvent, TimelineItem } from '../types';

class TcodeToolCallView extends LitElement {
  static properties = {
    sessionId: { type: String },
    toolCallId: { type: String },
    subagentId: { type: String },
  };

  sessionId = '';
  toolCallId = '';
  subagentId = '';
  private statusText = '';
  private events: RawStreamEvent[] = [];
  private timeline: TimelineItem[] = [];
  private streamState = 'connecting';
  private loading = true;
  private errorMessage = '';
  private actionMessage = '';
  private cancelling = false;
  private pollHandle: number | null = null;
  private eventSource: EventSource | null = null;
  private replayBuffer = new ReplayAwareBuffer();

  createRenderRoot(): this {
    return this;
  }

  connectedCallback(): void {
    super.connectedCallback();
    this.startView();
  }

  disconnectedCallback(): void {
    super.disconnectedCallback();
    this.stopView();
  }

  updated(changed: Map<string, unknown>): void {
    if (changed.has('sessionId') || changed.has('toolCallId') || changed.has('subagentId')) {
      this.startView();
    }
  }

  private startView(): void {
    if (!this.sessionId || !this.toolCallId) {
      return;
    }

    this.stopView();
    this.statusText = '';
    this.events = [];
    this.timeline = [];
    this.streamState = 'connecting';
    this.loading = true;
    this.errorMessage = '';
    this.actionMessage = '';
    this.replayBuffer.reset();
    this.requestUpdate();
    void this.refreshStatus(true);
    this.openStream();
    this.pollHandle = window.setInterval(() => {
      void this.refreshStatus(false);
    }, 3000);
  }

  private stopView(): void {
    if (this.pollHandle !== null) {
      window.clearInterval(this.pollHandle);
      this.pollHandle = null;
    }

    this.eventSource?.close();
    this.eventSource = null;
  }

  private async refreshStatus(initial: boolean): Promise<void> {
    try {
      this.statusText = this.subagentId
        ? (await api.getSubagentToolCallStatus(this.sessionId, this.subagentId, this.toolCallId)).trim()
        : (await api.getSessionToolCallStatus(this.sessionId, this.toolCallId)).trim();
      this.errorMessage = '';
      if (initial) {
        this.loading = false;
      }
      this.requestUpdate();
    } catch (error) {
      this.errorMessage = error instanceof Error ? error.message : 'Failed to load tool-call status';
      if (initial) {
        this.loading = false;
      }
      this.requestUpdate();
    }
  }

  private displayPath(): string {
    return this.subagentId
      ? api.subagentToolCallDisplayPath(this.sessionId, this.subagentId, this.toolCallId)
      : api.sessionToolCallDisplayPath(this.sessionId, this.toolCallId);
  }

  private openStream(): void {
    this.eventSource?.close();
    this.replayBuffer.beginReplay();
    const source = openEventStream(this.displayPath());
    this.eventSource = source;

    source.onopen = () => {
      this.streamState = 'live';
      this.replayBuffer.beginReplay();
      this.requestUpdate();
    };

    source.onmessage = (message) => {
      const raw = message.data;
      if (typeof raw !== 'string' || !this.replayBuffer.accept(raw)) {
        return;
      }

      const parsed = parseStreamLine(raw);
      if (!parsed) {
        return;
      }

      this.events = [...this.events, parsed];
      this.timeline = buildConversationTimeline(this.events);
      const variant = rawVariant(parsed);
      const systemNotification = extractSystemNotification(parsed);
      if (systemNotification?.message) {
        this.dispatchEvent(
          new CustomEvent('system-notification', {
            detail: systemNotification,
            bubbles: true,
            composed: true,
          }),
        );
      }
      if (
        variant === 'PermissionUpdated' ||
        variant === 'ToolRequestPermission' ||
        variant === 'ToolPermissionApproved' ||
        variant === 'SubAgentWaitingPermission' ||
        variant === 'SubAgentPermissionApproved' ||
        variant === 'SubAgentPermissionDenied'
      ) {
        this.dispatchEvent(
          new CustomEvent('permissions-refresh-requested', {
            bubbles: true,
            composed: true,
          }),
        );
      }
      this.requestUpdate();
    };

    source.onerror = () => {
      this.streamState = 'reconnecting';
      this.replayBuffer.beginReplay();
      this.requestUpdate();
    };
  }

  private async cancelToolCall(): Promise<void> {
    if (this.cancelling) {
      return;
    }

    this.cancelling = true;
    this.errorMessage = '';
    this.actionMessage = '';
    this.requestUpdate();

    try {
      if (this.subagentId) {
        await api.cancelSubagentToolCall(this.sessionId, this.subagentId, this.toolCallId);
      } else {
        await api.cancelSessionToolCall(this.sessionId, this.toolCallId);
      }
      this.actionMessage = 'Tool cancel requested.';
    } catch (error) {
      this.errorMessage = error instanceof Error ? error.message : 'Failed to cancel tool call';
    } finally {
      this.cancelling = false;
      this.requestUpdate();
    }
  }

  private parentHref(): string {
    if (this.subagentId) {
      return hrefForRoute({
        kind: 'subagent',
        sessionId: this.sessionId,
        subagentId: this.subagentId,
      });
    }

    return hrefForRoute({
      kind: 'session',
      sessionId: this.sessionId,
    });
  }

  render() {
    return html`
      <section class="page">
        <header class="page-header">
          <div>
            <h1 class="page-title">Tool call ${this.toolCallId}</h1>
            <p class="page-subtitle">
              Dedicated tool-call stream and status view.
              ${this.subagentId ? `This tool belongs to subagent ${this.subagentId}.` : 'This tool belongs to the root session.'}
            </p>
          </div>
          <div class="header-actions">
            <span class="pill pill-${this.streamState}">${this.streamState}</span>
            <a class="button secondary" href="${this.parentHref()}">Back</a>
            <button class="button danger" @click=${this.cancelToolCall} ?disabled=${this.cancelling}>
              ${this.cancelling ? 'Cancelling…' : 'Cancel tool'}
            </button>
          </div>
        </header>

        ${this.errorMessage ? html`<div class="inline-alert error">${this.errorMessage}</div>` : nothing}
        ${this.actionMessage ? html`<div class="inline-alert info">${this.actionMessage}</div>` : nothing}

        <section class="panel">
          <h3>Status</h3>
          <pre class="timeline-pre">${this.statusText || 'No status text yet.'}</pre>
        </section>

        <section class="panel">
          <h3>Tool stream</h3>
          ${this.loading
            ? html`<div class="timeline-empty">Loading tool call…</div>`
            : this.timeline.length
              ? html`
                  <div class="timeline">
                    ${this.timeline.map((item) =>
                      renderTimelineItem(item, {
                        sessionId: this.sessionId,
                        currentSubagentId: this.subagentId || undefined,
                      }),
                    )}
                  </div>
                `
              : html`<div class="timeline-empty">Waiting for streamed tool events…</div>`}
        </section>
      </section>
    `;
  }
}

customElements.define('tcode-tool-call-view', TcodeToolCallView);
