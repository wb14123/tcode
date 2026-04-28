import { html, nothing, type TemplateResult } from 'lit';

import type { AppRoute, SubagentTimelineItem, ToolTimelineItem } from './types.ts';
import { isSpecialWebToolName, specialToolArgsPresentation } from './tool-args.ts';

export type CompactRowType = 'tool' | 'subagent';
export type RowStatusKind = CompactRowType;

const VISIBLE_FAILURE_STATUSES = new Set(['failed', 'error', 'errored', 'denied', 'userdenied', 'rejected', 'cancelled', 'canceled', 'timeout', 'timedout']);
const FAILURE_STATUS_PRESENTATION = new Map<string, { className: string; label: string }>([
  ['userdenied', { className: 'denied', label: 'Denied' }],
  ['denied', { className: 'denied', label: 'Denied' }],
  ['canceled', { className: 'cancelled', label: 'Cancelled' }],
  ['timedout', { className: 'timeout', label: 'Timeout' }],
]);

function normalizedStatus(value: string | null | undefined): string {
  return (value ?? '').trim().toLowerCase();
}

function statusLabel(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) {
    return '';
  }
  return trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}

function statusPresentation(value: string): { className: string; label: string } {
  const normalized = normalizedStatus(value);
  const fallbackClass = normalized.replace(/[^a-z0-9]+/g, '-');
  return FAILURE_STATUS_PRESENTATION.get(normalized) ?? { className: fallbackClass, label: statusLabel(value) };
}

function visibleFailureStatus(...values: Array<string | null | undefined>): { className: string; label: string } | null {
  for (const value of values) {
    if (VISIBLE_FAILURE_STATUSES.has(normalizedStatus(value))) {
      return statusPresentation(value?.trim() ?? '');
    }
  }
  return null;
}

export function rowStatusIndicator(
  kind: RowStatusKind,
  status: string | null | undefined,
  permissionState: string | null | undefined,
  pending = false,
): TemplateResult | typeof nothing {
  const failure = visibleFailureStatus(permissionState, status);
  if (failure) {
    return html`<span class="pill pill-${failure.className} row-failure-status">${failure.label}</span>`;
  }

  const normalizedPermission = normalizedStatus(permissionState);
  const normalizedEndStatus = normalizedStatus(status);
  const label = normalizedPermission === 'waiting'
    ? `${kind === 'tool' ? 'Tool' : 'Subagent'} waiting for approval`
    : normalizedPermission === 'approved' && !normalizedEndStatus
      ? `${kind === 'tool' ? 'Tool' : 'Subagent'} approval granted`
      : pending
        ? `${kind === 'tool' ? 'Tool' : 'Subagent'} pending`
        : !normalizedEndStatus
          ? `${kind === 'tool' ? 'Tool' : 'Subagent'} running`
          : '';

  return label
    ? html`<span class="row-state-indicator" aria-label=${label} title=${label}><span class="sr-only">${label}</span></span>`
    : nothing;
}

export function firstLinePreview(value: string, limit = 160): string {
  const firstLine = value.trim().split(/\r?\n/, 1)[0] ?? '';
  if (firstLine.length <= limit) {
    return firstLine;
  }
  return `${firstLine.slice(0, limit)}…`;
}

export function countText(label: string, value: string): string | null {
  if (!value) {
    return null;
  }
  return `${label}: ${value.length.toLocaleString()} chars`;
}

export function compactText(...values: Array<string | null | undefined>): string {
  return values.filter((value): value is string => Boolean(value)).join(' · ');
}

export function compactRowTypeMarker(kind: CompactRowType): TemplateResult {
  if (kind === 'tool') {
    return html`
      <span class="compact-row-type compact-row-type-tool" aria-hidden="true">
        <svg viewBox="0 0 16 16" focusable="false">
          <rect x="2.5" y="3" width="11" height="10" rx="2" fill="none" stroke="currentColor" stroke-width="1.5"></rect>
          <path d="M5 6.5 7 8 5 9.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"></path>
          <path d="M8.5 10.25h2.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"></path>
        </svg>
      </span>
    `;
  }

  return html`
    <span class="compact-row-type compact-row-type-subagent" aria-hidden="true">
      <svg viewBox="0 0 16 16" focusable="false">
        <path d="M5.5 5.5h5M5.5 10.5h5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"></path>
        <circle cx="4" cy="5.5" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4"></circle>
        <circle cx="12" cy="5.5" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4"></circle>
        <circle cx="4" cy="10.5" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4"></circle>
        <circle cx="12" cy="10.5" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4"></circle>
      </svg>
    </span>
  `;
}

export function renderExpandableRowTitle(
  kind: CompactRowType,
  title: string,
  expanded: boolean,
  status: TemplateResult | typeof nothing = nothing,
  showToggle = true,
): TemplateResult {
  return html`
    ${compactRowTypeMarker(kind)}
    <span class="row-toggle-marker" aria-hidden="true">
      ${showToggle
        ? expanded
          ? html`<svg viewBox="0 0 12 12" focusable="false"><path d="M3 4.5 6 7.5 9 4.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"></path></svg>`
          : html`<svg viewBox="0 0 12 12" focusable="false"><path d="M4.5 3 7.5 6 4.5 9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"></path></svg>`
        : nothing}
    </span>
    <span class="compact-row-title">${title}</span>
    ${status === nothing ? nothing : html`<span class="compact-row-status">${status}</span>`}
  `;
}

export interface ExpandableRowToggleOptions {
  ariaLabel: string;
  onClick: () => void;
  onKeydown: (event: KeyboardEvent) => void;
}

export interface CollapsedExpandableRowOptions {
  kind: CompactRowType;
  cardClass: string;
  title: string;
  status?: TemplateResult | typeof nothing;
  toggle: ExpandableRowToggleOptions;
}

export function renderCollapsedExpandableRow(options: CollapsedExpandableRowOptions): TemplateResult {
  return html`
    <article
      class=${`timeline-card chat-card ${options.cardClass} expandable-row is-collapsed`}
      role="button"
      tabindex="0"
      aria-expanded="false"
      aria-label=${options.toggle.ariaLabel}
      @click=${options.toggle.onClick}
      @keydown=${options.toggle.onKeydown}
    >
      ${renderExpandableRowTitle(options.kind, options.title, false, options.status ?? nothing)}
    </article>
  `;
}

export interface ExpandedExpandableRowOptions {
  kind: CompactRowType;
  cardClass: string;
  title: string;
  status?: TemplateResult | typeof nothing;
  toggle?: ExpandableRowToggleOptions;
  action?: TemplateResult | typeof nothing;
  body: TemplateResult | typeof nothing;
  footer?: TemplateResult | typeof nothing;
}

export function renderExpandedExpandableRow(options: ExpandedExpandableRowOptions): TemplateResult {
  const toggle = options.toggle;
  const canToggle = toggle !== undefined;
  return html`
    <article class=${`timeline-card chat-card ${options.cardClass} ${canToggle ? 'expandable-row is-expanded' : ''}`}>
      <header class="chat-card-header expanded-row-header">
        <div
          class=${canToggle ? 'expandable-row-header expanded-row-disclosure' : 'expanded-row-disclosure'}
          role=${canToggle ? 'button' : nothing}
          tabindex=${canToggle ? '0' : nothing}
          aria-expanded=${canToggle ? 'true' : nothing}
          aria-label=${canToggle ? toggle.ariaLabel : nothing}
          @click=${canToggle ? toggle.onClick : nothing}
          @keydown=${canToggle ? toggle.onKeydown : nothing}
        >
          ${renderExpandableRowTitle(options.kind, options.title, true, options.status ?? nothing, canToggle)}
        </div>
      </header>
      ${options.action ?? nothing}
      <div class="expandable-row-body">
        ${options.body}
        ${options.footer === undefined || options.footer === nothing ? nothing : html`<footer class="chat-card-footer">${options.footer}</footer>`}
      </div>
    </article>
  `;
}

export function renderExpandedRowAction(label: string, href: string): TemplateResult {
  return html`
    <div class="expanded-row-action-row">
      <a
        class="expanded-row-action"
        href="${href}"
        target="_blank"
        rel="noopener noreferrer"
        @click=${(event: Event) => event.stopPropagation()}
      >
        ${label}
      </a>
    </div>
  `;
}

export function renderToolFooterContent(item: Pick<ToolTimelineItem, 'toolCallId'>): TemplateResult {
  return html`<span>tool call id: ${item.toolCallId}</span>`;
}

export function renderSubagentFooterContent(item: Pick<SubagentTimelineItem, 'toolCallId' | 'pending'>): TemplateResult {
  return html`
    <span>${item.toolCallId ? `spawned by ${item.toolCallId}` : item.pending ? 'pending subagent input' : 'subagent session'}</span>
    ${item.pending ? html`<span>Waiting for subagent conversation…</span>` : nothing}
  `;
}

export function toolDetailRoute(sessionId: string, toolCallId: string, currentSubagentId?: string): AppRoute {
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

export function subagentDetailRoute(sessionId: string, subagentId: string): AppRoute {
  return {
    kind: 'subagent',
    sessionId,
    subagentId,
  };
}

export function toolCollapsedPreview(item: Pick<ToolTimelineItem, 'toolName' | 'toolArgs' | 'output'>): string {
  const argsPresentation = specialToolArgsPresentation(item.toolName, item.toolArgs);
  const fallbackPreview = isSpecialWebToolName(item.toolName) && item.toolArgs ? firstLinePreview(item.toolArgs) : firstLinePreview(item.output || item.toolArgs);
  return (argsPresentation?.collapsedSummary ?? fallbackPreview) || 'Waiting for tool output…';
}

export function toolRowTitle(item: Pick<ToolTimelineItem, 'toolName' | 'toolArgs' | 'output'>): string {
  return compactText(item.toolName || 'Tool call', toolCollapsedPreview(item));
}

const SUBAGENT_PROMPT_PREFIX = /^\s*You\s+are\s+a\s+subagent\.\s*/i;

function stripSubagentPromptPrefix(value: string): string {
  return value.replace(SUBAGENT_PROMPT_PREFIX, '').trim();
}

function stringField(record: Record<string, unknown>, key: string): string {
  const value = record[key];
  return typeof value === 'string' ? value : '';
}

function decodeJsonStringFragment(value: string): string {
  try {
    return JSON.parse(`"${value}"`) as string;
  } catch {
    return value
      .replace(/\\n/g, '\n')
      .replace(/\\r/g, '\r')
      .replace(/\\t/g, '\t')
      .replace(/\\"/g, '"')
      .replace(/\\\\/g, '\\');
  }
}

function extractSubagentPromptFromJsonInput(input: string): string {
  const trimmed = input.trim();
  if (!trimmed.startsWith('{')) {
    return '';
  }

  try {
    const parsed = JSON.parse(trimmed) as unknown;
    if (typeof parsed === 'object' && parsed !== null && !Array.isArray(parsed)) {
      const record = parsed as Record<string, unknown>;
      return stringField(record, 'task') || stringField(record, 'prompt') || stringField(record, 'message');
    }
  } catch {
    // Fall through to best-effort extraction for streaming partial JSON.
  }

  const match = trimmed.match(/"(?:task|prompt|message)"\s*:\s*"((?:\\.|[^"\\])*)/s);
  return match ? decodeJsonStringFragment(match[1] ?? '') : '';
}

function isInternalSubagentIdentifier(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed || /\s/.test(trimmed)) {
    return false;
  }

  return (
    /^(?:subagent:)?(?:pending:)?(?:tool|index|orphan):/i.test(trimmed) ||
    /^(?:subagent:)?pending:/i.test(trimmed) ||
    /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(trimmed) ||
    /^(?:conversation|session|runtime|client)[_-]?[a-z0-9-]{8,}$/i.test(trimmed)
  );
}

function titleCandidate(value: string): string {
  const preview = firstLinePreview(value);
  return isInternalSubagentIdentifier(preview) ? '' : preview;
}

export function subagentInputPreview(input: string): string {
  const prompt = extractSubagentPromptFromJsonInput(input);
  if (prompt) {
    return titleCandidate(stripSubagentPromptPrefix(prompt));
  }
  if (input.trim().startsWith('{')) {
    return '';
  }
  return titleCandidate(stripSubagentPromptPrefix(input));
}

export function subagentRowTitle(item: Pick<SubagentTimelineItem, 'input' | 'response' | 'description'>): string {
  return subagentInputPreview(item.input) || titleCandidate(item.response) || titleCandidate(item.description) || 'Waiting for subagent…';
}

export function renderExpandedToolBody(item: Pick<ToolTimelineItem, 'toolName' | 'toolArgs' | 'output'>): TemplateResult {
  const argsPresentation = specialToolArgsPresentation(item.toolName, item.toolArgs);
  return html`
    ${item.toolArgs ? renderExpandedDetailSection('Arguments', argsPresentation?.expandedText ?? item.toolArgs) : nothing}
    ${item.output
      ? renderExpandedDetailSection('Output', item.output)
      : renderExpandedDetailSection('Output', 'Waiting for tool output…', true)}
  `;
}

export function renderExpandedSubagentBody(item: Pick<SubagentTimelineItem, 'input' | 'response'>): TemplateResult {
  return html`
    ${item.input ? renderExpandedDetailSection('Task input', item.input) : nothing}
    ${item.response
      ? renderExpandedDetailSection('Latest response', item.response)
      : renderExpandedDetailSection('Latest response', 'Waiting for subagent response…', true)}
  `;
}

export function renderExpandedDetailSection(label: string, value: string, muted = false): TemplateResult {
  return html`
    <section class="expanded-detail-section">
      <div class="expanded-detail-label">${label}</div>
      ${muted ? html`<div class="timeline-empty expanded-detail-empty">${value}</div>` : html`<pre class="timeline-pre expanded-detail-pre">${value}</pre>`}
    </section>
  `;
}
