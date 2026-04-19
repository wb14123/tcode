import { html, nothing, type TemplateResult } from 'lit';
import { unsafeHTML } from 'lit/directives/unsafe-html.js';

import { renderMarkdownToHtml } from './markdown';
import { hrefForRoute } from './router';
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

function createAssistant(msgId: number | null): AssistantTimelineItem {
  return {
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

function createRawItem(event: RawStreamEvent, label: string): RawTimelineItem {
  return {
    kind: 'raw',
    label,
    rawText: event.rawText,
    rawJson: event.rawJson,
  };
}

function createSignal(label: string, details = ''): SignalTimelineItem {
  return {
    kind: 'signal',
    label,
    details,
  };
}

function ensureActiveAssistant(
  items: TimelineItem[],
  activeAssistant: AssistantTimelineItem | null,
): AssistantTimelineItem {
  if (activeAssistant) {
    return activeAssistant;
  }

  const item = createAssistant(null);
  items.push(item);
  return item;
}

function getOrCreateTool(
  items: TimelineItem[],
  map: Map<string, ToolTimelineItem>,
  toolCallId: string,
): ToolTimelineItem {
  const existing = map.get(toolCallId);
  if (existing) {
    return existing;
  }

  const item = createTool(toolCallId);
  items.push(item);
  map.set(toolCallId, item);
  return item;
}

function getOrCreateSubagent(
  items: TimelineItem[],
  map: Map<string, SubagentTimelineItem>,
  conversationId: string,
): SubagentTimelineItem {
  const existing = map.get(conversationId);
  if (existing) {
    return existing;
  }

  const item = createSubagent(conversationId);
  items.push(item);
  map.set(conversationId, item);
  return item;
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

export function buildConversationTimeline(events: RawStreamEvent[]): TimelineItem[] {
  const items: TimelineItem[] = [];
  let activeAssistant: AssistantTimelineItem | null = null;
  const tools = new Map<string, ToolTimelineItem>();
  const toolCallIdByIndex = new Map<number, string>();
  const subagents = new Map<string, SubagentTimelineItem>();
  const pendingSubagentsByToolCall = new Map<string, SubagentTimelineItem>();
  const pendingSubagentToolByIndex = new Map<number, string>();

  for (const event of events) {
    const wire = event.wire;
    if (!wire) {
      items.push(createRawItem(event, 'Raw event'));
      continue;
    }

    const { variant, payload } = wire;

    switch (variant) {
      case 'UserMessage': {
        const item: UserTimelineItem = {
          kind: 'user',
          msgId: asNumber(payload.msg_id),
          createdAt: asNumber(payload.created_at),
          content: asString(payload.content) ?? '',
        };
        items.push(item);
        break;
      }
      case 'AssistantMessageStart': {
        const item = createAssistant(asNumber(payload.msg_id));
        item.createdAt = asNumber(payload.created_at);
        items.push(item);
        activeAssistant = item;
        break;
      }
      case 'AssistantMessageChunk': {
        const item = ensureActiveAssistant(items, activeAssistant);
        item.msgId ??= asNumber(payload.msg_id);
        item.content += asString(payload.content) ?? '';
        activeAssistant = item;
        break;
      }
      case 'AssistantThinkingChunk': {
        const item = ensureActiveAssistant(items, activeAssistant);
        item.msgId ??= asNumber(payload.msg_id);
        item.thinking += asString(payload.content) ?? '';
        activeAssistant = item;
        break;
      }
      case 'AssistantMessageEnd': {
        const item = ensureActiveAssistant(items, activeAssistant);
        item.msgId ??= asNumber(payload.msg_id);
        item.endStatus = asString(payload.end_status);
        item.error = asString(payload.error);
        item.inputTokens = asNumber(payload.input_tokens);
        item.outputTokens = asNumber(payload.output_tokens);
        item.reasoningTokens = asNumber(payload.reasoning_tokens);
        activeAssistant = null;
        break;
      }
      case 'AssistantToolCallStart': {
        const toolCallId = asString(payload.tool_call_id);
        const toolCallIndex = asNumber(payload.tool_call_index);
        if (!toolCallId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = getOrCreateTool(items, tools, toolCallId);
        item.msgId = asNumber(payload.msg_id);
        item.createdAt = asNumber(payload.created_at);
        item.toolName = asString(payload.tool_name) ?? item.toolName;
        if (toolCallIndex !== null) {
          toolCallIdByIndex.set(toolCallIndex, toolCallId);
        }
        break;
      }
      case 'AssistantToolCallArgChunk': {
        const index = asNumber(payload.tool_call_index);
        const toolCallId = index !== null ? toolCallIdByIndex.get(index) : null;
        if (!toolCallId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = getOrCreateTool(items, tools, toolCallId);
        item.toolName = asString(payload.tool_name) ?? item.toolName;
        item.toolArgs += asString(payload.content) ?? '';
        break;
      }
      case 'ToolMessageStart': {
        const toolCallId = asString(payload.tool_call_id);
        if (!toolCallId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = getOrCreateTool(items, tools, toolCallId);
        item.msgId = asNumber(payload.msg_id);
        item.createdAt = asNumber(payload.created_at);
        item.toolName = asString(payload.tool_name) ?? item.toolName;
        item.toolArgs = asString(payload.tool_args) ?? item.toolArgs;
        break;
      }
      case 'ToolOutputChunk': {
        const toolCallId = asString(payload.tool_call_id);
        if (!toolCallId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = getOrCreateTool(items, tools, toolCallId);
        item.toolName = asString(payload.tool_name) ?? item.toolName;
        item.output += asString(payload.content) ?? '';
        break;
      }
      case 'ToolMessageEnd': {
        const toolCallId = asString(payload.tool_call_id);
        if (!toolCallId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = getOrCreateTool(items, tools, toolCallId);
        item.msgId = asNumber(payload.msg_id);
        item.endStatus = asString(payload.end_status);
        item.inputTokens = asNumber(payload.input_tokens);
        item.outputTokens = asNumber(payload.output_tokens);
        break;
      }
      case 'SubAgentInputStart': {
        const toolCallId = asString(payload.tool_call_id);
        const toolCallIndex = asNumber(payload.tool_call_index);
        if (!toolCallId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = createSubagent(`pending:${toolCallId}`);
        item.msgId = asNumber(payload.msg_id);
        item.toolCallId = toolCallId;
        item.createdAt = asNumber(payload.created_at);
        item.description = asString(payload.tool_name) ?? '';
        items.push(item);
        pendingSubagentsByToolCall.set(toolCallId, item);
        if (toolCallIndex !== null) {
          pendingSubagentToolByIndex.set(toolCallIndex, toolCallId);
        }
        break;
      }
      case 'SubAgentInputChunk': {
        const toolCallId = asNumber(payload.tool_call_index) !== null
          ? pendingSubagentToolByIndex.get(asNumber(payload.tool_call_index) as number)
          : null;
        if (!toolCallId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = pendingSubagentsByToolCall.get(toolCallId);
        if (!item) {
          items.push(createRawItem(event, variant));
          break;
        }
        item.description = asString(payload.tool_name) ?? item.description;
        item.input += asString(payload.content) ?? '';
        break;
      }
      case 'SubAgentStart':
      case 'SubAgentContinue': {
        const conversationId = asString(payload.conversation_id);
        const toolCallId = asString(payload.tool_call_id);
        if (!conversationId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const pending = toolCallId ? pendingSubagentsByToolCall.get(toolCallId) : undefined;
        if (pending && toolCallId) {
          pending.conversationId = conversationId;
          subagents.set(conversationId, pending);
          pendingSubagentsByToolCall.delete(toolCallId);
        }
        const item = pending ?? getOrCreateSubagent(items, subagents, conversationId);

        item.msgId = asNumber(payload.msg_id);
        item.toolCallId = toolCallId ?? item.toolCallId;
        item.description = asString(payload.description) ?? item.description;
        break;
      }
      case 'SubAgentTurnEnd':
      case 'SubAgentEnd': {
        const conversationId = asString(payload.conversation_id);
        if (!conversationId) {
          items.push(createRawItem(event, variant));
          break;
        }

        const item = getOrCreateSubagent(items, subagents, conversationId);
        item.msgId = asNumber(payload.msg_id);
        item.response = asString(payload.response) ?? item.response;
        item.endStatus = asString(payload.end_status);
        item.inputTokens = asNumber(payload.input_tokens);
        item.outputTokens = asNumber(payload.output_tokens);
        break;
      }
      case 'SystemMessage': {
        const item: SystemTimelineItem = {
          kind: 'system',
          msgId: asNumber(payload.msg_id),
          createdAt: asNumber(payload.created_at),
          level: asString(payload.level) ?? 'Info',
          message: asString(payload.message) ?? '',
        };
        items.push(item);
        break;
      }
      case 'ToolRequestPermission': {
        const toolCallId = asString(payload.tool_call_id);
        if (toolCallId && tools.has(toolCallId)) {
          tools.get(toolCallId)!.permissionState = 'waiting';
        } else {
          items.push(createSignal('Tool waiting for permission', toolCallId ?? ''));
        }
        break;
      }
      case 'ToolPermissionApproved': {
        const toolCallId = asString(payload.tool_call_id);
        if (toolCallId && tools.has(toolCallId)) {
          tools.get(toolCallId)!.permissionState = 'approved';
        } else {
          items.push(createSignal('Tool permission approved', toolCallId ?? ''));
        }
        break;
      }
      case 'SubAgentWaitingPermission': {
        const conversationId = asString(payload.conversation_id);
        if (conversationId) {
          getOrCreateSubagent(items, subagents, conversationId).permissionState = 'waiting';
        } else {
          items.push(createRawItem(event, variant));
        }
        break;
      }
      case 'SubAgentPermissionApproved': {
        const conversationId = asString(payload.conversation_id);
        if (conversationId) {
          getOrCreateSubagent(items, subagents, conversationId).permissionState = 'approved';
        } else {
          items.push(createRawItem(event, variant));
        }
        break;
      }
      case 'SubAgentPermissionDenied': {
        const conversationId = asString(payload.conversation_id);
        if (conversationId) {
          getOrCreateSubagent(items, subagents, conversationId).permissionState = 'denied';
        } else {
          items.push(createRawItem(event, variant));
        }
        break;
      }
      case 'PermissionUpdated':
        items.push(createSignal('Permission state updated'));
        break;
      case 'AssistantRequestEnd':
        items.push(createSignal('Assistant requested conversation end'));
        break;
      case 'UserRequestEnd':
        items.push(createSignal('User requested conversation end', asString(payload.conversation_id) ?? ''));
        break;
      case 'ToolCallResolved':
        items.push(createSignal('Tool call resolved', asString(payload.tool_call_id) ?? ''));
        break;
      case 'AggregateTokenUpdate':
        items.push(createSignal('Aggregate tokens updated'));
        break;
      case 'SubAgentTokenRollup':
        items.push(createSignal('Subagent token rollup recorded'));
        break;
      default:
        items.push(createRawItem(event, variant));
        break;
    }
  }

  return items.filter(shouldRenderTimelineItem);
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
  expandedSubagentIds?: ReadonlySet<string>;
  toggleSubagentExpansion?: (conversationId: string) => void;
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
  const expanded = context.expandedSubagentIds?.has(item.conversationId) ?? false;
  const canToggle = Boolean(context.toggleSubagentExpansion);

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
                      @click=${() => context.toggleSubagentExpansion?.(item.conversationId)}
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
