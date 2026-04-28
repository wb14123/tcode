import { LitElement, html, nothing, type TemplateResult } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import { unsafeHTML } from 'lit/directives/unsafe-html.js';

import { renderMarkdownToHtml } from '../markdown';
import { hrefForRoute } from '../router';
import type {
  AssistantTimelineItem,
  RawTimelineItem,
  SubagentTimelineItem,
  TimelineItem,
  ToolTimelineItem,
  UserTimelineItem,
} from '../types';
import { TimelineStore, type TimelineUnsubscribe } from '../timeline-store';
import {
  firstLinePreview,
  renderCollapsedExpandableRow,
  renderExpandedExpandableRow,
  renderExpandedRowAction,
  renderExpandedSubagentBody,
  renderExpandedToolBody,
  renderSubagentFooterContent,
  renderToolFooterContent,
  rowStatusIndicator,
  subagentDetailRoute,
  subagentRowTitle,
  toolDetailRoute,
  toolRowTitle,
} from '../timeline-render-helpers';

type TimelineRowTag = 'tcode-user-message' | 'tcode-assistant-message' | 'tcode-tool-row' | 'tcode-subagent-row' | 'tcode-raw-event-row';

function tagForItem(item: TimelineItem): TimelineRowTag | null {
  switch (item.kind) {
    case 'user':
      return 'tcode-user-message';
    case 'assistant':
      return 'tcode-assistant-message';
    case 'tool':
      return 'tcode-tool-row';
    case 'subagent':
      return 'tcode-subagent-row';
    case 'raw':
      return 'tcode-raw-event-row';
    case 'system':
    case 'signal':
      return null;
  }
}

function formatTimestamp(timestamp: number | null | undefined): string {
  if (!timestamp) {
    return '—';
  }

  return new Date(timestamp).toLocaleString();
}

function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

class TcodeTimeline extends LitElement {
  static properties = {
    store: { attribute: false },
    sessionId: { type: String, attribute: 'session-id' },
    currentSubagentId: { type: String, attribute: 'current-subagent-id' },
    loading: { type: Boolean },
    loadingMessage: { type: String, attribute: 'loading-message' },
    emptyMessage: { type: String, attribute: 'empty-message' },
    scrollToBottomToken: { type: Number, attribute: 'scroll-to-bottom-token' },
  };

  store: TimelineStore | null = null;
  sessionId = '';
  currentSubagentId = '';
  loading = false;
  loadingMessage = 'Loading…';
  emptyMessage = 'Waiting for streamed events…';
  scrollToBottomToken = 0;
  private visibleIds: readonly string[] = [];
  private unsubscribeStructure: TimelineUnsubscribe | null = null;
  private unsubscribeBeforeChange: TimelineUnsubscribe | null = null;
  private resizeObserver: ResizeObserver | null = null;
  private observedElements = new Set<Element>();
  private restoreFrame: number | null = null;
  private stickToBottom = true;
  private restoringScroll = false;
  private capturedAnchor: { wasStuckToBottom: boolean; itemId: string | null; topOffset: number } | null = null;

  createRenderRoot(): this {
    return this;
  }

  connectedCallback(): void {
    super.connectedCallback();
    this.resizeObserver = new ResizeObserver(() => this.scheduleRestoreScroll());
    this.syncStoreSubscription();
  }

  disconnectedCallback(): void {
    this.unsubscribeStructure?.();
    this.unsubscribeStructure = null;
    this.unsubscribeBeforeChange?.();
    this.unsubscribeBeforeChange = null;
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
    this.observedElements.clear();
    if (this.restoreFrame !== null) {
      window.cancelAnimationFrame(this.restoreFrame);
      this.restoreFrame = null;
    }
    super.disconnectedCallback();
  }

  willUpdate(changed: Map<string, unknown>): void {
    if (changed.has('store')) {
      this.syncStoreSubscription();
    }
  }

  updated(changed: Map<string, unknown>): void {
    this.syncResizeObservers();
    if (changed.has('scrollToBottomToken')) {
      this.scheduleRestoreScroll(true);
    }
  }

  private syncStoreSubscription(): void {
    this.unsubscribeStructure?.();
    this.unsubscribeStructure = null;
    this.unsubscribeBeforeChange?.();
    this.unsubscribeBeforeChange = null;
    this.visibleIds = this.store?.getVisibleIds() ?? [];

    if (!this.store || !this.isConnected) {
      return;
    }

    this.unsubscribeBeforeChange = this.store.subscribeBeforeChange(() => this.captureScrollAnchor());
    this.unsubscribeStructure = this.store.subscribeStructure(() => {
      this.visibleIds = this.store?.getVisibleIds() ?? [];
      this.requestUpdate();
      this.scheduleRestoreScroll();
    });
  }

  render(): TemplateResult {
    return html`
      <section class="chat-scroll-area" @scroll=${this.onScroll}>
        ${this.loading
          ? html`<div class="chat-empty-state">${this.loadingMessage}</div>`
          : this.visibleIds.length
            ? html`
                <div class="timeline chat-timeline">
                  ${repeat(
                    this.visibleIds,
                    (id) => id,
                    (id) => this.renderRow(id),
                  )}
                </div>
              `
            : html`<div class="chat-empty-state">${this.emptyMessage}</div>`}
      </section>
    `;
  }

  private renderRow(itemId: string): TemplateResult | typeof nothing {
    const item = this.store?.getItem(itemId);
    if (!item || !this.store) {
      return nothing;
    }

    const tag = tagForItem(item);
    if (!tag) {
      return nothing;
    }

    switch (tag) {
      case 'tcode-user-message':
        return html`<tcode-user-message data-timeline-item-id=${itemId} .store=${this.store} .itemId=${itemId} .sessionId=${this.sessionId} .currentSubagentId=${this.currentSubagentId}></tcode-user-message>`;
      case 'tcode-assistant-message':
        return html`<tcode-assistant-message data-timeline-item-id=${itemId} .store=${this.store} .itemId=${itemId} .sessionId=${this.sessionId} .currentSubagentId=${this.currentSubagentId}></tcode-assistant-message>`;
      case 'tcode-tool-row':
        return html`<tcode-tool-row data-timeline-item-id=${itemId} .store=${this.store} .itemId=${itemId} .sessionId=${this.sessionId} .currentSubagentId=${this.currentSubagentId}></tcode-tool-row>`;
      case 'tcode-subagent-row':
        return html`<tcode-subagent-row data-timeline-item-id=${itemId} .store=${this.store} .itemId=${itemId} .sessionId=${this.sessionId} .currentSubagentId=${this.currentSubagentId}></tcode-subagent-row>`;
      case 'tcode-raw-event-row':
        return html`<tcode-raw-event-row data-timeline-item-id=${itemId} .store=${this.store} .itemId=${itemId} .sessionId=${this.sessionId} .currentSubagentId=${this.currentSubagentId}></tcode-raw-event-row>`;
    }
  }

  private onScroll = (event: Event): void => {
    const target = event.currentTarget;
    if (!(target instanceof HTMLElement)) {
      return;
    }

    const remaining = target.scrollHeight - target.scrollTop - target.clientHeight;
    this.stickToBottom = remaining < 80;
    if (!this.restoringScroll) {
      this.capturedAnchor = null;
    }
  };

  private captureScrollAnchor(): void {
    const scroller = this.querySelector<HTMLElement>('.chat-scroll-area');
    if (!scroller) {
      this.capturedAnchor = null;
      return;
    }

    const remaining = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    const wasStuckToBottom = remaining < 80;
    this.stickToBottom = wasStuckToBottom;
    const scrollerRect = scroller.getBoundingClientRect();
    const rows = Array.from(this.querySelectorAll<HTMLElement>('[data-timeline-item-id]'));
    const anchorRow = rows.find((row) => row.getBoundingClientRect().bottom >= scrollerRect.top);
    this.capturedAnchor = {
      wasStuckToBottom,
      itemId: anchorRow?.dataset.timelineItemId ?? null,
      topOffset: anchorRow ? anchorRow.getBoundingClientRect().top - scrollerRect.top : 0,
    };
  }

  private scheduleRestoreScroll(forceBottom = false): void {
    if (forceBottom) {
      this.capturedAnchor = { wasStuckToBottom: true, itemId: null, topOffset: 0 };
    }

    if (this.restoreFrame !== null) {
      return;
    }

    this.restoreFrame = window.requestAnimationFrame(() => {
      this.restoreFrame = null;
      this.restoreScroll(forceBottom);
    });
  }

  private restoreScroll(forceBottom = false): void {
    const scroller = this.querySelector<HTMLElement>('.chat-scroll-area');
    if (!scroller) {
      return;
    }

    const anchor = this.capturedAnchor;
    const setScrollTop = (value: number): void => {
      this.restoringScroll = true;
      scroller.scrollTop = value;
      window.setTimeout(() => {
        this.restoringScroll = false;
      }, 0);
    };
    if (forceBottom || anchor?.wasStuckToBottom || this.stickToBottom) {
      setScrollTop(scroller.scrollHeight);
      this.stickToBottom = true;
      if (forceBottom || anchor?.wasStuckToBottom) {
        this.capturedAnchor = null;
      }
      return;
    }

    if (!anchor?.itemId) {
      return;
    }

    const row = this.querySelector<HTMLElement>(`[data-timeline-item-id="${CSS.escape(anchor.itemId)}"]`);
    if (!row) {
      return;
    }

    const scrollerRect = scroller.getBoundingClientRect();
    const rowTop = row.getBoundingClientRect().top;
    setScrollTop(scroller.scrollTop + rowTop - scrollerRect.top - anchor.topOffset);
  }

  private syncResizeObservers(): void {
    if (!this.resizeObserver) {
      return;
    }

    const nextElements = new Set<Element>([...this.querySelectorAll('[data-timeline-item-id]')]);
    const scroller = this.querySelector('.chat-scroll-area');
    if (scroller) {
      nextElements.add(scroller);
    }

    for (const element of this.observedElements) {
      if (!nextElements.has(element)) {
        this.resizeObserver.unobserve(element);
      }
    }

    for (const element of nextElements) {
      if (!this.observedElements.has(element)) {
        this.resizeObserver.observe(element);
      }
    }

    this.observedElements = nextElements;
  }
}

abstract class TimelineRowElement extends LitElement {
  static properties = {
    store: { attribute: false },
    itemId: { type: String, attribute: 'item-id' },
    sessionId: { type: String, attribute: 'session-id' },
    currentSubagentId: { type: String, attribute: 'current-subagent-id' },
  };

  store: TimelineStore | null = null;
  itemId = '';
  sessionId = '';
  currentSubagentId = '';
  protected item: TimelineItem | undefined;
  private unsubscribeItem: TimelineUnsubscribe | null = null;
  private subscribedStore: TimelineStore | null = null;
  private subscribedItemId = '';

  createRenderRoot(): this {
    return this;
  }

  connectedCallback(): void {
    super.connectedCallback();
    this.syncItemSubscription();
  }

  disconnectedCallback(): void {
    this.unsubscribeItem?.();
    this.unsubscribeItem = null;
    this.subscribedStore = null;
    this.subscribedItemId = '';
    super.disconnectedCallback();
  }

  willUpdate(changed: Map<string, unknown>): void {
    if (changed.has('store') || changed.has('itemId')) {
      this.syncItemSubscription();
    }
  }

  updated(changed: Map<string, unknown>): void {
    if (changed.has('store') || changed.has('itemId')) {
      this.syncItemSubscription();
    }
  }

  protected abstract expectedKind(): TimelineItem['kind'];

  render(): TemplateResult | typeof nothing {
    const item = this.item;
    if (!item || item.kind !== this.expectedKind()) {
      return nothing;
    }

    return this.renderItem(item);
  }

  protected toggleExpanded(itemId = this.itemId): void {
    this.store?.toggleExpanded(itemId);
  }

  protected toggleExpandedOnKeydown(event: KeyboardEvent, itemId = this.itemId): void {
    if (event.key !== 'Enter' && event.key !== ' ') {
      return;
    }

    event.preventDefault();
    this.toggleExpanded(itemId);
  }

  protected abstract renderItem(item: TimelineItem): TemplateResult | typeof nothing;

  private syncItemSubscription(): void {
    if (this.subscribedStore === this.store && this.subscribedItemId === this.itemId) {
      return;
    }

    this.unsubscribeItem?.();
    this.unsubscribeItem = null;
    this.subscribedStore = this.store;
    this.subscribedItemId = this.itemId;
    this.item = this.store?.getItem(this.itemId);

    if (!this.store || !this.itemId || !this.isConnected) {
      return;
    }

    this.unsubscribeItem = this.store.subscribeItem(this.itemId, (item) => {
      this.item = item;
      this.requestUpdate();
    });
  }
}

class TcodeUserMessage extends TimelineRowElement {
  protected expectedKind(): TimelineItem['kind'] {
    return 'user';
  }

  protected renderItem(item: TimelineItem): TemplateResult | typeof nothing {
    if (item.kind !== 'user') {
      return nothing;
    }
    return this.renderUser(item);
  }

  private renderUser(item: UserTimelineItem): TemplateResult {
    return html`
      <article class="chat-bubble chat-bubble-user timeline-user">
        <div class="message-meta">You · ${formatTimestamp(item.createdAt)}</div>
        <pre class="timeline-pre message-bubble-content">${item.content}</pre>
      </article>
    `;
  }
}

class TcodeAssistantMessage extends TimelineRowElement {
  private lastRenderedSource = '';
  private lastRenderedHtml = '';
  private lastMarkdownRenderTimeMs = 0;
  private pendingMarkdownTimer: number | null = null;
  private pendingSource: string | null = null;

  protected expectedKind(): TimelineItem['kind'] {
    return 'assistant';
  }

  disconnectedCallback(): void {
    this.clearPendingMarkdownTimer();
    super.disconnectedCallback();
  }

  protected renderItem(item: TimelineItem): TemplateResult | typeof nothing {
    if (item.kind !== 'assistant') {
      return nothing;
    }
    return this.renderAssistant(item);
  }

  private renderAssistant(item: AssistantTimelineItem): TemplateResult {
    return html`
      <article class="chat-bubble chat-bubble-assistant timeline-assistant">
        <div class="message-meta">
          <span>Assistant</span>
          <span>${formatTimestamp(item.createdAt)}</span>
        </div>
        ${item.thinking
          ? html`
              <details class="thinking-panel">
                <summary>Reasoning stream</summary>
                <pre class="timeline-pre">${item.thinking}</pre>
              </details>
            `
          : nothing}
        ${item.content
          ? html`<div class="message-bubble-content markdown-content">${unsafeHTML(this.markdownHtmlFor(item))}</div>`
          : nothing}
        ${item.error ? html`<div class="inline-alert error">${item.error}</div>` : nothing}
        ${(item.inputTokens ?? item.outputTokens ?? item.reasoningTokens) !== null
          ? html`
              <footer class="timeline-footer">
                in ${item.inputTokens ?? 0} · out ${item.outputTokens ?? 0} · reasoning ${item.reasoningTokens ?? 0}
              </footer>
            `
          : nothing}
      </article>
    `;
  }

  private markdownHtmlFor(item: AssistantTimelineItem): string {
    const source = item.content;
    if (source === this.lastRenderedSource) {
      return this.lastRenderedHtml;
    }

    const isActive = this.store?.getActiveAssistantId() === item.id;
    const isFinal = !isActive && (item.endStatus !== null || item.error !== null || item.inputTokens !== null || item.outputTokens !== null || item.reasoningTokens !== null);
    if (!isActive || isFinal || !this.lastRenderedSource) {
      this.clearPendingMarkdownTimer();
      return this.renderMarkdownNow(source);
    }

    const now = performance.now();
    const elapsed = now - this.lastMarkdownRenderTimeMs;
    if (elapsed >= 100) {
      this.clearPendingMarkdownTimer();
      return this.renderMarkdownNow(source);
    }

    this.pendingSource = source;
    if (this.pendingMarkdownTimer === null) {
      this.pendingMarkdownTimer = window.setTimeout(() => {
        this.pendingMarkdownTimer = null;
        const pending = this.pendingSource;
        this.pendingSource = null;
        if (pending !== null && pending !== this.lastRenderedSource) {
          this.renderMarkdownNow(pending);
          this.requestUpdate();
        }
      }, 100 - elapsed);
    }

    return this.lastRenderedHtml;
  }

  private renderMarkdownNow(source: string): string {
    this.lastRenderedSource = source;
    this.lastRenderedHtml = renderMarkdownToHtml(source);
    this.lastMarkdownRenderTimeMs = performance.now();
    return this.lastRenderedHtml;
  }

  private clearPendingMarkdownTimer(): void {
    if (this.pendingMarkdownTimer !== null) {
      window.clearTimeout(this.pendingMarkdownTimer);
      this.pendingMarkdownTimer = null;
    }
    this.pendingSource = null;
  }
}

class TcodeToolRow extends TimelineRowElement {
  protected expectedKind(): TimelineItem['kind'] {
    return 'tool';
  }

  protected renderItem(item: TimelineItem): TemplateResult | typeof nothing {
    if (item.kind !== 'tool') {
      return nothing;
    }
    return this.renderTool(item);
  }

  private renderTool(item: ToolTimelineItem): TemplateResult {
    const expanded = this.store?.isExpanded(item.id) ?? false;
    return expanded ? this.renderExpandedTool(item) : this.renderCollapsedTool(item);
  }

  private renderCollapsedTool(item: ToolTimelineItem): TemplateResult {
    return renderCollapsedExpandableRow({
      kind: 'tool',
      cardClass: 'chat-card-tool timeline-tool',
      title: toolRowTitle(item),
      status: rowStatusIndicator('tool', item.endStatus, item.permissionState),
      toggle: {
        ariaLabel: `Expand tool ${item.toolName || 'Tool call'}`,
        onClick: () => this.toggleExpanded(item.id),
        onKeydown: (event: KeyboardEvent) => this.toggleExpandedOnKeydown(event, item.id),
      },
    });
  }

  private renderExpandedTool(item: ToolTimelineItem): TemplateResult {
    return renderExpandedExpandableRow({
      kind: 'tool',
      cardClass: 'chat-card-tool timeline-tool',
      title: toolRowTitle(item),
      status: rowStatusIndicator('tool', item.endStatus, item.permissionState),
      toggle: {
        ariaLabel: `Collapse tool ${item.toolName || 'Tool call'}`,
        onClick: () => this.toggleExpanded(item.id),
        onKeydown: (event: KeyboardEvent) => this.toggleExpandedOnKeydown(event, item.id),
      },
      action: renderExpandedRowAction(
        'Open detail',
        hrefForRoute(toolDetailRoute(this.sessionId, item.toolCallId, this.currentSubagentId || undefined)),
      ),
      body: renderExpandedToolBody(item),
      footer: renderToolFooterContent(item),
    });
  }
}

class TcodeSubagentRow extends TimelineRowElement {
  protected expectedKind(): TimelineItem['kind'] {
    return 'subagent';
  }

  protected renderItem(item: TimelineItem): TemplateResult | typeof nothing {
    if (item.kind !== 'subagent') {
      return nothing;
    }
    return this.renderSubagent(item);
  }

  private renderSubagent(item: SubagentTimelineItem): TemplateResult {
    const expanded = this.store?.isExpanded(item.id) ?? false;
    return expanded ? this.renderExpandedSubagent(item) : this.renderCollapsedSubagent(item);
  }

  private renderCollapsedSubagent(item: SubagentTimelineItem): TemplateResult {
    const title = subagentRowTitle(item);
    return renderCollapsedExpandableRow({
      kind: 'subagent',
      cardClass: 'chat-card-subagent timeline-subagent',
      title,
      status: rowStatusIndicator('subagent', item.endStatus, item.permissionState, item.pending === true),
      toggle: {
        ariaLabel: `Expand subagent ${title}`,
        onClick: () => this.toggleExpanded(item.id),
        onKeydown: (event: KeyboardEvent) => this.toggleExpandedOnKeydown(event, item.id),
      },
    });
  }

  private renderExpandedSubagent(item: SubagentTimelineItem): TemplateResult {
    const title = subagentRowTitle(item);
    return renderExpandedExpandableRow({
      kind: 'subagent',
      cardClass: 'chat-card-subagent timeline-subagent',
      title,
      status: rowStatusIndicator('subagent', item.endStatus, item.permissionState, item.pending === true),
      toggle: {
        ariaLabel: `Collapse subagent ${title}`,
        onClick: () => this.toggleExpanded(item.id),
        onKeydown: (event: KeyboardEvent) => this.toggleExpandedOnKeydown(event, item.id),
      },
      action: item.pending
        ? nothing
        : renderExpandedRowAction('Open conversation', hrefForRoute(subagentDetailRoute(this.sessionId, item.conversationId))),
      body: renderExpandedSubagentBody(item),
      footer: renderSubagentFooterContent(item),
    });
  }
}

class TcodeRawEventRow extends TimelineRowElement {
  protected expectedKind(): TimelineItem['kind'] {
    return 'raw';
  }

  protected renderItem(item: TimelineItem): TemplateResult | typeof nothing {
    if (item.kind !== 'raw') {
      return nothing;
    }
    return this.renderRaw(item);
  }

  private renderRaw(item: RawTimelineItem): TemplateResult {
    const expanded = this.store?.isExpanded(item.id) ?? false;
    const preview = firstLinePreview(item.rawText || String(item.rawJson));
    return html`
      <article class="timeline-card chat-card compact-card chat-card-raw timeline-raw">
        <header class="chat-card-header">
          <div>
            <div class="chat-card-title">${item.label}</div>
            <div class="chat-card-subtitle">${item.rawText.length.toLocaleString()} chars</div>
          </div>
          <button class="button ghost subagent-toggle" type="button" @click=${() => this.store?.toggleExpanded(item.id)}>
            ${expanded ? 'Collapse' : 'Expand'}
          </button>
        </header>
        ${expanded ? html`<pre class="timeline-pre">${prettyJson(item.rawJson)}</pre>` : html`<div class="timeline-text">${preview}</div>`}
      </article>
    `;
  }
}

customElements.define('tcode-timeline', TcodeTimeline);
customElements.define('tcode-user-message', TcodeUserMessage);
customElements.define('tcode-assistant-message', TcodeAssistantMessage);
customElements.define('tcode-tool-row', TcodeToolRow);
customElements.define('tcode-subagent-row', TcodeSubagentRow);
customElements.define('tcode-raw-event-row', TcodeRawEventRow);
