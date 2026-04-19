import { runtimeConfig } from './config';
import type {
  AuthSessionStatus,
  CreateSessionResponse,
  PendingPermissionInfo,
  PermissionDecisionPayload,
  PermissionKey,
  PermissionState,
  SessionsResponse,
} from './types';

function authExpired(): void {
  window.dispatchEvent(new CustomEvent('tcode-auth-required'));
}

function makeUrl(path: string): string {
  const cleanPath = path.startsWith('/') ? path.slice(1) : path;
  const base = runtimeConfig.apiBase || window.location.origin;
  return new URL(cleanPath, base).toString();
}

export class ApiError extends Error {
  status: number;
  bodyText: string;

  constructor(status: number, bodyText: string) {
    super(bodyText || `Request failed with status ${status}`);
    this.status = status;
    this.bodyText = bodyText;
  }
}

async function readErrorText(response: Response): Promise<string> {
  const text = await response.text();
  if (!text) {
    return '';
  }

  try {
    const parsed = JSON.parse(text) as { error?: string };
    return parsed.error || text;
  } catch {
    return text;
  }
}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(makeUrl(path), {
    credentials: 'include',
    ...init,
    headers: {
      'Content-Type': 'application/json',
      ...(init?.headers ?? {}),
    },
  });

  if (response.status === 401) {
    authExpired();
  }

  if (!response.ok) {
    throw new ApiError(response.status, await readErrorText(response));
  }

  return response;
}

async function jsonRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await request(path, init);
  return (await response.json()) as T;
}

async function textRequest(path: string): Promise<string> {
  const response = await request(path, { headers: {} });
  return response.text();
}

export class ReplayAwareBuffer {
  private lines: string[] = [];
  private replayCursor = 0;

  reset(): void {
    this.lines = [];
    this.replayCursor = 0;
  }

  beginReplay(): void {
    this.replayCursor = 0;
  }

  accept(line: string): boolean {
    if (this.replayCursor < this.lines.length && this.lines[this.replayCursor] === line) {
      this.replayCursor += 1;
      return false;
    }

    this.lines.push(line);
    this.replayCursor = this.lines.length;
    return true;
  }
}

export function openEventStream(path: string): EventSource {
  return new EventSource(makeUrl(path), {
    withCredentials: runtimeConfig.eventSourceWithCredentials,
  });
}

function jsonBody(body: unknown): string {
  return JSON.stringify(body);
}

export const api = {
  getAuthSession(): Promise<AuthSessionStatus> {
    return jsonRequest<AuthSessionStatus>('api/auth/session', { headers: {} });
  },

  login(secret: string): Promise<AuthSessionStatus> {
    return jsonRequest<AuthSessionStatus>('api/auth/login', {
      method: 'POST',
      body: jsonBody({ secret }),
    });
  },

  logout(): Promise<void> {
    return request('api/auth/logout', { method: 'POST', headers: {} }).then(() => undefined);
  },

  listSessions(): Promise<SessionsResponse> {
    return jsonRequest<SessionsResponse>('api/sessions', { headers: {} });
  },

  createSession(initialPrompt: string): Promise<CreateSessionResponse> {
    return jsonRequest<CreateSessionResponse>('api/sessions', {
      method: 'POST',
      body: jsonBody({ initial_prompt: initialPrompt }),
    });
  },

  getSessionStatus(sessionId: string): Promise<string> {
    return textRequest(`api/sessions/${encodeURIComponent(sessionId)}/status.txt`);
  },

  getSessionUsage(sessionId: string): Promise<string> {
    return textRequest(`api/sessions/${encodeURIComponent(sessionId)}/usage.txt`);
  },

  getSessionTokenUsage(sessionId: string): Promise<string> {
    return textRequest(`api/sessions/${encodeURIComponent(sessionId)}/token_usage.txt`);
  },

  sendSessionMessage(sessionId: string, text: string): Promise<void> {
    return request(`api/sessions/${encodeURIComponent(sessionId)}/messages`, {
      method: 'POST',
      body: jsonBody({ text }),
    }).then(() => undefined);
  },

  cancelSession(sessionId: string): Promise<void> {
    return request(`api/sessions/${encodeURIComponent(sessionId)}/cancel`, {
      method: 'POST',
    }).then(() => undefined);
  },

  sessionDisplayPath(sessionId: string): string {
    return `api/sessions/${encodeURIComponent(sessionId)}/display.jsonl`;
  },

  sessionToolCallDisplayPath(sessionId: string, toolCallId: string): string {
    return `api/sessions/${encodeURIComponent(sessionId)}/tool-calls/${encodeURIComponent(toolCallId)}.jsonl`;
  },

  getSessionToolCallStatus(sessionId: string, toolCallId: string): Promise<string> {
    return textRequest(
      `api/sessions/${encodeURIComponent(sessionId)}/tool-calls/${encodeURIComponent(toolCallId)}/status.txt`,
    );
  },

  cancelSessionToolCall(sessionId: string, toolCallId: string): Promise<void> {
    return request(
      `api/sessions/${encodeURIComponent(sessionId)}/tool-calls/${encodeURIComponent(toolCallId)}/cancel`,
      { method: 'POST' },
    ).then(() => undefined);
  },

  getSubagentStatus(sessionId: string, subagentId: string): Promise<string> {
    return textRequest(
      `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/status.txt`,
    );
  },

  getSubagentTokenUsage(sessionId: string, subagentId: string): Promise<string> {
    return textRequest(
      `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/token_usage.txt`,
    );
  },

  sendSubagentMessage(sessionId: string, subagentId: string, text: string): Promise<void> {
    return request(
      `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/messages`,
      {
        method: 'POST',
        body: jsonBody({ text }),
      },
    ).then(() => undefined);
  },

  finishSubagent(sessionId: string, subagentId: string): Promise<void> {
    return request(
      `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/finish`,
      {
        method: 'POST',
      },
    ).then(() => undefined);
  },

  cancelSubagent(sessionId: string, subagentId: string): Promise<void> {
    return request(
      `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/cancel`,
      {
        method: 'POST',
      },
    ).then(() => undefined);
  },

  subagentDisplayPath(sessionId: string, subagentId: string): string {
    return `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/display.jsonl`;
  },

  subagentToolCallDisplayPath(sessionId: string, subagentId: string, toolCallId: string): string {
    return `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/tool-calls/${encodeURIComponent(toolCallId)}.jsonl`;
  },

  getSubagentToolCallStatus(sessionId: string, subagentId: string, toolCallId: string): Promise<string> {
    return textRequest(
      `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/tool-calls/${encodeURIComponent(toolCallId)}/status.txt`,
    );
  },

  cancelSubagentToolCall(sessionId: string, subagentId: string, toolCallId: string): Promise<void> {
    return request(
      `api/sessions/${encodeURIComponent(sessionId)}/subagents/${encodeURIComponent(subagentId)}/tool-calls/${encodeURIComponent(toolCallId)}/cancel`,
      { method: 'POST' },
    ).then(() => undefined);
  },

  getPermissions(sessionId: string): Promise<PermissionState> {
    return jsonRequest<PermissionState>(`api/sessions/${encodeURIComponent(sessionId)}/permissions`, {
      headers: {},
    });
  },

  resolvePermission(
    sessionId: string,
    pending: PendingPermissionInfo,
    decision: PermissionDecisionPayload,
  ): Promise<void> {
    const key: PermissionKey = {
      tool: pending.tool,
      key: pending.key,
      value: pending.value,
    };

    return request(`api/sessions/${encodeURIComponent(sessionId)}/permissions/resolve`, {
      method: 'POST',
      body: jsonBody({ key, decision, request_id: pending.request_id }),
    }).then(() => undefined);
  },
};
