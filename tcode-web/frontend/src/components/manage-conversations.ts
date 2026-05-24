import { LitElement, html, nothing } from 'lit';

import { api } from '../api';
import { formatTimestamp } from '../formatting';
import type { SessionSummary } from '../types';

class TcodeManageConversations extends LitElement {
  static properties = {
    sessions: { type: Array },
  };

  sessions: SessionSummary[] = [];
  private selectedIds = new Set<string>();
  private moving = false;
  private showConfirm = false;
  private moveProgress: { current: number; total: number } | null = null;

  createRenderRoot(): this {
    return this;
  }

  private get selectedCount(): number {
    return this.selectedIds.size;
  }

  private get allSelected(): boolean {
    return this.sessions.length > 0 && this.selectedIds.size === this.sessions.length;
  }

  private toggleSelectAll(): void {
    if (this.allSelected) {
      this.selectedIds = new Set();
    } else {
      this.selectedIds = new Set(this.sessions.map((s) => s.id));
    }
    this.requestUpdate();
  }

  private toggleSession(sessionId: string): void {
    const next = new Set(this.selectedIds);
    if (next.has(sessionId)) {
      next.delete(sessionId);
    } else {
      next.add(sessionId);
    }
    this.selectedIds = next;
    this.requestUpdate();
  }

  private openConfirm(): void {
    this.showConfirm = true;
    this.requestUpdate();
  }

  private closeConfirm(): void {
    this.showConfirm = false;
    this.requestUpdate();
  }

  private async executeMove(): Promise<void> {
    const ids = Array.from(this.selectedIds);
    if (ids.length === 0 || this.moving) {
      return;
    }

    this.moving = true;
    this.moveProgress = { current: 0, total: ids.length };
    this.requestUpdate();

    let successCount = 0;

    for (let i = 0; i < ids.length; i++) {
      const id = ids[i];
      this.moveProgress = { current: i + 1, total: ids.length };
      this.requestUpdate();

      try {
        await api.deleteSession(id);
        successCount += 1;
        this.selectedIds.delete(id);
        this.sessions = this.sessions.filter((s) => s.id !== id);
      } catch (error) {
        const message = error instanceof Error ? error.message : 'Failed to delete session';
        this.dispatchEvent(
          new CustomEvent('system-notification', {
            detail: { message, level: 'error', createdAt: Date.now() },
            bubbles: true,
            composed: true,
          }),
        );
      }
    }

    this.moving = false;
    this.showConfirm = false;
    this.moveProgress = null;

    this.dispatchEvent(new CustomEvent('sessions-refresh-requested', { bubbles: true, composed: true }));

    if (successCount > 0) {
      this.dispatchEvent(
        new CustomEvent('system-notification', {
          detail: {
            message: `Moved ${successCount} conversation${successCount !== 1 ? 's' : ''} to trash`,
            level: 'info',
            createdAt: Date.now(),
          },
          bubbles: true,
          composed: true,
        }),
      );
    }

    this.requestUpdate();
  }

  private renderConfirmModal() {
    if (!this.showConfirm) {
      return nothing;
    }

    const count = this.selectedCount;

    return html`
      <div class="modal-backdrop" @click=${this.closeConfirm}>
        <div class="modal-card" @click=${(e: Event) => e.stopPropagation()}>
          <h2 class="page-title" style="margin:0;font-size:1.25rem;">Move ${count} conversation${count !== 1 ? 's' : ''} to trash?</h2>
          <p class="muted" style="margin:0;">This action cannot be undone from the web UI.</p>
          ${this.moving && this.moveProgress
            ? html`<p class="muted" style="margin:0;">Moving ${this.moveProgress.current}/${this.moveProgress.total}&hellip;</p>`
            : nothing}
          <div class="modal-actions" style="justify-content:flex-end;">
            <button class="button secondary" type="button" @click=${this.closeConfirm} ?disabled=${this.moving}>
              Cancel
            </button>
            <button class="button danger" type="button" @click=${() => void this.executeMove()} ?disabled=${this.moving}>
              Move to trash
            </button>
          </div>
        </div>
      </div>
    `;
  }

  render() {
    const count = this.selectedCount;

    return html`
      <div class="page manage-page">
        <div class="page-header">
          <h1 class="page-title" style="margin:0;">Manage Conversations</h1>
        </div>

        <div class="manage-toolbar">
          <label class="manage-checkbox-row">
            <input
              type="checkbox"
              .checked=${this.allSelected}
              .indeterminate=${count > 0 && !this.allSelected}
              @change=${this.toggleSelectAll}
            />
            ${this.allSelected ? 'Deselect all' : 'Select all'}
          </label>
          <span class="muted">${count > 0 ? `${count} selected` : `(${this.sessions.length})`}</span>
          <button
            class="button danger"
            type="button"
            ?disabled=${count === 0 || this.moving}
            @click=${this.openConfirm}
          >
            ${this.moving && this.moveProgress
              ? `Moving ${this.moveProgress.current}/${this.moveProgress.total}&hellip;`
              : 'Move to trash'}
          </button>
        </div>

        <section class="manage-session-list">
          ${this.sessions.length
            ? this.sessions.map(
                (session) => html`
                  <label class="manage-session-row">
                    <input
                      type="checkbox"
                      .checked=${this.selectedIds.has(session.id)}
                      @change=${() => this.toggleSession(session.id)}
                    />
                    <div class="manage-session-info">
                      <div class="manage-session-desc">${session.description || 'Untitled conversation'}</div>
                      <div class="manage-session-meta">
                        ${session.mode === 'web_only' ? 'web-only' : 'normal'}
                        &middot;
                        ${session.status || 'inactive'}
                        ${session.last_active_at ? html`&middot; ${formatTimestamp(session.last_active_at)}` : nothing}
                        ${!session.last_active_at && session.created_at ? html`&middot; ${formatTimestamp(session.created_at)}` : nothing}
                      </div>
                    </div>
                  </label>
                `,
              )
            : html`<div class="empty-state"><p class="empty-copy">No conversations to manage.</p></div>`}
        </section>
      </div>
      ${this.renderConfirmModal()}
    `;
  }
}

customElements.define('tcode-manage-conversations', TcodeManageConversations);
