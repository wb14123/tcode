import { html, nothing, type TemplateResult } from 'lit';
import { unsafeHTML } from 'lit/directives/unsafe-html.js';

import { renderMarkdownToHtml } from './markdown';
import { hrefForRoute } from './router';
import { TimelineStore } from './timeline-store';
import type {
  AppRoute,
  AssistantTimelineItem,
  RawStreamEvent,
  RawTimelineItem,
  SignalTimelineItem,
  SubagentTimelineItem,
  SystemTimelineItem,
  TimelineItem,
  ToolTimelineItem,
  UserTimelineItem,
  WireMessageEnvelope,
} from './types';

export interface SystemNotification {
  createdAt: number | null;
  level: string | null;
  message: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }

  return null;
}

function asString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function asNumber(value: unknown): number | null {
  return typeof value === 'number' ? value : null;
}

function oneKeyObject(value: unknown): [string, Record<string, unknown>] | null {
  const record = asRecord(value);
  if (!record) {
    return null;
  }

  const entries = Object.entries(record);
  if (entries.length !== 1) {
    return null;
  }

  const [variant, payload] = entries[0];
  const payloadRecord = asRecord(payload);
  if (!payloadRecord) {
    return null;
  }

  return [variant, payloadRecord];
}

export function parseStreamLine(rawText: string): RawStreamEvent | null {
  const trimmed = rawText.trim();
  if (!trimmed || trimmed === 'keepalive') {
    return null;
  }

  try {
    const rawJson = JSON.parse(rawText) as unknown;
    const tagged = oneKeyObject(rawJson);
    const wire: WireMessageEnvelope | null = tagged
      ? {
          variant: tagged[0],
          payload: tagged[1],
          raw: rawJson,
          rawText,
        }
      : null;

    return {
      rawText,
      rawJson,
      wire,
    };
  } catch {
    return {
      rawText,
      rawJson: rawText,
      wire: null,
    };
  }
}

function createAssistant(id: string, msgId: number | null): AssistantTimelineItem {
  return {
    id,
    revision: 0,
    kind: 'assistant',
    msgId,
    createdAt: null,
    content: '',
    thinking: '',
    endStatus: null,
    error: null,
    inputTokens: null,
    outputTokens: null,
    reasoningTokens: null,
  };
}

function createTool(toolCallId: string): ToolTimelineItem {
  return {
    id: `tool:${toolCallId}`,
    revision: 0,
    kind: 'tool',
    msgId: null,
    toolCallId,
    createdAt: null,
    toolName: '',
    toolArgs: '',
    output: '',
    endStatus: null,
    inputTokens: null,
    outputTokens: null,
    permissionState: null,
  };
}

function createSubagent(conversationId: string): SubagentTimelineItem {
  return {
    id: `subagent:${conversationId}`,
    revision: 0,
    kind: 'subagent',
    msgId: null,
    toolCallId: null,
    conversationId,
    createdAt: null,
    description: '',
    input: '',
    response: '',
    endStatus: null,
    inputTokens: null,
    outputTokens: null,
    permissionState: null,
  };
}

function createRawItem(store: TimelineStore, event: RawStreamEvent, label: string): RawTimelineItem {
  return {
    id: `raw:${store.nextSequence()}`,
    revision: 0,
    kind: 'raw',
    createdAt: null,
    label,
    rawText: event.rawText,
    rawJson: event.rawJson,
  };
}

function createSignal(store: TimelineStore, label: string, details = ''): SignalTimelineItem {
  return {
    id: `signal:${store.nextSequence()}`,
    revision: 0,
    kind: 'signal',
    label,
    details,
  };
}

function shouldRenderTimelineItem(item: TimelineItem): boolean {
  if (item.kind === 'signal' || item.kind === 'system') {
    return false;
  }

  if (item.kind !== 'assistant') {
    return true;
  }

  return Boolean(item.content.trim() || item.thinking.trim() || item.error);
}

interface PendingSubagentInput {
  msgId: number | null;
  toolCallId: string | null;
  createdAt: number | null;
  description: string;
  input: string;
}

export class ConversationTimelineBuilder {
  readonly store = new TimelineStore();

  private itemIds: string[] = [];
  private visibleItems: TimelineItem[] = [];
  private tools = new Map<string, string>();
  private toolCallIdByIndex = new Map<number, string>();
  private subagents = new Map<string, string>();
  private subagentIdByToolCall = new Map<string, string>();
  private subagentIdByToolIndex = new Map<number, string>();
  private pendingSubagentInputByToolCall = new Map<string, PendingSubagentInput>();
  private pendingSubagentToolCallByIndex = new Map<number, string>();
  private visibleSet = new Set<string>();

  get timeline(): TimelineItem[] {
    return this.visibleItems;
  }

  reset(): void {
    this.itemIds = [];
    this.visibleItems.splice(0);
    this.tools = new Map<string, string>();
    this.toolCallIdByIndex = new Map<number, string>();
    this.subagents = new Map<string, string>();
    this.subagentIdByToolCall = new Map<string, string>();
    this.subagentIdByToolIndex = new Map<number, string>();
    this.pendingSubagentInputByToolCall = new Map<string, PendingSubagentInput>();
    this.pendingSubagentToolCallByIndex = new Map<number, string>();
    this.visibleSet = new Set<string>();
    this.store.reset();
  }

  appendEvent(event: RawStreamEvent): TimelineItem[] {
    return this.appendEvents([event]);
  }

  appendEvents(events: RawStreamEvent[]): TimelineItem[] {
    this.store.batch({ layoutMayChange: true }, () => {
      for (const event of events) {
        this.appendEventInBatch(event);
      }
    });

    this.syncVisibleItemsFromStore();
    return this.visibleItems;
  }

  private appendEventInBatch(event: RawStreamEvent): void {
      const wire = event.wire;
      if (!wire) {
        this.addItem(createRawItem(this.store, event, 'Raw event'));
        return;
      }

      const { variant, payload } = wire;

      switch (variant) {
        case 'UserMessage': {
          const msgId = asNumber(payload.msg_id);
          const item: UserTimelineItem = {
            id: msgId !== null ? `user:${msgId}` : this.store.nextSequenceId('user'),
            revision: 0,
            kind: 'user',
            msgId,
            createdAt: asNumber(payload.created_at),
            content: asString(payload.content) ?? '',
          };
          this.addItem(item);
          break;
        }
        case 'AssistantMessageStart': {
          const msgId = asNumber(payload.msg_id);
          const id = msgId !== null ? `assistant:${msgId}` : this.store.nextSequenceId('assistant');
          const item = createAssistant(id, msgId);
          item.createdAt = asNumber(payload.created_at);
          this.addItem(item, false);
          this.store.setActiveAssistantId(id);
          break;
        }
        case 'AssistantMessageChunk': {
          const id = this.ensureActiveAssistantId();
          this.updateAssistant(id, (item) => {
            item.msgId ??= asNumber(payload.msg_id);
            item.content += asString(payload.content) ?? '';
          });
          this.store.setActiveAssistantId(id);
          break;
        }
        case 'AssistantThinkingChunk': {
          const id = this.ensureActiveAssistantId();
          this.updateAssistant(id, (item) => {
            item.msgId ??= asNumber(payload.msg_id);
            item.thinking += asString(payload.content) ?? '';
          });
          this.store.setActiveAssistantId(id);
          break;
        }
        case 'AssistantMessageEnd': {
          const id = this.ensureActiveAssistantId();
          this.updateAssistant(id, (item) => {
            item.msgId ??= asNumber(payload.msg_id);
            item.endStatus = asString(payload.end_status);
            item.error = asString(payload.error);
            item.inputTokens = asNumber(payload.input_tokens);
            item.outputTokens = asNumber(payload.output_tokens);
            item.reasoningTokens = asNumber(payload.reasoning_tokens);
          });
          this.store.setActiveAssistantId(null);
          break;
        }
        case 'AssistantToolCallStart': {
          const toolCallId = asString(payload.tool_call_id);
          const toolCallIndex = asNumber(payload.tool_call_index);
          if (!toolCallId) {
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateToolId(toolCallId);
          this.updateTool(id, (item) => {
            item.msgId = asNumber(payload.msg_id);
            item.createdAt = asNumber(payload.created_at);
            item.toolName = asString(payload.tool_name) ?? item.toolName;
          });
          if (toolCallIndex !== null) {
            this.toolCallIdByIndex.set(toolCallIndex, toolCallId);
          }
          break;
        }
        case 'AssistantToolCallArgChunk': {
          const index = asNumber(payload.tool_call_index);
          const toolCallId = index !== null ? this.toolCallIdByIndex.get(index) : null;
          if (!toolCallId) {
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateToolId(toolCallId);
          this.updateTool(id, (item) => {
            item.toolName = asString(payload.tool_name) ?? item.toolName;
            item.toolArgs += asString(payload.content) ?? '';
          });
          break;
        }
        case 'ToolMessageStart': {
          const toolCallId = asString(payload.tool_call_id);
          if (!toolCallId) {
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateToolId(toolCallId);
          this.updateTool(id, (item) => {
            item.msgId = asNumber(payload.msg_id);
            item.createdAt = asNumber(payload.created_at);
            item.toolName = asString(payload.tool_name) ?? item.toolName;
            item.toolArgs = asString(payload.tool_args) ?? item.toolArgs;
          });
          break;
        }
        case 'ToolOutputChunk': {
          const toolCallId = asString(payload.tool_call_id);
          if (!toolCallId) {
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateToolId(toolCallId);
          this.updateTool(id, (item) => {
            item.toolName = asString(payload.tool_name) ?? item.toolName;
            item.output += asString(payload.content) ?? '';
          });
          break;
        }
        case 'ToolMessageEnd': {
          const toolCallId = asString(payload.tool_call_id);
          if (!toolCallId) {
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateToolId(toolCallId);
          this.updateTool(id, (item) => {
            item.msgId = asNumber(payload.msg_id);
            item.endStatus = asString(payload.end_status);
            item.inputTokens = asNumber(payload.input_tokens);
            item.outputTokens = asNumber(payload.output_tokens);
          });
          break;
        }
        case 'SubAgentInputStart': {
          const conversationId = asString(payload.conversation_id);
          const toolCallId = asString(payload.tool_call_id);
          const toolCallIndex = asNumber(payload.tool_call_index);
          const pendingInput: PendingSubagentInput = {
            msgId: asNumber(payload.msg_id),
            toolCallId,
            createdAt: asNumber(payload.created_at),
            description: asString(payload.tool_name) ?? '',
            input: '',
          };

          if (!conversationId) {
            if (toolCallId) {
              this.pendingSubagentInputByToolCall.set(toolCallId, pendingInput);
            }
            if (toolCallIndex !== null && toolCallId) {
              this.pendingSubagentToolCallByIndex.set(toolCallIndex, toolCallId);
            }
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateSubagentId(conversationId);
          this.updateSubagent(id, (item) => {
            item.msgId = pendingInput.msgId;
            item.toolCallId = toolCallId ?? item.toolCallId;
            item.createdAt = pendingInput.createdAt;
            item.description = pendingInput.description || item.description;
          });
          if (toolCallId) {
            this.subagentIdByToolCall.set(toolCallId, id);
          }
          if (toolCallIndex !== null) {
            this.subagentIdByToolIndex.set(toolCallIndex, id);
          }
          break;
        }
        case 'SubAgentInputChunk': {
          const conversationId = asString(payload.conversation_id);
          const toolCallId = asString(payload.tool_call_id);
          const toolCallIndex = asNumber(payload.tool_call_index);
          const pendingToolCallId = toolCallId ?? (toolCallIndex !== null ? this.pendingSubagentToolCallByIndex.get(toolCallIndex) : undefined);
          const id = conversationId
            ? this.getOrCreateSubagentId(conversationId)
            : toolCallId
              ? this.subagentIdByToolCall.get(toolCallId)
              : toolCallIndex !== null
                ? this.subagentIdByToolIndex.get(toolCallIndex)
                : undefined;
          if (!id) {
            if (pendingToolCallId) {
              const pending = this.pendingSubagentInputByToolCall.get(pendingToolCallId) ?? {
                msgId: null,
                toolCallId: pendingToolCallId,
                createdAt: null,
                description: '',
                input: '',
              };
              pending.description = asString(payload.tool_name) ?? pending.description;
              pending.input += asString(payload.content) ?? '';
              this.pendingSubagentInputByToolCall.set(pendingToolCallId, pending);
            }
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          this.updateSubagent(id, (item) => {
            item.description = asString(payload.tool_name) ?? item.description;
            item.input += asString(payload.content) ?? '';
          });
          break;
        }
        case 'SubAgentStart':
        case 'SubAgentContinue': {
          const conversationId = asString(payload.conversation_id);
          const toolCallId = asString(payload.tool_call_id);
          const toolCallIndex = asNumber(payload.tool_call_index);
          if (!conversationId) {
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateSubagentId(conversationId);
          const pendingToolCallId = toolCallId ?? (toolCallIndex !== null ? this.pendingSubagentToolCallByIndex.get(toolCallIndex) : undefined);
          const pending = pendingToolCallId ? this.pendingSubagentInputByToolCall.get(pendingToolCallId) : undefined;
          this.updateSubagent(id, (item) => {
            item.msgId = asNumber(payload.msg_id) ?? pending?.msgId ?? item.msgId;
            item.toolCallId = toolCallId ?? pending?.toolCallId ?? item.toolCallId;
            item.createdAt = pending?.createdAt ?? item.createdAt;
            item.description = asString(payload.description) ?? pending?.description ?? item.description;
            if (pending?.input && !item.input) {
              item.input = pending.input;
            }
          });
          if (toolCallId) {
            this.subagentIdByToolCall.set(toolCallId, id);
          }
          if (pendingToolCallId) {
            this.subagentIdByToolCall.set(pendingToolCallId, id);
            this.pendingSubagentInputByToolCall.delete(pendingToolCallId);
          }
          if (toolCallIndex !== null) {
            this.subagentIdByToolIndex.set(toolCallIndex, id);
          }
          break;
        }
        case 'SubAgentTurnEnd':
        case 'SubAgentEnd': {
          const conversationId = asString(payload.conversation_id);
          if (!conversationId) {
            this.addItem(createRawItem(this.store, event, variant));
            break;
          }

          const id = this.getOrCreateSubagentId(conversationId);
          this.updateSubagent(id, (item) => {
            item.msgId = asNumber(payload.msg_id);
            item.response = asString(payload.response) ?? item.response;
            item.endStatus = asString(payload.end_status);
            item.inputTokens = asNumber(payload.input_tokens);
            item.outputTokens = asNumber(payload.output_tokens);
          });
          break;
        }
        case 'SystemMessage': {
          const item: SystemTimelineItem = {
            id: this.store.nextSequenceId('system'),
            revision: 0,
            kind: 'system',
            msgId: asNumber(payload.msg_id),
            createdAt: asNumber(payload.created_at),
            level: asString(payload.level) ?? 'Info',
            message: asString(payload.message) ?? '',
          };
          this.addItem(item, false);
          break;
        }
        case 'ToolRequestPermission': {
          const toolCallId = asString(payload.tool_call_id);
          const id = toolCallId ? this.tools.get(toolCallId) : undefined;
          if (id) {
            this.updateTool(id, (item) => {
              item.permissionState = 'waiting';
            });
          } else {
            this.addItem(createSignal(this.store, 'Tool waiting for permission', toolCallId ?? ''), false);
          }
          break;
        }
        case 'ToolPermissionApproved': {
          const toolCallId = asString(payload.tool_call_id);
          const id = toolCallId ? this.tools.get(toolCallId) : undefined;
          if (id) {
            this.updateTool(id, (item) => {
              item.permissionState = 'approved';
            });
          } else {
            this.addItem(createSignal(this.store, 'Tool permission approved', toolCallId ?? ''), false);
          }
          break;
        }
        case 'SubAgentWaitingPermission': {
          const conversationId = asString(payload.conversation_id);
          if (conversationId) {
            const id = this.getOrCreateSubagentId(conversationId);
            this.updateSubagent(id, (item) => {
              item.permissionState = 'waiting';
            });
          } else {
            this.addItem(createRawItem(this.store, event, variant));
          }
          break;
        }
        case 'SubAgentPermissionApproved': {
          const conversationId = asString(payload.conversation_id);
          if (conversationId) {
            const id = this.getOrCreateSubagentId(conversationId);
            this.updateSubagent(id, (item) => {
              item.permissionState = 'approved';
            });
          } else {
            this.addItem(createRawItem(this.store, event, variant));
          }
          break;
        }
        case 'SubAgentPermissionDenied': {
          const conversationId = asString(payload.conversation_id);
          if (conversationId) {
            const id = this.getOrCreateSubagentId(conversationId);
            this.updateSubagent(id, (item) => {
              item.permissionState = 'denied';
            });
          } else {
            this.addItem(createRawItem(this.store, event, variant));
          }
          break;
        }
        case 'PermissionUpdated':
          this.addItem(createSignal(this.store, 'Permission state updated'), false);
          break;
        case 'AssistantRequestEnd':
          this.addItem(createSignal(this.store, 'Assistant requested conversation end'), false);
          break;
        case 'UserRequestEnd':
          this.addItem(createSignal(this.store, 'User requested conversation end', asString(payload.conversation_id) ?? ''), false);
          break;
        case 'ToolCallResolved':
          this.addItem(createSignal(this.store, 'Tool call resolved', asString(payload.tool_call_id) ?? ''), false);
          break;
        case 'AggregateTokenUpdate':
          this.addItem(createSignal(this.store, 'Aggregate tokens updated'), false);
          break;
        case 'SubAgentTokenRollup':
          this.addItem(createSignal(this.store, 'Subagent token rollup recorded'), false);
          break;
        default:
          this.addItem(createRawItem(this.store, event, variant));
          break;
      }
  }

  private addItem(item: TimelineItem, visible = true): void {
    if (this.store.hasItem(item.id)) {
      return;
    }

    this.itemIds.push(item.id);
    this.store.addItem(item, { visible, layoutMayChange: visible });
    if (visible && shouldRenderTimelineItem(item)) {
      this.visibleSet.add(item.id);
    }
  }

  private ensureActiveAssistantId(): string {
    const activeId = this.store.getActiveAssistantId();
    if (activeId && this.store.hasItem(activeId)) {
      return activeId;
    }

    const id = this.store.nextSequenceId('assistant');
    const item = createAssistant(id, null);
    this.addItem(item, false);
    this.store.setActiveAssistantId(id);
    return id;
  }

  private getOrCreateToolId(toolCallId: string): string {
    const existing = this.tools.get(toolCallId);
    if (existing) {
      return existing;
    }

    const item = createTool(toolCallId);
    this.tools.set(toolCallId, item.id);
    this.addItem(item);
    return item.id;
  }

  private getOrCreateSubagentId(conversationId: string): string {
    const existing = this.subagents.get(conversationId);
    if (existing) {
      return existing;
    }

    const item = createSubagent(conversationId);
    this.subagents.set(conversationId, item.id);
    this.addItem(item);
    return item.id;
  }

  private updateAssistant(id: string, mutator: (item: AssistantTimelineItem) => void): void {
    const wasVisible = this.visibleSet.has(id);
    this.store.updateItem(id, { layoutMayChange: true, visibleChange: wasVisible }, (item) => {
      if (item.kind === 'assistant') {
        mutator(item);
      }
    });
    this.syncItemVisibility(id);
  }

  private updateTool(id: string, mutator: (item: ToolTimelineItem) => void): void {
    this.store.updateItem(id, { layoutMayChange: true }, (item) => {
      if (item.kind === 'tool') {
        mutator(item);
      }
    });
  }

  private updateSubagent(id: string, mutator: (item: SubagentTimelineItem) => void): void {
    this.store.updateItem(id, { layoutMayChange: true }, (item) => {
      if (item.kind === 'subagent') {
        mutator(item);
      }
    });
  }

  private syncItemVisibility(id: string): void {
    const item = this.store.getItem(id);
    if (!item) {
      return;
    }

    const shouldRender = shouldRenderTimelineItem(item);
    const isVisible = this.visibleSet.has(id);

    if (shouldRender && !isVisible) {
      this.store.showItem(id, { index: this.visibleInsertIndex(id), layoutMayChange: true });
      this.visibleSet.add(id);
      return;
    }

    if (!shouldRender && isVisible) {
      this.store.hideItem(id, { layoutMayChange: true });
      this.visibleSet.delete(id);
    }
  }

  private visibleInsertIndex(id: string): number | undefined {
    const itemIndex = this.itemIds.indexOf(id);
    if (itemIndex === -1) {
      return undefined;
    }

    const visibleIds = this.store.getVisibleIds();
    for (let index = itemIndex - 1; index >= 0; index -= 1) {
      const previousId = this.itemIds[index];
      if (this.visibleSet.has(previousId)) {
        const visibleIndex = visibleIds.indexOf(previousId);
        return visibleIndex === -1 ? undefined : visibleIndex + 1;
      }
    }

    return 0;
  }

  private syncVisibleItemsFromStore(): void {
    this.visibleItems.splice(0, this.visibleItems.length, ...this.store.getVisibleItems());
  }
}

export function extractSystemNotification(event: RawStreamEvent): SystemNotification | null {
  const wire = event.wire;
  if (!wire || wire.variant !== 'SystemMessage') {
    return null;
  }

  return {
    createdAt: asNumber(wire.payload.created_at),
    level: asString(wire.payload.level),
    message: asString(wire.payload.message) ?? '',
  };
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

function statusBadge(status: string | null, flavor?: string): TemplateResult | typeof nothing {
  if (!status && !flavor) {
    return nothing;
  }

  const badgeLabel = status || flavor || '';
  const badgeFlavor = (status || flavor || '').toLowerCase();
  return html`<span class="pill pill-${badgeFlavor.replace(/[^a-z0-9]+/g, '-')}">${badgeLabel}</span>`;
}

export interface TimelineRenderContext {
  sessionId: string;
  currentSubagentId?: string;
  timelineStore?: TimelineStore;
  expandedSubagentIds?: ReadonlySet<string>;
  toggleSubagentExpansion?: (conversationId: string) => void;
  toggleTimelineItemExpansion?: (itemId: string) => void;
}

function toolRoute(sessionId: string, toolCallId: string, currentSubagentId?: string): AppRoute {
  if (currentSubagentId) {
    return {
      kind: 'subagent-tool',
      sessionId,
      subagentId: currentSubagentId,
      toolCallId,
    };
  }

  return {
    kind: 'tool',
    sessionId,
    toolCallId,
  };
}

function renderUser(item: UserTimelineItem): TemplateResult {
  return html`
    <article class="chat-bubble chat-bubble-user timeline-user">
      <div class="message-meta">You · ${formatTimestamp(item.createdAt)}</div>
      <pre class="timeline-pre message-bubble-content">${item.content}</pre>
    </article>
  `;
}

function renderAssistant(item: AssistantTimelineItem): TemplateResult {
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
        ? html`<div class="message-bubble-content markdown-content">${unsafeHTML(renderMarkdownToHtml(item.content))}</div>`
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

function renderTool(item: ToolTimelineItem, context: TimelineRenderContext): TemplateResult {
  return html`
    <article class="timeline-card chat-card chat-card-tool timeline-tool">
      <header class="chat-card-header">
        <div>
          <div class="chat-card-title">Tool · ${item.toolName || item.toolCallId}</div>
          <div class="chat-card-subtitle">${formatTimestamp(item.createdAt)}</div>
        </div>
        <div class="chat-card-actions">
          ${statusBadge(item.endStatus)}
          ${item.permissionState ? statusBadge(item.permissionState, item.permissionState) : nothing}
        </div>
      </header>
      ${item.toolArgs
        ? html`
            <details>
              <summary>Arguments</summary>
              <pre class="timeline-pre">${item.toolArgs}</pre>
            </details>
          `
        : nothing}
      ${item.output
        ? html`
            <details open>
              <summary>Output</summary>
              <pre class="timeline-pre">${item.output}</pre>
            </details>
          `
        : html`<div class="timeline-empty">Waiting for tool output…</div>`}
      <footer class="chat-card-footer">
        <span>tool call id: ${item.toolCallId}</span>
        <a
          href="${hrefForRoute(toolRoute(context.sessionId, item.toolCallId, context.currentSubagentId))}"
          target="_blank"
          rel="noopener noreferrer"
        >
          Open detail
        </a>
      </footer>
    </article>
  `;
}

function renderSubagent(item: SubagentTimelineItem, context: TimelineRenderContext): TemplateResult {
  const expanded = context.timelineStore?.isExpanded(item.id) ?? context.expandedSubagentIds?.has(item.conversationId) ?? false;
  const canToggle = Boolean(context.toggleTimelineItemExpansion || context.toggleSubagentExpansion);
  const toggle = () => {
    if (context.toggleTimelineItemExpansion) {
      context.toggleTimelineItemExpansion(item.id);
    } else {
      context.toggleSubagentExpansion?.(item.conversationId);
    }
  };

  return html`
    <article class="timeline-card chat-card chat-card-subagent timeline-subagent">
      <header class="chat-card-header">
        <div>
          <div class="chat-card-title">Subagent</div>
          <div class="chat-card-subtitle">${item.description || 'Subagent task'} · ${formatTimestamp(item.createdAt)}</div>
        </div>
        <div class="chat-card-actions">
          ${statusBadge(item.endStatus)}
          ${item.permissionState ? statusBadge(item.permissionState, item.permissionState) : nothing}
        </div>
      </header>
      ${item.input
        ? html`
            <details>
              <summary>Task input</summary>
              <pre class="timeline-pre">${item.input}</pre>
            </details>
          `
        : nothing}
      ${item.response
        ? html`
            <section>
              <div class="chat-card-subtitle">Latest response</div>
              <div class="subagent-response ${expanded ? 'subagent-response-expanded' : 'subagent-response-collapsed'}">
                <pre class="timeline-pre">${item.response}</pre>
              </div>
              ${canToggle
                ? html`
                    <button
                      class="button ghost subagent-toggle"
                      type="button"
                      @click=${toggle}
                    >
                      ${expanded ? 'Show less' : 'Show more'}
                    </button>
                  `
                : nothing}
            </section>
          `
        : html`<div class="timeline-empty">Waiting for subagent response…</div>`}
      <footer class="chat-card-footer">
        <span>${item.toolCallId ? `spawned by ${item.toolCallId}` : 'subagent session'}</span>
        <a
          href="${hrefForRoute({
            kind: 'subagent',
            sessionId: context.sessionId,
            subagentId: item.conversationId,
          })}"
          target="_blank"
          rel="noopener noreferrer"
        >
          Open conversation
        </a>
      </footer>
    </article>
  `;
}

function renderSystem(item: SystemTimelineItem): TemplateResult {
  return html`
    <article class="timeline-card chat-card compact-card chat-card-system timeline-system">
      <header class="chat-card-header">
        <div class="chat-card-title">System</div>
        <div class="chat-card-actions">
          ${statusBadge(item.level, item.level)}
          <span class="timeline-meta">${formatTimestamp(item.createdAt)}</span>
        </div>
      </header>
      <pre class="timeline-pre">${item.message}</pre>
    </article>
  `;
}

function renderSignal(item: SignalTimelineItem): TemplateResult {
  return html`
    <article class="timeline-card chat-card compact-card chat-card-signal timeline-signal">
      <header class="chat-card-header">
        <div class="chat-card-title">Event</div>
      </header>
      <div class="timeline-text">${item.label}</div>
      ${item.details ? html`<pre class="timeline-pre">${item.details}</pre>` : nothing}
    </article>
  `;
}

function renderRaw(item: RawTimelineItem): TemplateResult {
  return html`
    <article class="timeline-card chat-card compact-card chat-card-raw timeline-raw">
      <header class="chat-card-header">
        <div class="chat-card-title">${item.label}</div>
      </header>
      <pre class="timeline-pre">${prettyJson(item.rawJson)}</pre>
    </article>
  `;
}

export function renderTimelineItem(item: TimelineItem, context: TimelineRenderContext): TemplateResult {
  switch (item.kind) {
    case 'user':
      return renderUser(item);
    case 'assistant':
      return renderAssistant(item);
    case 'tool':
      return renderTool(item, context);
    case 'subagent':
      return renderSubagent(item, context);
    case 'system':
      return renderSystem(item);
    case 'signal':
      return renderSignal(item);
    case 'raw':
      return renderRaw(item);
  }
}

export function rawVariant(event: RawStreamEvent): string | null {
  return event.wire?.variant ?? null;
}
