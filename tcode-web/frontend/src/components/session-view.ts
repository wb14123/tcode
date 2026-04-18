import { LitElement, html, nothing } from 'lit';

import { ApiError, ReplayAwareBuffer, api, openEventStream } from '../api';
import { buildConversationTimeline, parseStreamLine, rawVariant, renderTimelineItem } from '../messages';
import type { ConversationState, RawStreamEvent, SessionMeta, TimelineItem } from '../types';

interface ToastNotice {
  id: number;
  tone: 'error' | 'info';
  message: string;
}

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
  private sending = false;
  private cancelling = false;
  private pollHandle: number | null = null;
  private eventSource: EventSource | null = null;
  private replayBuffer = new ReplayAwareBuffer();
  private toasts: ToastNotice[] = [];
  private toastCounter = 0;
  private toastTimeouts = new Map<number, number>();
  private detailsOpen = false;
  private expandedSubagentIds = new Set<string>();
  private stickToBottom = true;
  private lastSnapshotError = '';

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
    for (const timeout of this.toastTimeouts.values()) {
      window.clearTimeout(timeout);
    }
    this.toastTimeouts.clear();
  }

  updated(changed: Map<string, unknown>): void {
    if (changed.has('sessionId')) {
      this.startView();
    }

    this.syncComposerHeight();
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
    this.sending = false;
    this.cancelling = false;
    this.detailsOpen = false;
    this.expandedSubagentIds = new Set<string>();
    this.stickToBottom = true;
    this.lastSnapshotError = '';
    this.clearToasts();
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

  private clearToasts(): void {
    for (const timeout of this.toastTimeouts.values()) {
      window.clearTimeout(timeout);
    }
    this.toastTimeouts.clear();
    this.toasts = [];
  }

  private showToast(message: string, tone: 'error' | 'info', durationMs = 5000): void {
    const id = ++this.toastCounter;
    const timeout = window.setTimeout(() => {
      this.dismissToast(id);
    }, durationMs);

    this.toastTimeouts.set(id, timeout);
    this.toasts = [...this.toasts, { id, tone, message }];
    this.requestUpdate();
  }

  private dismissToast(id: number): void {
    const timeout = this.toastTimeouts.get(id);
    if (timeout !== undefined) {
      window.clearTimeout(timeout);
      this.toastTimeouts.delete(id);
    }

    const nextToasts = this.toasts.filter((toast) => toast.id !== id);
    if (nextToasts.length !== this.toasts.length) {
      this.toasts = nextToasts;
      this.requestUpdate();
    }
  }

  private combinedUsageText(): string {
    return [this.tokenUsageText, this.usageText].filter((value) => value.trim()).join(' │ ');
  }

  private statusTone(): 'generating' | 'idle' | 'connecting' {
    if (this.loading && !this.statusText.trim()) {
      return 'connecting';
    }
    if (this.isGenerating()) {
      return 'generating';
    }
    return 'idle';
  }

  private statusSummary(): string {
    if (this.statusText.trim()) {
      return this.statusText.trim();
    }
    if (this.loading) {
      return 'Connecting…';
    }
    return 'Ready';
  }

  private isGenerating(): boolean {
    const status = this.statusText.trim().toLowerCase();
    return status.includes('stream') || status.includes('thinking');
  }

  private syncComposerHeight(textarea?: HTMLTextAreaElement | null): void {
    const composerInput = textarea ?? this.querySelector<HTMLTextAreaElement>('.chat-composer-input');
    if (!composerInput) {
      return;
    }

    composerInput.style.height = 'auto';
    const maxHeight = Number.parseFloat(window.getComputedStyle(composerInput).maxHeight) || 160;
    const nextHeight = Math.min(composerInput.scrollHeight, maxHeight);
    composerInput.style.height = `${nextHeight}px`;
    composerInput.style.overflowY = composerInput.scrollHeight > maxHeight ? 'auto' : 'hidden';
  }

  private renderSendIcon() {
    return html`
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M3.4 20.4 19.85 13.35a1.5 1.5 0 0 0 0-2.76L3.4 3.6a1 1 0 0 0-1.37 1.22l2.36 6.49a1 1 0 0 0 .94.66h7.36a1 1 0 1 1 0 2H5.33a1 1 0 0 0-.94.66l-2.36 6.49A1 1 0 0 0 3.4 20.4Z"></path>
      </svg>
    `;
  }

  private renderCancelIcon() {
    return html`
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M7 7h10v10H7z"></path>
      </svg>
    `;
  }

  private async scheduleScrollToBottom(force = false): Promise<void> {
    if (!force && !this.stickToBottom) {
      return;
    }

    await this.updateComplete;
    window.requestAnimationFrame(() => {
      window.requestAnimationFrame(() => {
        const scroller = this.querySelector<HTMLElement>('.chat-scroll-area');
        if (!scroller) {
          return;
        }
        scroller.scrollTop = scroller.scrollHeight;
      });
    });
  }

  private onChatScroll = (event: Event): void => {
    const target = event.currentTarget;
    if (!(target instanceof HTMLElement)) {
      return;
    }

    const remaining = target.scrollHeight - target.scrollTop - target.clientHeight;
    this.stickToBottom = remaining < 80;
  };

  private toggleDetails = (): void => {
    this.detailsOpen = !this.detailsOpen;
    this.requestUpdate();
  };

  private closeDetails = (): void => {
    this.detailsOpen = false;
    this.requestUpdate();
  };

  private toggleSubagentExpansion = (conversationId: string): void => {
    const nextExpanded = new Set(this.expandedSubagentIds);
    if (nextExpanded.has(conversationId)) {
      nextExpanded.delete(conversationId);
    } else {
      nextExpanded.add(conversationId);
    }
    this.expandedSubagentIds = nextExpanded;
    this.requestUpdate();
  };

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
      this.lastSnapshotError = '';
      if (initial) {
        this.loading = false;
        this.scheduleScrollToBottom(true);
      }
      this.requestUpdate();
    } catch (error) {
      const message =
        error instanceof ApiError && error.status === 404
          ? 'Session snapshots are not available yet. Waiting for runtime output…'
          : error instanceof Error
            ? error.message
            : 'Failed to load session data';

      if (message !== this.lastSnapshotError) {
        this.showToast(message, 'error');
        this.lastSnapshotError = message;
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
      this.scheduleScrollToBottom();
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
    this.syncComposerHeight(target);
    this.requestUpdate();
  }

  private onComposerKeyDown = (event: KeyboardEvent): void => {
    if (
      event.key !== 'Enter' ||
      event.shiftKey ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.isComposing
    ) {
      return;
    }

    event.preventDefault();
    void this.submitMessage(event);
  };

  private async submitMessage(event: Event): Promise<void> {
    event.preventDefault();
    const text = this.composerText.trim();
    if (!text || this.sending || this.isGenerating()) {
      return;
    }

    this.sending = true;
    this.requestUpdate();

    try {
      await api.sendSessionMessage(this.sessionId, text);
      this.composerText = '';
      this.requestUpdate();
      await this.updateComplete;
      this.syncComposerHeight();
      this.showToast('Message sent.', 'info', 3000);
      this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
      this.scheduleScrollToBottom(true);
    } catch (error) {
      this.showToast(error instanceof Error ? error.message : 'Failed to send message', 'error');
    } finally {
      this.sending = false;
      this.requestUpdate();
    }
  }

  private async cancelConversation(): Promise<void> {
    if (this.cancelling || !this.isGenerating()) {
      return;
    }

    this.cancelling = true;
    this.requestUpdate();

    try {
      await api.cancelSession(this.sessionId);
      this.showToast('Cancel requested.', 'info', 3000);
      this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
    } catch (error) {
      this.showToast(error instanceof Error ? error.message : 'Failed to cancel conversation', 'error');
    } finally {
      this.cancelling = false;
      this.requestUpdate();
    }
  }

  private renderDetailsModal() {
    if (!this.detailsOpen) {
      return nothing;
    }

    return html`
      <div class="modal-backdrop" @click=${this.closeDetails}>
        <section class="modal-card session-details-modal" @click=${(event: Event) => event.stopPropagation()}>
          <div class="session-details-header">
            <div>
              <h2 class="page-title">Session details</h2>
              <p class="page-subtitle">Less-frequent metadata lives here instead of taking over the chat view.</p>
            </div>
            <button class="button secondary" type="button" @click=${this.closeDetails}>Close</button>
          </div>

          <dl class="meta-list session-details-list">
            <div>
              <dt>Description</dt>
              <dd>${this.meta?.description || '—'}</dd>
            </div>
            <div>
              <dt>Model</dt>
              <dd>${this.state?.model || '—'}</dd>
            </div>
            <div>
              <dt>Session id</dt>
              <dd>${this.sessionId}</dd>
            </div>
            <div>
              <dt>Conversation id</dt>
              <dd>${this.state?.id || 'pending'}</dd>
            </div>
            <div>
              <dt>Created</dt>
              <dd>${this.meta?.created_at ? new Date(this.meta.created_at).toLocaleString() : '—'}</dd>
            </div>
            <div>
              <dt>Last active</dt>
              <dd>${this.meta?.last_active_at ? new Date(this.meta.last_active_at).toLocaleString() : '—'}</dd>
            </div>
            <div>
              <dt>Transport</dt>
              <dd>${this.streamState}</dd>
            </div>
            <div>
              <dt>Status</dt>
              <dd>${this.statusSummary()}</dd>
            </div>
          </dl>

          ${this.tokenUsageText
            ? html`
                <section>
                  <h3>Token usage</h3>
                  <pre class="timeline-pre">${this.tokenUsageText}</pre>
                </section>
              `
            : nothing}
          ${this.usageText
            ? html`
                <section>
                  <h3>Usage</h3>
                  <pre class="timeline-pre">${this.usageText}</pre>
                </section>
              `
            : nothing}
        </section>
      </div>
    `;
  }

  private renderToasts() {
    if (!this.toasts.length) {
      return nothing;
    }

    return html`
      <div class="toast-stack" aria-live="polite" aria-atomic="true">
        ${this.toasts.map(
          (toast) => html`
            <div class="toast toast-${toast.tone}" role="status">
              <div class="toast-message">${toast.message}</div>
              <button class="toast-close" type="button" @click=${() => this.dismissToast(toast.id)} aria-label="Dismiss notification">
                ×
              </button>
            </div>
          `,
        )}
      </div>
    `;
  }

  render() {
    const combinedUsage = this.combinedUsageText();
    const statusTone = this.statusTone();
    const showProgress = this.loading || this.sending || this.isGenerating();

    return html`
      <section class="page chat-page">
        <div class="chat-shell">
          ${showProgress
            ? html`
                <div class="chat-progress-wrap" aria-label="Conversation progress">
                  <div class="chat-progress-bar"></div>
                </div>
              `
            : nothing}
          <section class="chat-scroll-area" @scroll=${this.onChatScroll}>
            ${this.loading
              ? html`<div class="chat-empty-state">Loading session…</div>`
              : this.timeline.length
                ? html`
                    <div class="timeline chat-timeline">
                      ${this.timeline.map((item) =>
                        renderTimelineItem(item, {
                          sessionId: this.sessionId,
                          expandedSubagentIds: this.expandedSubagentIds,
                          toggleSubagentExpansion: this.toggleSubagentExpansion,
                        }),
                      )}
                    </div>
                  `
                : html`
                    <div class="chat-empty-state">
                      Waiting for streamed events… Send a message below to get started.
                    </div>
                  `}
          </section>

          <div class="chat-bottom-stack">
            <footer class="chat-status-bar">
              <span class="pill pill-${statusTone}">${this.statusSummary()}</span>
              ${combinedUsage ? html`<span class="chat-status-divider">│</span><span class="chat-usage-text">${combinedUsage}</span>` : nothing}
            </footer>

            <form class="panel chat-composer" @submit=${this.submitMessage}>
              <div class="chat-composer-row">
                <textarea
                  class="chat-composer-input"
                  rows="1"
                  placeholder="Message tcode…"
                  .value=${this.composerText}
                  @input=${this.onComposerInput}
                  @keydown=${this.onComposerKeyDown}
                ></textarea>
                ${this.isGenerating()
                  ? html`
                      <button
                        class="button danger chat-composer-action"
                        type="button"
                        @click=${this.cancelConversation}
                        ?disabled=${this.cancelling}
                        aria-label=${this.cancelling ? 'Cancelling conversation' : 'Cancel conversation'}
                        title=${this.cancelling ? 'Cancelling…' : 'Cancel conversation'}
                      >
                        ${this.renderCancelIcon()}
                      </button>
                    `
                  : html`
                      <button
                        class="button chat-composer-action"
                        type="submit"
                        ?disabled=${this.sending || !this.composerText.trim()}
                        aria-label=${this.sending ? 'Sending message' : 'Send message'}
                        title=${this.sending ? 'Sending…' : 'Send message'}
                      >
                        ${this.renderSendIcon()}
                      </button>
                    `}
              </div>
            </form>
          </div>
        </div>

        ${this.renderDetailsModal()} ${this.renderToasts()}
      </section>
    `;
  }
}

customElements.define('tcode-session-view', TcodeSessionView);
