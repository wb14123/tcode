import { runtimeConfig } from './config';
import type {
  AuthSessionStatus,
  CreateSessionResponse,
  PendingPermissionInfo,
  PermissionDecisionPayload,
  PermissionKey,
  PermissionState,
  RegisterLeaseRequest,
  RuntimeInfoResponse,
  SessionRuntimeInfo,
  SessionsResponse,
  LeaseResponse,
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

export interface LeaseSnapshot {
  sessionId: string | null;
  runtimeInfo: SessionRuntimeInfo | null;
  disconnected: boolean;
  reconnecting: boolean;
  errorMessage: string;
}

type LeaseSubscriber = (snapshot: LeaseSnapshot) => void;

class SessionLeaseManager {
  private sessionId: string | null = null;
  private clientId: string | null = null;
  private runtimeInfo: SessionRuntimeInfo | null = null;
  private disconnected = false;
  private reconnecting = false;
  private errorMessage = '';
  private subscribers = new Set<LeaseSubscriber>();
  private releaseTimer: number | null = null;
  private heartbeatTimer: number | null = null;
  private heartbeatInFlight: { sessionId: string; clientId: string } | null = null;
  private startInFlight = false;
  private heartbeatIntervalSeconds = 15;
  private requestToken = 0;

  attach(sessionId: string, subscriber: LeaseSubscriber): () => void {
    if (this.releaseTimer !== null) {
      window.clearTimeout(this.releaseTimer);
      this.releaseTimer = null;
    }
    if (this.sessionId && this.sessionId !== sessionId) {
      this.detachNow();
    }
    this.sessionId = sessionId;
    this.subscribers.add(subscriber);
    subscriber(this.snapshot());
    if (!this.clientId && !this.startInFlight) {
      void this.startLease(false);
    }

    let released = false;
    return () => {
      if (released) {
        return;
      }
      released = true;
      this.subscribers.delete(subscriber);
      if (this.subscribers.size === 0) {
        this.releaseTimer = window.setTimeout(() => {
          this.releaseTimer = null;
          if (this.subscribers.size === 0) {
            this.detachNow();
          }
        }, 250);
      }
    };
  }

  async reconnect(sessionId: string): Promise<void> {
    if (this.sessionId && this.sessionId !== sessionId) {
      this.detachNow();
    }
    this.sessionId = sessionId;
    await this.startLease(true);
  }

  private async startLease(resume: boolean): Promise<void> {
    const sessionId = this.sessionId;
    if (!sessionId) {
      return;
    }
    if (this.startInFlight) {
      return;
    }
    if (this.clientId && !resume) {
      return;
    }

    const requestToken = ++this.requestToken;
    const previousClientId = this.clientId;
    const previousSessionId = this.sessionId;
    this.startInFlight = true;
    this.reconnecting = resume;
    this.stopHeartbeat();
    this.clientId = null;
    this.notify();
    if (previousClientId && previousSessionId) {
      void api.detachSessionLease(previousSessionId, previousClientId).catch(() => undefined);
    }

    let autoResumeAfterInactiveAttach = false;

    try {
      const response = await api.registerSessionLease(sessionId, {
        client_label: 'web-ui',
        resume,
      });
      if (requestToken !== this.requestToken || sessionId !== this.sessionId) {
        if (response.client_id) {
          void api.detachSessionLease(sessionId, response.client_id).catch(() => undefined);
        }
        return;
      }

      this.runtimeInfo = response.runtime_info;
      if (!response.active || !response.client_id) {
        autoResumeAfterInactiveAttach =
          !resume &&
          !response.active &&
          !response.client_id &&
          response.runtime_info.active_lease_count === 0;
        this.clientId = null;
        this.disconnected = true;
        this.reconnecting = autoResumeAfterInactiveAttach;
        this.errorMessage = resume ? 'Session runtime is not active.' : '';
        this.notify();
        return;
      }

      this.clientId = response.client_id;
      this.disconnected = false;
      this.errorMessage = '';
      this.heartbeatIntervalSeconds = Math.max(5, response.heartbeat_interval_seconds || 15);
      this.scheduleHeartbeat();
      this.notify();
    } catch (error) {
      if (requestToken !== this.requestToken || sessionId !== this.sessionId) {
        return;
      }
      this.runtimeInfo = null;
      this.clientId = null;
      this.disconnected = true;
      this.errorMessage = error instanceof Error ? error.message : 'Failed to connect session runtime';
      this.notify();
    } finally {
      if (requestToken === this.requestToken) {
        this.startInFlight = false;
        if (autoResumeAfterInactiveAttach && sessionId === this.sessionId && this.subscribers.size > 0) {
          void this.startLease(true);
          return;
        }
        this.reconnecting = false;
        this.notify();
      }
    }
  }

  private scheduleHeartbeat(): void {
    this.stopHeartbeat();
    this.heartbeatTimer = window.setTimeout(() => {
      this.heartbeatTimer = null;
      void this.sendHeartbeat();
    }, this.heartbeatIntervalSeconds * 1000);
  }

  private stopHeartbeat(): void {
    if (this.heartbeatTimer !== null) {
      window.clearTimeout(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  private async sendHeartbeat(): Promise<void> {
    const sessionId = this.sessionId;
    const clientId = this.clientId;
    if (!sessionId || !clientId) {
      return;
    }

    const inFlight = this.heartbeatInFlight;
    if (inFlight?.sessionId === sessionId && inFlight.clientId === clientId) {
      return;
    }

    const heartbeatLease = { sessionId, clientId };
    this.heartbeatInFlight = heartbeatLease;
    try {
      const response = await api.heartbeatSessionLease(sessionId, clientId);
      if (sessionId !== this.sessionId || clientId !== this.clientId) {
        return;
      }
      this.runtimeInfo = response.runtime_info;
      if (!response.runtime_info.active) {
        this.markDisconnected('Session runtime disconnected.', sessionId, clientId);
        return;
      }
      this.disconnected = false;
      this.errorMessage = '';
      this.notify();
      this.scheduleHeartbeat();
    } catch (error) {
      if (sessionId === this.sessionId && clientId === this.clientId) {
        this.markDisconnected(error instanceof Error ? error.message : 'Session runtime disconnected.', sessionId, clientId);
      }
    } finally {
      if (this.heartbeatInFlight === heartbeatLease) {
        this.heartbeatInFlight = null;
      }
    }
  }

  private markDisconnected(message: string, sessionId: string, clientId: string): void {
    this.stopHeartbeat();
    this.clientId = null;
    this.disconnected = true;
    this.errorMessage = message;
    void api.detachSessionLease(sessionId, clientId).catch(() => undefined);
    this.notify();
  }

  private detachNow(): void {
    this.requestToken += 1;
    this.stopHeartbeat();
    const sessionId = this.sessionId;
    const clientId = this.clientId;
    this.sessionId = null;
    this.clientId = null;
    this.runtimeInfo = null;
    this.disconnected = false;
    this.reconnecting = false;
    this.errorMessage = '';
    this.startInFlight = false;
    this.subscribers.clear();
    if (clientId && sessionId) {
      void api.detachSessionLease(sessionId, clientId).catch(() => undefined);
    }
    this.notify();
  }

  private snapshot(): LeaseSnapshot {
    return {
      sessionId: this.sessionId,
      runtimeInfo: this.runtimeInfo,
      disconnected: this.disconnected,
      reconnecting: this.reconnecting,
      errorMessage: this.errorMessage,
    };
  }

  private notify(): void {
    const snapshot = this.snapshot();
    for (const subscriber of this.subscribers) {
      subscriber(snapshot);
    }
  }
}

export const sessionLeaseManager = new SessionLeaseManager();
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

  registerSessionLease(sessionId: string, body: RegisterLeaseRequest = {}): Promise<LeaseResponse> {
    return jsonRequest<LeaseResponse>(`api/sessions/${encodeURIComponent(sessionId)}/leases`, {
      method: 'POST',
      body: jsonBody(body),
    });
  },

  heartbeatSessionLease(sessionId: string, clientId: string): Promise<RuntimeInfoResponse> {
    return jsonRequest<RuntimeInfoResponse>(
      `api/sessions/${encodeURIComponent(sessionId)}/leases/${encodeURIComponent(clientId)}/heartbeat`,
      { method: 'POST' },
    );
  },

  detachSessionLease(sessionId: string, clientId: string): Promise<void> {
    return request(`api/sessions/${encodeURIComponent(sessionId)}/leases/${encodeURIComponent(clientId)}`, {
      method: 'DELETE',
      keepalive: true,
    }).then(() => undefined);
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
