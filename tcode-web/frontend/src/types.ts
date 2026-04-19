export interface AuthSessionStatus {
  authenticated: boolean;
}

export interface SessionSummary {
  id: string;
  description?: string | null;
  created_at: number | null;
  last_active_at: number | null;
  status: string;
}

export interface SessionsResponse {
  sessions: SessionSummary[];
}

export interface CreateSessionResponse {
  id: string;
}

export interface PermissionKey {
  tool: string;
  key: string;
  value: string;
}

export interface PendingPermissionInfo {
  tool: string;
  prompt: string;
  key: string;
  value: string;
  request_id: string;
  preview_file_path?: string | null;
  once_only: boolean;
}

export interface PermissionState {
  pending: PendingPermissionInfo[];
  session: PermissionKey[];
  project: PermissionKey[];
}

export type PermissionDecisionPayload =
  | 'AllowOnce'
  | 'AllowSession'
  | 'AllowProject'
  | { Deny: { reason: string | null } };

export type AppRoute =
  | { kind: 'login' }
  | { kind: 'home' }
  | { kind: 'session'; sessionId: string }
  | { kind: 'tool'; sessionId: string; toolCallId: string }
  | { kind: 'subagent'; sessionId: string; subagentId: string }
  | {
      kind: 'subagent-tool';
      sessionId: string;
      subagentId: string;
      toolCallId: string;
    };

export interface WireMessageEnvelope {
  variant: string;
  payload: Record<string, unknown>;
  raw: unknown;
  rawText: string;
}

export interface RawStreamEvent {
  rawText: string;
  rawJson: unknown;
  wire: WireMessageEnvelope | null;
}

export interface AssistantTimelineItem {
  kind: 'assistant';
  msgId: number | null;
  createdAt: number | null;
  content: string;
  thinking: string;
  endStatus: string | null;
  error: string | null;
  inputTokens: number | null;
  outputTokens: number | null;
  reasoningTokens: number | null;
}

export interface UserTimelineItem {
  kind: 'user';
  msgId: number | null;
  createdAt: number | null;
  content: string;
}

export interface ToolTimelineItem {
  kind: 'tool';
  msgId: number | null;
  toolCallId: string;
  createdAt: number | null;
  toolName: string;
  toolArgs: string;
  output: string;
  endStatus: string | null;
  inputTokens: number | null;
  outputTokens: number | null;
  permissionState: 'waiting' | 'approved' | null;
}

export interface SubagentTimelineItem {
  kind: 'subagent';
  msgId: number | null;
  toolCallId: string | null;
  conversationId: string;
  createdAt: number | null;
  description: string;
  input: string;
  response: string;
  endStatus: string | null;
  inputTokens: number | null;
  outputTokens: number | null;
  permissionState: 'waiting' | 'approved' | 'denied' | null;
}

export interface SystemTimelineItem {
  kind: 'system';
  msgId: number | null;
  createdAt: number | null;
  level: string;
  message: string;
}

export interface SignalTimelineItem {
  kind: 'signal';
  label: string;
  details: string;
}

export interface RawTimelineItem {
  kind: 'raw';
  label: string;
  rawText: string;
  rawJson: unknown;
}

export type TimelineItem =
  | UserTimelineItem
  | AssistantTimelineItem
  | ToolTimelineItem
  | SubagentTimelineItem
  | SystemTimelineItem
  | SignalTimelineItem
  | RawTimelineItem;
