import { LitElement, html, nothing } from 'lit';

import { ApiError, ReplayAwareBuffer, api, openEventStream } from '../api';
import { buildConversationTimeline, parseStreamLine, rawVariant, renderTimelineItem } from '../messages';
import type { ConversationState, RawStreamEvent, SessionMeta, TimelineItem } from '../types';

class TcodeSessionView extends LitElement {
  static properties = {
    sessionId: { type: String },
  };

  sessionId = '';
  private meta: SessionMeta | null = null;
  private state: ConversationState | null = null;
  private statusText = '';
  private usageText = '';
  private tokenUsageText = '';
  private events: RawStreamEvent[] = [];
  private timeline: TimelineItem[] = [];
  private composerText = '';
  private loading = true;
  private streamState = 'connecting';
  private errorMessage = '';
  private actionMessage = '';
  private sending = false;
  private finishing = false;
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
    if (changed.has('sessionId')) {
      this.startView();
    }
  }

  private startView(): void {
    if (!this.sessionId) {
      return;
    }

    this.stopView();
    this.meta = null;
    this.state = null;
    this.statusText = '';
    this.usageText = '';
    this.tokenUsageText = '';
    this.events = [];
    this.timeline = [];
    this.loading = true;
    this.streamState = 'connecting';
    this.errorMessage = '';
    this.actionMessage = '';
    this.replayBuffer.reset();
    this.requestUpdate();
    void this.refreshSnapshots(true);
    this.openStream();
    this.pollHandle = window.setInterval(() => {
      void this.refreshSnapshots(false);
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

  private async refreshSnapshots(initial: boolean): Promise<void> {
    try {
      const [meta, state, statusText, usageText, tokenUsageText] = await Promise.all([
        api.getSessionMeta(this.sessionId),
        api.getSessionConversationState(this.sessionId),
        api.getSessionStatus(this.sessionId),
        api.getSessionUsage(this.sessionId),
        api.getSessionTokenUsage(this.sessionId),
      ]);
      this.meta = meta;
      this.state = state;
      this.statusText = statusText.trim();
      this.usageText = usageText.trim();
      this.tokenUsageText = tokenUsageText.trim();
      this.errorMessage = '';
      if (initial) {
        this.loading = false;
      }
      this.requestUpdate();
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) {
        this.errorMessage = 'Session snapshots are not available yet. Waiting for runtime output…';
      } else {
        this.errorMessage = error instanceof Error ? error.message : 'Failed to load session data';
      }
      if (initial) {
        this.loading = false;
      }
      this.requestUpdate();
    }
  }

  private openStream(): void {
    this.eventSource?.close();
    this.replayBuffer.beginReplay();
    const source = openEventStream(api.sessionDisplayPath(this.sessionId));
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

  private onComposerInput(event: Event): void {
    const target = event.target as HTMLTextAreaElement;
    this.composerText = target.value;
    this.requestUpdate();
  }

  private async submitMessage(event: Event): Promise<void> {
    event.preventDefault();
    const text = this.composerText.trim();
    if (!text || this.sending) {
      return;
    }

    this.sending = true;
    this.actionMessage = '';
    this.errorMessage = '';
    this.requestUpdate();

    try {
      await api.sendSessionMessage(this.sessionId, text);
      this.composerText = '';
      this.actionMessage = 'Message sent.';
      this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
    } catch (error) {
      this.errorMessage = error instanceof Error ? error.message : 'Failed to send message';
    } finally {
      this.sending = false;
      this.requestUpdate();
    }
  }

  private async finishConversation(): Promise<void> {
    if (this.finishing) {
      return;
    }

    this.finishing = true;
    this.actionMessage = '';
    this.errorMessage = '';
    this.requestUpdate();

    try {
      await api.finishSession(this.sessionId);
      this.actionMessage = 'Finish requested.';
      this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
    } catch (error) {
      this.errorMessage = error instanceof Error ? error.message : 'Failed to finish conversation';
    } finally {
      this.finishing = false;
      this.requestUpdate();
    }
  }

  private async cancelConversation(): Promise<void> {
    if (this.cancelling) {
      return;
    }

    this.cancelling = true;
    this.actionMessage = '';
    this.errorMessage = '';
    this.requestUpdate();

    try {
      await api.cancelSession(this.sessionId);
      this.actionMessage = 'Cancel requested.';
      this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
    } catch (error) {
      this.errorMessage = error instanceof Error ? error.message : 'Failed to cancel conversation';
    } finally {
      this.cancelling = false;
      this.requestUpdate();
    }
  }

  private renderStats() {
    return html`
      <section class="panel-grid">
        <section class="panel">
          <h3>Session snapshot</h3>
          <dl class="meta-list">
            <div>
              <dt>Description</dt>
              <dd>${this.meta?.description || '—'}</dd>
            </div>
            <div>
              <dt>Model</dt>
              <dd>${this.state?.model || '—'}</dd>
            </div>
            <div>
              <dt>Created</dt>
              <dd>${this.meta?.created_at ? new Date(this.meta.created_at).toLocaleString() : '—'}</dd>
            </div>
            <div>
              <dt>Last active</dt>
              <dd>
                ${this.meta?.last_active_at ? new Date(this.meta.last_active_at).toLocaleString() : '—'}
              </dd>
            </div>
            <div>
              <dt>Conversation id</dt>
              <dd>${this.state?.id || 'pending'}</dd>
            </div>
            <div>
              <dt>Stream</dt>
              <dd>${this.streamState}</dd>
            </div>
          </dl>
        </section>
        <section class="panel">
          <h3>Usage</h3>
          <dl class="meta-list">
            <div>
              <dt>Total input</dt>
              <dd>${this.state?.total_input_tokens ?? 0}</dd>
            </div>
            <div>
              <dt>Total output</dt>
              <dd>${this.state?.total_output_tokens ?? 0}</dd>
            </div>
            <div>
              <dt>Aggregate input</dt>
              <dd>${this.state?.aggregate_input_tokens ?? 0}</dd>
            </div>
            <div>
              <dt>Aggregate output</dt>
              <dd>${this.state?.aggregate_output_tokens ?? 0}</dd>
            </div>
          </dl>
          ${this.usageText ? html`<pre class="timeline-pre">${this.usageText}</pre>` : nothing}
          ${this.tokenUsageText ? html`<pre class="timeline-pre">${this.tokenUsageText}</pre>` : nothing}
        </section>
      </section>
    `;
  }

  render() {
    return html`
      <section class="page">
        <header class="page-header">
          <div>
            <h1 class="page-title">Session ${this.sessionId}</h1>
            <p class="page-subtitle">Live conversation stream with polling snapshots and session controls.</p>
          </div>
          <div class="header-actions">
            <span class="pill pill-${this.streamState}">${this.streamState}</span>
            <button class="button secondary" @click=${this.finishConversation} ?disabled=${this.finishing}>
              ${this.finishing ? 'Finishing…' : 'Finish'}
            </button>
            <button class="button danger" @click=${this.cancelConversation} ?disabled=${this.cancelling}>
              ${this.cancelling ? 'Cancelling…' : 'Cancel'}
            </button>
          </div>
        </header>

        ${this.errorMessage ? html`<div class="inline-alert error">${this.errorMessage}</div>` : nothing}
        ${this.actionMessage ? html`<div class="inline-alert info">${this.actionMessage}</div>` : nothing}
        ${this.renderStats()}

        <section class="panel">
          <h3>Composer</h3>
          <form class="composer" @submit=${this.submitMessage}>
            <textarea
              placeholder="Ask tcode to inspect code, run tools, or continue the session…"
              .value=${this.composerText}
              @input=${this.onComposerInput}
            ></textarea>
            <div class="form-actions">
              <button class="button" type="submit" ?disabled=${this.sending || !this.composerText.trim()}>
                ${this.sending ? 'Sending…' : 'Send message'}
              </button>
              <span class="muted">Tip: the backend only exposes root-session messaging, so this composer targets the main session conversation.</span>
            </div>
          </form>
        </section>

        <section class="panel">
          <h3>Status</h3>
          <pre class="timeline-pre">${this.statusText || 'No status text yet.'}</pre>
        </section>

        <section class="panel">
          <h3>Conversation</h3>
          ${this.loading
            ? html`<div class="timeline-empty">Loading session…</div>`
            : this.timeline.length
              ? html`
                  <div class="timeline">
                    ${this.timeline.map((item) =>
                      renderTimelineItem(item, {
                        sessionId: this.sessionId,
                      }),
                    )}
                  </div>
                `
              : html`<div class="timeline-empty">Waiting for streamed events…</div>`}
        </section>
      </section>
    `;
  }
}

customElements.define('tcode-session-view', TcodeSessionView);
