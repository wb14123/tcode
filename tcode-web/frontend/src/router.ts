import { runtimeConfig } from './config';
import type { AppRoute } from './types';

function stripRouterBase(pathname: string): string {
  const base = runtimeConfig.routerBase;
  if (base !== '/' && pathname.startsWith(base)) {
    return pathname.slice(base.length - 1) || '/';
  }

  return pathname || '/';
}

function joinRoute(path: string): string {
  const clean = path.replace(/^\/+/, '');
  if (!clean) {
    return runtimeConfig.routerBase;
  }

  return `${runtimeConfig.routerBase}${clean}`.replace(/\/+/g, '/');
}

export function hrefForRoute(route: AppRoute): string {
  switch (route.kind) {
    case 'login':
      return joinRoute('login');
    case 'home':
      return joinRoute('');
    case 'session':
      return joinRoute(`sessions/${encodeURIComponent(route.sessionId)}`);
    case 'tool':
      return joinRoute(
        `sessions/${encodeURIComponent(route.sessionId)}/tool-calls/${encodeURIComponent(route.toolCallId)}`,
      );
    case 'subagent':
      return joinRoute(
        `sessions/${encodeURIComponent(route.sessionId)}/subagents/${encodeURIComponent(route.subagentId)}`,
      );
    case 'subagent-tool':
      return joinRoute(
        `sessions/${encodeURIComponent(route.sessionId)}/subagents/${encodeURIComponent(route.subagentId)}/tool-calls/${encodeURIComponent(route.toolCallId)}`,
      );
  }
}

export function parseRoute(pathname = window.location.pathname): AppRoute {
  const relativePath = stripRouterBase(pathname);
  const segments = relativePath.split('/').filter(Boolean).map(decodeURIComponent);

  if (segments.length === 0) {
    return { kind: 'home' };
  }

  if (segments.length === 1 && segments[0] === 'login') {
    return { kind: 'login' };
  }

  if (segments[0] === 'sessions' && segments[1]) {
    const sessionId = segments[1];

    if (segments.length === 2) {
      return { kind: 'session', sessionId };
    }

    if (segments[2] === 'tool-calls' && segments[3] && segments.length === 4) {
      return { kind: 'tool', sessionId, toolCallId: segments[3] };
    }

    if (segments[2] === 'subagents' && segments[3]) {
      const subagentId = segments[3];

      if (segments.length === 4) {
        return { kind: 'subagent', sessionId, subagentId };
      }

      if (segments[4] === 'tool-calls' && segments[5] && segments.length === 6) {
        return { kind: 'subagent-tool', sessionId, subagentId, toolCallId: segments[5] };
      }
    }
  }

  return { kind: 'home' };
}

export function navigate(route: AppRoute, replace = false): void {
  const href = hrefForRoute(route);
  if (replace) {
    window.history.replaceState({}, '', href);
  } else {
    window.history.pushState({}, '', href);
  }
  window.dispatchEvent(new PopStateEvent('popstate'));
}

export function activeSessionId(route: AppRoute): string | null {
  switch (route.kind) {
    case 'session':
    case 'tool':
    case 'subagent':
    case 'subagent-tool':
      return route.sessionId;
    default:
      return null;
  }
}
