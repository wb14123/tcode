import { LitElement, html, nothing } from 'lit';

import { ApiError, api, openEventStream, sessionLeaseManager, type LeaseSnapshot } from '../api';
import { buildConversationTimeline, extractSystemNotification, parseStreamLine, rawVariant, renderTimelineItem } from '../messages';
import type { RawStreamEvent, TimelineItem } from '../types';

interface ToastNotice {
  id: number;
  tone: 'error' | 'info';
  message: string;
}

class TcodeSubagentView extends LitElement {
  static properties = {
    sessionId: { type: String },
    subagentId: { type: String },
  };

  sessionId = '';
  subagentId = '';
  private statusText = '';
  private tokenUsageText = '';
  private events: RawStreamEvent[] = [];
  private timeline: TimelineItem[] = [];
  private composerText = '';
  private loading = true;
  private sending = false;
  private cancelling = false;
  private finishing = false;
  private pollHandle: number | null = null;
  private eventSource: EventSource | null = null;
  private leaseRelease: (() => void) | null = null;
  private toasts: ToastNotice[] = [];
  private toastCounter = 0;
  private toastTimeouts = new Map<number, number>();
  private expandedSubagentIds = new Set<string>();
  private stickToBottom = true;
  private lastSnapshotError = '';
  private sessionDisconnected = false;
  private reconnecting = false;
  private leaseErrorMessage = '';
  private lastLeaseError = '';

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
    if (changed.has('sessionId') || changed.has('subagentId')) {
      this.startView();
    }

    this.syncComposerHeight();
  }

  private startView(): void {
    if (!this.sessionId || !this.subagentId) {
      return;
    }

    this.stopView();
    this.statusText = '';
    this.tokenUsageText = '';
    this.events = [];
    this.timeline = [];
    this.composerText = '';
    this.loading = true;
    this.sending = false;
    this.cancelling = false;
    this.finishing = false;
    this.expandedSubagentIds = new Set<string>();
    this.stickToBottom = true;
    this.lastSnapshotError = '';
    this.sessionDisconnected = false;
    this.reconnecting = false;
    this.leaseErrorMessage = '';
    this.lastLeaseError = '';
    this.clearToasts();
    this.requestUpdate();
    void this.refreshSnapshots(true);
    this.attachLease();
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

    this.leaseRelease?.();
    this.leaseRelease = null;

    this.eventSource?.close();
    this.eventSource = null;
  }

  private attachLease(): void {
    this.leaseRelease?.();
    this.leaseRelease = null;
    if (this.sessionId) {
      this.leaseRelease = sessionLeaseManager.attach(this.sessionId, (snapshot) => this.onLeaseSnapshot(snapshot));
    }
  }

  private onLeaseSnapshot(snapshot: LeaseSnapshot): void {
    if (snapshot.sessionId !== this.sessionId) {
      return;
    }

    this.sessionDisconnected = snapshot.disconnected;
    this.reconnecting = snapshot.reconnecting;
    this.leaseErrorMessage = snapshot.errorMessage;
    if (snapshot.errorMessage && snapshot.errorMessage !== this.lastLeaseError) {
      this.lastLeaseError = snapshot.errorMessage;
      this.showToast(snapshot.errorMessage, 'error');
    }
    if (!snapshot.errorMessage) {
      this.lastLeaseError = '';
    }
    this.requestUpdate();
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
    return this.tokenUsageText.trim();
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
    if (this.sessionDisconnected) {
      return 'Disconnected';
    }
    if (this.reconnecting) {
      return 'Reconnecting…';
    }
    if (this.statusText.trim()) {
      return this.statusText.trim();
    }
    if (this.loading) {
      return 'Connecting…';
    }
    return 'Ready';
  }

  private isGenerating(): boolean {
    if (this.sessionDisconnected) {
      return false;
    }
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

  private mutationDisabled(): boolean {
    return this.sessionDisconnected || this.reconnecting;
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
      const [statusText, tokenUsageText] = await Promise.all([
        api.getSubagentStatus(this.sessionId, this.subagentId),
        api.getSubagentTokenUsage(this.sessionId, this.subagentId),
      ]);
      this.statusText = statusText.trim();
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
          ? 'Subagent snapshot files are missing or not available yet; this may be historical/incomplete output or runtime output may still be pending.'
          : error instanceof Error
            ? error.message
            : 'Failed to load subagent data';

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
    const source = openEventStream(api.subagentDisplayPath(this.sessionId, this.subagentId));
    this.eventSource = source;

    source.onopen = () => {
      this.requestUpdate();
    };

    source.onmessage = (message) => {
      const raw = message.data;
      if (typeof raw !== 'string') {
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
      this.scheduleScrollToBottom();
    };

    source.onerror = () => {
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
    if (!text || this.sending || this.finishing || this.isGenerating() || this.mutationDisabled()) {
      return;
    }

    this.sending = true;
    this.requestUpdate();

    try {
      await api.sendSubagentMessage(this.sessionId, this.subagentId, text);
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

  private canFinishConversation(): boolean {
    for (let index = this.timeline.length - 1; index >= 0; index -= 1) {
      const item = this.timeline[index];
      if (item.kind === 'assistant') {
        return Boolean(item.content.trim());
      }
      if (item.kind === 'user') {
        return false;
      }
    }
    return false;
  }

  private async cancelConversation(): Promise<void> {
    if (this.cancelling || !this.isGenerating() || this.mutationDisabled()) {
      return;
    }

    this.cancelling = true;
    this.requestUpdate();

    try {
      await api.cancelSubagent(this.sessionId, this.subagentId);
      this.showToast('Cancel requested.', 'info', 3000);
      this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
    } catch (error) {
      this.showToast(error instanceof Error ? error.message : 'Failed to cancel subagent', 'error');
    } finally {
      this.cancelling = false;
      this.requestUpdate();
    }
  }

  private async finishConversation(): Promise<void> {
    if (this.finishing || this.sending || this.isGenerating() || !this.canFinishConversation() || this.mutationDisabled()) {
      return;
    }

    this.finishing = true;
    this.requestUpdate();

    try {
      await api.finishSubagent(this.sessionId, this.subagentId);
      this.showToast('Done requested.', 'info', 3000);
      this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
    } catch (error) {
      this.showToast(error instanceof Error ? error.message : 'Failed to mark subagent done', 'error');
    } finally {
      this.finishing = false;
      this.requestUpdate();
    }
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
    const canFinish = this.canFinishConversation();
    const mutationsDisabled = this.mutationDisabled();
    const leaseAlert = this.sessionDisconnected
      ? this.leaseErrorMessage || 'Session runtime disconnected. Reconnect from the session view before changing this subagent.'
      : '';
    const showProgress = this.loading || this.sending || this.finishing || this.isGenerating();

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
              ? html`<div class="chat-empty-state">Loading subagent…</div>`
              : this.timeline.length
                ? html`
                    <div class="timeline chat-timeline">
                      ${this.timeline.map((item) =>
                        renderTimelineItem(item, {
                          sessionId: this.sessionId,
                          currentSubagentId: this.subagentId,
                          expandedSubagentIds: this.expandedSubagentIds,
                          toggleSubagentExpansion: this.toggleSubagentExpansion,
                        }),
                      )}
                    </div>
                  `
                : html`<div class="chat-empty-state">Waiting for subagent events… Send a message below to continue.</div>`}
          </section>

          <div class="chat-bottom-stack">
            <footer class="chat-status-bar">
              <span class="pill pill-${statusTone}">${this.statusSummary()}</span>
              ${combinedUsage ? html`<span class="chat-status-divider">│</span><span class="chat-usage-text">${combinedUsage}</span>` : nothing}
            </footer>

            ${leaseAlert ? html`<div class="inline-alert error">${leaseAlert}</div>` : nothing}

            <form class="panel chat-composer" @submit=${this.submitMessage}>
              <div class="chat-composer-row">
                <textarea
                  class="chat-composer-input"
                  rows="1"
                  placeholder="Message subagent…"
                  .value=${this.composerText}
                  @input=${this.onComposerInput}
                  @keydown=${this.onComposerKeyDown}
                  ?disabled=${mutationsDisabled}
                ></textarea>
                <div class="chat-composer-actions">
                  ${this.isGenerating()
                    ? nothing
                    : html`
                        <button
                          class="button secondary chat-composer-done"
                          type="button"
                          @click=${this.finishConversation}
                          ?disabled=${mutationsDisabled || this.finishing || this.sending || !canFinish}
                          title=${
                            this.finishing
                              ? 'Marking done…'
                              : canFinish
                                ? 'Done with this subagent for now'
                                : 'Wait for a completed subagent reply before sending it back'
                          }
                        >
                          ${this.finishing ? 'Done…' : 'Done'}
                        </button>
                      `}
                  ${this.isGenerating()
                    ? html`
                        <button
                          class="button danger chat-composer-action"
                          type="button"
                          @click=${this.cancelConversation}
                          ?disabled=${mutationsDisabled || this.cancelling}
                          aria-label=${this.cancelling ? 'Cancelling subagent' : 'Cancel subagent'}
                          title=${this.cancelling ? 'Cancelling…' : 'Cancel subagent'}
                        >
                          ${this.renderCancelIcon()}
                        </button>
                      `
                    : html`
                        <button
                          class="button chat-composer-action"
                          type="submit"
                          ?disabled=${mutationsDisabled || this.sending || this.finishing || !this.composerText.trim()}
                          aria-label=${this.sending ? 'Sending message' : 'Send message'}
                          title=${this.sending ? 'Sending…' : 'Send message'}
                        >
                          ${this.renderSendIcon()}
                        </button>
                      `}
                </div>
              </div>
            </form>
          </div>
        </div>

        ${this.renderToasts()}
      </section>
    `;
  }
}

customElements.define('tcode-subagent-view', TcodeSubagentView);
