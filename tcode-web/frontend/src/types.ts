export interface AuthSessionStatus {
  authenticated: boolean;
  secure_session_cookie: boolean;
}

export type SessionMode = 'normal' | 'web_only';

export interface SessionSummary {
  id: string;
  description?: string | null;
  created_at: number | null;
  last_active_at: number | null;
  status: string;
  mode: SessionMode;
}

export interface SessionsResponse {
  sessions: SessionSummary[];
}

export interface CreateSessionResponse {
  id: string;
}

export type RuntimeOwnerKind = 'Cli' | 'Web' | 'Serve';

export interface SessionRuntimeInfo {
  active: boolean;
  owner_kind: RuntimeOwnerKind;
  session_mode: SessionMode;
  active_lease_count: number;
  lease_timeout_seconds: number;
  runtime_id: string;
}

export interface RuntimeInfoResponse {
  runtime_info: SessionRuntimeInfo;
}

export interface LeaseResponse {
  active: boolean;
  client_id: string | null;
  lease_timeout_seconds: number;
  heartbeat_interval_seconds: number;
  runtime_info: SessionRuntimeInfo;
}

export interface RegisterLeaseRequest {
  client_label?: string;
  resume?: boolean;
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

export interface TimelineItemBase {
  id: string;
  revision: number;
}

export interface AssistantTextBlock {
  kind: 'text';
  text: string;
}

export interface AssistantImageBlock {
  kind: 'image';
  imageId: string | null;
  pending: boolean;
  image?: {
    relative_path: string;
    media_type: string;
  };
}

export type AssistantContentBlock = AssistantTextBlock | AssistantImageBlock;

export interface AssistantTimelineItem extends TimelineItemBase {
  kind: 'assistant';
  msgId: number | null;
  createdAt: number | null;
  contentBlocks: AssistantContentBlock[];
  thinking: string;
  endStatus: string | null;
  error: string | null;
  inputTokens: number | null;
  outputTokens: number | null;
  reasoningTokens: number | null;
  cacheCreationTokens: number | null;
  cacheReadTokens: number | null;
}

export interface UserTimelineItem extends TimelineItemBase {
  kind: 'user';
  msgId: number | null;
  createdAt: number | null;
  content: string;
  images: string[]; // relative image filenames within images/ dir
}

export interface ToolTimelineItem extends TimelineItemBase {
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

export interface SubagentTimelineItem extends TimelineItemBase {
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
  cacheCreationTokens: number | null;
  cacheReadTokens: number | null;
  permissionState: 'waiting' | 'approved' | 'denied' | null;
  pending?: boolean;
}

export interface SystemTimelineItem extends TimelineItemBase {
  kind: 'system';
  msgId: number | null;
  createdAt: number | null;
  level: string;
  message: string;
}

export interface SignalTimelineItem extends TimelineItemBase {
  kind: 'signal';
  label: string;
  details: string;
}

export interface RawTimelineItem extends TimelineItemBase {
  kind: 'raw';
  createdAt: number | null;
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

// Must match llm_rs::permission::ALL_SCOPES in permission.rs
// When adding a new scope/key to the Rust side, update this constant too.
export const ALL_SCOPES: Record<string, string[]> = {
  bash: ['command'],
  file_read: ['path'],
  file_write: ['path'],
  web_fetch: ['hostname'],
};
