import { LitElement, html, nothing } from 'lit';

import { ApiError, api, openEventStream, sessionLeaseManager, type LeaseSnapshot } from '../api';
import { navigate } from '../router';
import { StreamEventBatcher } from '../stream-event-batcher';
import { ConversationTimelineBuilder, extractSystemNotification, parseStreamLine, rawVariant } from '../messages';
import { SUPPORTED_IMAGE_TYPES } from '../image-types';
import type { MessageSubmitDetail } from './composer';

import './composer';
import './timeline';

interface ToastNotice {
  id: number;
  tone: 'error' | 'info';
  message: string;
}

class TcodeSessionView extends LitElement {
  static properties = {
    sessionId: { type: String },
    draftMode: { type: Boolean },
    draftVersion: { type: Number },
  };

  sessionId = '';
  draftMode = false;
  draftVersion = 0;
  private statusText = '';
  private usageText = '';
  private tokenUsageText = '';
  private timelineBuilder = new ConversationTimelineBuilder();
  private streamBatcher = new StreamEventBatcher((events) => {
    this.timelineBuilder.appendEvents(events);
    this.requestUpdate();
  });
  private composerResetToken = 0;
  private timelineScrollToken = 0;
  private loading = true;
  private sending = false;
  private cancelling = false;
  private pollHandle: number | null = null;
  private eventSource: EventSource | null = null;
  private leaseRelease: (() => void) | null = null;
  private sessionDisconnected = false;
  private reconnecting = false;
  private lastLeaseError = '';
  private toasts: ToastNotice[] = [];
  private toastCounter = 0;
  private toastTimeouts = new Map<number, number>();
  private lastSnapshotError = '';
  private streamReconnectHandle: number | null = null;
  private streamRetryDelayMs = 1000;
  private streamEventsReceived = 0;
  // Map<File, string> relies on JS Map reference equality for File keys.
  // The composer must preserve the same File objects across submits for
  // retry deduplication to work (currently it does via [...this.imageFiles]).
  private pendingUploadMap: Map<File, string> | null = null;
  private draftSessionId: string | null = null;
  private uploadProgress: string | null = null;

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
    if (changed.has('sessionId') || changed.has('draftMode') || changed.has('draftVersion')) {
      this.startView();
    }
  }

  private startView(): void {
    this.stopView();
    this.statusText = '';
    this.usageText = '';
    this.tokenUsageText = '';
    this.timelineBuilder.reset();
    this.composerResetToken += 1;
    this.loading = true;
    this.sending = false;
    this.cancelling = false;
    this.sessionDisconnected = false;
    this.reconnecting = false;
    this.timelineScrollToken += 1;
    this.lastSnapshotError = '';
    this.lastLeaseError = '';
    this.streamRetryDelayMs = 1000;
    this.streamEventsReceived = 0;
    this.pendingUploadMap = null;
    this.draftSessionId = null;
    this.uploadProgress = null;
    this.clearToasts();

    if (!this.sessionId) {
      this.loading = false;
      this.requestUpdate();
      return;
    }

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

    if (this.streamReconnectHandle !== null) {
      window.clearTimeout(this.streamReconnectHandle);
      this.streamReconnectHandle = null;
    }

    this.streamBatcher.clear();
  }

  private clearToasts(): void {
    for (const timeout of this.toastTimeouts.values()) {
      window.clearTimeout(timeout);
    }
    this.toastTimeouts.clear();
    this.toasts = [];
  }

  private attachLease(): void {
    this.leaseRelease?.();
    this.leaseRelease = null;
    if (!this.sessionId || this.draftMode) {
      return;
    }
    this.leaseRelease = sessionLeaseManager.attach(this.sessionId, (snapshot) => this.onLeaseSnapshot(snapshot));
  }

  private onLeaseSnapshot(snapshot: LeaseSnapshot): void {
    if (snapshot.sessionId !== this.sessionId) {
      return;
    }
    this.sessionDisconnected = snapshot.disconnected;
    this.reconnecting = snapshot.reconnecting;
    if (snapshot.errorMessage && snapshot.errorMessage !== this.lastLeaseError) {
      this.lastLeaseError = snapshot.errorMessage;
      this.showToast(snapshot.errorMessage, 'error');
    }
    if (!snapshot.errorMessage) {
      this.lastLeaseError = '';
    }
    this.requestUpdate();
  }

  private async reconnectSession(): Promise<void> {
    if (!this.sessionId || this.reconnecting) {
      return;
    }

    this.reconnecting = true;
    this.requestUpdate();
    try {
      await sessionLeaseManager.reconnect(this.sessionId);
      if (!this.sessionDisconnected) {
        this.showToast('Session reconnected.', 'info', 3000);
        this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
        void this.refreshSnapshots(false);
      }
    } finally {
      this.reconnecting = false;
      this.requestUpdate();
    }
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

  private onComposerNotification(event: CustomEvent<{ message: string; tone: 'error' | 'info' }>): void {
    this.showToast(event.detail.message, event.detail.tone);
  }

  private combinedUsageText(): string {
    return [this.tokenUsageText, this.usageText].filter((value) => value.trim()).join(' │ ');
  }

  private isGenerating(): boolean {
    if (this.mutationDisabled()) {
      return false;
    }
    const status = this.statusText.trim().toLowerCase();
    return status.includes('stream') || status.includes('thinking') || this.timelineBuilder.hasActiveWork();
  }

  private mutationDisabled(): boolean {
    return this.sessionDisconnected || this.reconnecting;
  }

  private async refreshSnapshots(initial: boolean): Promise<void> {
    try {
      const [statusText, usageText, tokenUsageText] = await Promise.all([
        api.getSessionStatus(this.sessionId),
        api.getSessionUsage(this.sessionId),
        api.getSessionTokenUsage(this.sessionId),
      ]);
      this.statusText = statusText.trim();
      this.usageText = usageText.trim();
      this.tokenUsageText = tokenUsageText.trim();
      this.lastSnapshotError = '';
      if (initial) {
        this.loading = false;
        this.timelineScrollToken += 1;
      }
      this.requestUpdate();
    } catch (error) {
      const message =
        error instanceof ApiError && error.status === 404
          ? 'Session snapshot files are missing or not available yet; this may be a historical/incomplete session or runtime output may still be pending.'
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

  private resetTimelineFromReplay(): void {
    this.timelineBuilder.reset();
  }

  private closeStream(): void {
    this.eventSource?.close();
    this.eventSource = null;
  }

  private scheduleStreamReconnect(): void {
    if (this.streamReconnectHandle !== null || !this.sessionId || this.draftMode) {
      return;
    }

    const delayMs = this.streamRetryDelayMs;
    this.streamRetryDelayMs = Math.min(this.streamRetryDelayMs * 2, 10000);
    this.streamReconnectHandle = window.setTimeout(() => {
      this.streamReconnectHandle = null;
      if (!this.isConnected || !this.sessionId || this.draftMode) {
        return;
      }
      this.restartStreamFromBeginning();
    }, delayMs);
  }

  private restartStreamFromBeginning(): void {
    if (this.streamReconnectHandle !== null) {
      window.clearTimeout(this.streamReconnectHandle);
      this.streamReconnectHandle = null;
    }

    this.closeStream();
    this.resetTimelineFromReplay();
    this.requestUpdate();
    this.openStream();
  }

  private scheduleSendCatchUp(sessionId: string, eventsBeforeSend: number): void {
    window.setTimeout(() => {
      if (!this.isConnected || this.sessionId !== sessionId || this.draftMode) {
        return;
      }

      if (this.streamEventsReceived !== eventsBeforeSend) {
        return;
      }

      this.restartStreamFromBeginning();
    }, 1500);
  }

  private openStream(): void {
    if (this.eventSource || !this.sessionId || this.draftMode) {
      return;
    }
    const source = openEventStream(api.sessionDisplayPath(this.sessionId));
    this.eventSource = source;

    source.onopen = () => {
      if (this.eventSource !== source) {
        return;
      }
      this.streamRetryDelayMs = 1000;
      this.requestUpdate();
    };

    source.onmessage = (message) => {
      if (this.eventSource !== source) {
        return;
      }

      const raw = message.data;
      if (typeof raw !== 'string') {
        return;
      }

      const parsed = parseStreamLine(raw);
      if (!parsed) {
        return;
      }

      this.streamEventsReceived += 1;
      this.streamBatcher.enqueue(parsed);
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

    };

    source.onerror = () => {
      if (this.eventSource !== source) {
        return;
      }

      this.closeStream();
      this.requestUpdate();
      this.scheduleStreamReconnect();
    };
  }

  private async submitMessage(event: CustomEvent<MessageSubmitDetail>): Promise<void> {
    const { text, imageFiles } = event.detail;
    if (!text && (!imageFiles || imageFiles.length === 0)) {
      return;
    }
    if (this.sending || this.isGenerating() || this.mutationDisabled()) {
      return;
    }

    this.sending = true;
    this.requestUpdate();

    try {
      // Determine the effective session ID (cached draft, or existing)
      let effectiveSessionId: string;
      let isDraft = false;

      if (this.draftMode && !this.sessionId) {
        isDraft = true;
        if (!this.draftSessionId) {
          // Validate image types before creating session
          if (imageFiles && imageFiles.length > 0) {
            const unsupported = imageFiles.filter((f) => !SUPPORTED_IMAGE_TYPES.includes(f.type));
            if (unsupported.length > 0) {
              this.showToast(
                `Unsupported image type(s): ${unsupported.map((f) => `${f.name} (${f.type})`).join(', ')}. Supported formats: PNG, JPEG/JPG, GIF, WebP.`,
                'error',
              );
              return;
            }
          }
          const created = await api.createSession('');
          this.draftSessionId = created.id;
        }
        effectiveSessionId = this.draftSessionId;
      } else {
        effectiveSessionId = this.sessionId;
      }

      if (!effectiveSessionId) {
        return;
      }

      let filenames: string[] = [];

      if (imageFiles && imageFiles.length > 0) {
        // Initialize pendingUploadMap on first submit
        if (!this.pendingUploadMap) {
          this.pendingUploadMap = new Map();
        }

        // Cleanup stale entries for files no longer in imageFiles
        // (user may have removed an image between retries)
        for (const file of this.pendingUploadMap.keys()) {
          if (!imageFiles.includes(file)) {
            this.pendingUploadMap.delete(file);
          }
        }

        // Separate into already-uploaded and need-upload
        const needUpload: File[] = [];
        for (const file of imageFiles) {
          if (!this.pendingUploadMap.has(file)) {
            needUpload.push(file);
          }
        }

        // Upload each file sequentially, one at a time
        let failures = 0;
        const alreadyCount = imageFiles.length - needUpload.length;
        const totalCount = imageFiles.length;

        for (let i = 0; i < needUpload.length; i++) {
          const file = needUpload[i];
          this.uploadProgress = `Uploading images… [${alreadyCount + i + 1}/${totalCount}]`;
          this.requestUpdate();

          try {
            const result = await api.uploadSessionImages(effectiveSessionId, [file]);
            const filename = result.files[0]?.filename;
            if (filename) {
              this.pendingUploadMap.set(file, filename);
            }
          } catch {
            failures += 1;
          }
        }

        this.uploadProgress = null;

        // Collect all filenames from the map
        for (const file of imageFiles) {
          const filename = this.pendingUploadMap.get(file);
          if (filename) {
            filenames.push(filename);
          }
        }

        if (failures > 0) {
          // Partial failure: keep state for retry, don't send message
          this.showToast(
            `${failures} of ${totalCount} image${totalCount !== 1 ? 's' : ''} failed to upload. Please retry.`,
            'error',
          );
          return;
        }
      }

      // All images uploaded successfully (or no images) — send the message
      if (isDraft) {
        if (filenames.length > 0) {
          await api.sendSessionMessageWithImages(effectiveSessionId, text, filenames);
        } else {
          await api.sendSessionMessage(effectiveSessionId, text);
        }
        this.composerResetToken += 1;
        this.requestUpdate();
        this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
        navigate({ kind: 'session', sessionId: effectiveSessionId }, false);
        this.pendingUploadMap = null;
        this.draftSessionId = null;
      } else {
        const eventsBeforeSend = this.streamEventsReceived;
        if (filenames.length > 0) {
          await api.sendSessionMessageWithImages(effectiveSessionId, text || '', filenames);
        } else {
          await api.sendSessionMessage(effectiveSessionId, text);
        }
        this.scheduleSendCatchUp(effectiveSessionId, eventsBeforeSend);
        this.composerResetToken += 1;
        this.requestUpdate();
        this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));
        this.timelineScrollToken += 1;
        this.pendingUploadMap = null;
        this.draftSessionId = null;
      }
    } catch (error) {
      let message: string;
      if (error instanceof ApiError) {
        message = `Failed to send message: ${error.message}`;
      } else if (error instanceof Error) {
        message = `Failed to send message: ${error.message}`;
      } else {
        message = 'Failed to send message';
      }
      this.showToast(message, 'error');
    } finally {
      this.sending = false;
      this.uploadProgress = null;
      this.requestUpdate();
    }
  }

  private async cancelConversation(): Promise<void> {
    if (this.cancelling || !this.isGenerating() || this.mutationDisabled()) {
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
          <tcode-timeline
            .store=${this.timelineBuilder.store}
            .sessionId=${this.sessionId}
            .loading=${this.loading}
            .loadingMessage=${'Loading session…'}
            .emptyMessage=${this.draftMode && !this.sessionId
              ? 'Send a message below to start a new conversation.'
              : 'Waiting for streamed events… Send a message below to get started.'}
            .scrollToBottomToken=${this.timelineScrollToken}
          ></tcode-timeline>

          <div class="chat-bottom-stack">
            ${combinedUsage
              ? html`
                  <footer class="chat-status-bar">
                    <span class="chat-status-meta">
                      <span class="chat-usage-text">${combinedUsage}</span>
                    </span>
                  </footer>
                `
              : nothing}

            ${this.sessionDisconnected && this.sessionId
              ? html`
                  <div class="inline-alert warning">
                    Session runtime has ended or disconnected. Messages are disabled until you reconnect.
                    <button class="button secondary" type="button" @click=${this.reconnectSession} ?disabled=${this.reconnecting}>
                      ${this.reconnecting ? 'Reconnecting…' : 'Reconnect / resume'}
                    </button>
                  </div>
                `
              : nothing}

            ${this.uploadProgress
              ? html`<div class="upload-progress">${this.uploadProgress}</div>`
              : nothing}

            <tcode-composer
              .disconnected=${this.mutationDisabled()}
              .sending=${this.sending}
              .generating=${this.isGenerating()}
              .cancelling=${this.cancelling}
              .placeholder=${this.sessionDisconnected ? 'Reconnect session to send messages…' : this.reconnecting ? 'Waiting for session reconnect…' : 'Message tcode…'}
              .resetToken=${this.composerResetToken}
              @message-submit=${this.submitMessage}
              @cancel-requested=${this.cancelConversation}
              @composer-notification=${this.onComposerNotification}
            ></tcode-composer>
          </div>
        </div>

        ${this.renderToasts()}
      </section>
    `;
  }
}

customElements.define('tcode-session-view', TcodeSessionView);
