import { LitElement, html, nothing } from 'lit';

import { ApiError, ReplayAwareBuffer, api, openEventStream } from '../api';
import { buildConversationTimeline, parseStreamLine, rawVariant, renderTimelineItem } from '../messages';
import { hrefForRoute } from '../router';
import type {
  ConversationState,
  ParentContext,
  RawStreamEvent,
  SessionMeta,
  TimelineItem,
} from '../types';

class TcodeSubagentView extends LitElement {
  static properties = {
    sessionId: { type: String },
    subagentId: { type: String },
  };

  sessionId = '';
  subagentId = '';
  private meta: SessionMeta | null = null;
  private state: ConversationState | null = null;
  private statusText = '';
  private tokenUsageText = '';
  private parent: ParentContext | null = null;
  private events: RawStreamEvent[] = [];
  private timeline: TimelineItem[] = [];
  private loading = true;
  private streamState = 'connecting';
  private errorMessage = '';
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
    if (changed.has('sessionId') || changed.has('subagentId')) {
      this.startView();
    }
  }

  private startView(): void {
    if (!this.sessionId || !this.subagentId) {
      return;
    }

    this.stopView();
    this.meta = null;
    this.state = null;
    this.statusText = '';
    this.tokenUsageText = '';
    this.parent = null;
    this.events = [];
    this.timeline = [];
    this.loading = true;
    this.streamState = 'connecting';
    this.errorMessage = '';
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
      const [metaResponse, state, statusText, tokenUsageText] = await Promise.all([
        api.getSubagentMeta(this.sessionId, this.subagentId),
        api.getSubagentConversationState(this.sessionId, this.subagentId),
        api.getSubagentStatus(this.sessionId, this.subagentId),
        api.getSubagentTokenUsage(this.sessionId, this.subagentId),
      ]);
      this.meta = metaResponse?.meta ?? null;
      this.parent = metaResponse?.parent ?? null;
      this.state = state;
      this.statusText = statusText.trim();
      this.tokenUsageText = tokenUsageText.trim();
      this.errorMessage = '';
      if (initial) {
        this.loading = false;
      }
      this.requestUpdate();
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) {
        this.errorMessage = 'Subagent files are not available yet. Waiting for conversation output…';
      } else {
        this.errorMessage = error instanceof Error ? error.message : 'Failed to load subagent data';
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
    const source = openEventStream(api.subagentDisplayPath(this.sessionId, this.subagentId));
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

  private parentHref(): string | null {
    if (!this.parent) {
      return null;
    }

    if (this.parent.kind === 'session') {
      return hrefForRoute({ kind: 'session', sessionId: this.sessionId });
    }

    if (this.parent.kind === 'subagent') {
      return hrefForRoute({
        kind: 'subagent',
        sessionId: this.sessionId,
        subagentId: this.parent.conversation_id,
      });
    }

    return null;
  }

  render() {
    const parentHref = this.parentHref();

    return html`
      <section class="page">
        <header class="page-header">
          <div>
            <h1 class="page-title">Subagent ${this.subagentId}</h1>
            <p class="page-subtitle">
              Read-only subagent detail view. The current backend exposes snapshots and streams here,
              but not a dedicated send/finish endpoint for subagents.
            </p>
          </div>
          <div class="header-actions">
            <span class="pill pill-${this.streamState}">${this.streamState}</span>
            ${parentHref
              ? html`<a class="button secondary" href="${parentHref}">Back to parent</a>`
              : nothing}
          </div>
        </header>

        ${this.errorMessage ? html`<div class="inline-alert error">${this.errorMessage}</div>` : nothing}

        <section class="panel-grid">
          <section class="panel">
            <h3>Subagent snapshot</h3>
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
                <dt>Conversation id</dt>
                <dd>${this.state?.id || this.subagentId}</dd>
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
                <dt>Parent</dt>
                <dd>${this.parent?.kind || 'unknown'}</dd>
              </div>
            </dl>
            ${this.parent?.tool_call_id
              ? html`<div class="timeline-footer">Spawn tool call: ${this.parent.tool_call_id}</div>`
              : nothing}
          </section>

          <section class="panel">
            <h3>Status</h3>
            <pre class="timeline-pre">${this.statusText || 'No status text yet.'}</pre>
            ${this.tokenUsageText ? html`<pre class="timeline-pre">${this.tokenUsageText}</pre>` : nothing}
          </section>
        </section>

        <section class="panel">
          <h3>Conversation</h3>
          ${this.loading
            ? html`<div class="timeline-empty">Loading subagent…</div>`
            : this.timeline.length
              ? html`
                  <div class="timeline">
                    ${this.timeline.map((item) =>
                      renderTimelineItem(item, {
                        sessionId: this.sessionId,
                        currentSubagentId: this.subagentId,
                      }),
                    )}
                  </div>
                `
              : html`<div class="timeline-empty">Waiting for subagent streamed events…</div>`}
        </section>
      </section>
    `;
  }
}

customElements.define('tcode-subagent-view', TcodeSubagentView);
