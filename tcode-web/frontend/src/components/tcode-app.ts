import { LitElement, html, nothing } from 'lit';

import { ApiError, api } from '../api';
import { runtimeConfig } from '../config';
import { activeSessionId, hrefForRoute, navigate, parseRoute } from '../router';
import type { AppRoute, PendingPermissionInfo, PermissionDecisionPayload, PermissionState, SessionSummary } from '../types';

import './session-view';
import './subagent-view';
import './tool-call-view';

class TcodeApp extends LitElement {
  private authState: 'loading' | 'authenticated' | 'unauthenticated' = 'loading';
  private route: AppRoute = parseRoute();
  private sessions: SessionSummary[] = [];
  private sessionsError = '';
  private loginSecret = '';
  private loginError = '';
  private loginBusy = false;
  private sessionsPollHandle: number | null = null;
  private permissionsPollHandle: number | null = null;
  private permissionState: PermissionState | null = null;
  private permissionsError = '';
  private newSessionOpen = false;
  private newSessionPrompt = '';
  private newSessionBusy = false;
  private newSessionError = '';
  private denyReason = '';
  private resolvingPermission = false;
  private sidebarOpen = false;

  createRenderRoot(): this {
    return this;
  }

  connectedCallback(): void {
    super.connectedCallback();
    window.addEventListener('popstate', this.handleRouteChange);
    window.addEventListener('tcode-auth-required', this.handleAuthRequired as EventListener);
    this.bootstrap();
  }

  disconnectedCallback(): void {
    super.disconnectedCallback();
    window.removeEventListener('popstate', this.handleRouteChange);
    window.removeEventListener('tcode-auth-required', this.handleAuthRequired as EventListener);
    this.stopSessionsPolling();
    this.stopPermissionsPolling();
  }

  private handleRouteChange = (): void => {
    this.route = parseRoute();
    this.sidebarOpen = false;
    this.resetPermissionPolling();
    this.requestUpdate();
  };

  private handleAuthRequired = (): void => {
    this.authState = 'unauthenticated';
    this.sessions = [];
    this.permissionState = null;
    this.permissionsError = '';
    this.stopSessionsPolling();
    this.stopPermissionsPolling();
    if (this.route.kind !== 'login') {
      navigate({ kind: 'login' }, true);
      this.route = { kind: 'login' };
    }
    this.requestUpdate();
  };

  private async bootstrap(): Promise<void> {
    this.authState = 'loading';
    this.requestUpdate();

    try {
      const session = await api.getAuthSession();
      if (session.authenticated) {
        this.authState = 'authenticated';
        if (this.route.kind === 'login') {
          navigate({ kind: 'home' }, true);
          this.route = { kind: 'home' };
        }
        await this.refreshSessions();
        this.startSessionsPolling();
        this.resetPermissionPolling();
      } else {
        this.authState = 'unauthenticated';
        if (this.route.kind !== 'login') {
          navigate({ kind: 'login' }, true);
          this.route = { kind: 'login' };
        }
      }
    } catch (error) {
      this.authState = 'unauthenticated';
      this.loginError = error instanceof Error ? error.message : 'Failed to probe auth session';
      if (this.route.kind !== 'login') {
        navigate({ kind: 'login' }, true);
        this.route = { kind: 'login' };
      }
    }

    this.requestUpdate();
  }

  private startSessionsPolling(): void {
    this.stopSessionsPolling();
    this.sessionsPollHandle = window.setInterval(() => {
      void this.refreshSessions();
    }, 5000);
  }

  private stopSessionsPolling(): void {
    if (this.sessionsPollHandle !== null) {
      window.clearInterval(this.sessionsPollHandle);
      this.sessionsPollHandle = null;
    }
  }

  private stopPermissionsPolling(): void {
    if (this.permissionsPollHandle !== null) {
      window.clearInterval(this.permissionsPollHandle);
      this.permissionsPollHandle = null;
    }
  }

  private resetPermissionPolling(): void {
    this.stopPermissionsPolling();
    if (this.authState !== 'authenticated') {
      this.permissionState = null;
      this.permissionsError = '';
      this.requestUpdate();
      return;
    }

    const sessionId = activeSessionId(this.route);
    if (!sessionId) {
      this.permissionState = null;
      this.permissionsError = '';
      this.requestUpdate();
      return;
    }

    void this.refreshPermissions();
    this.permissionsPollHandle = window.setInterval(() => {
      void this.refreshPermissions();
    }, 2500);
  }

  private async refreshSessions(): Promise<void> {
    try {
      const response = await api.listSessions();
      this.sessions = response.sessions;
      this.sessionsError = '';
    } catch (error) {
      this.sessionsError = error instanceof Error ? error.message : 'Failed to load sessions';
    }
    this.requestUpdate();
  }

  private async refreshPermissions(): Promise<void> {
    const sessionId = activeSessionId(this.route);
    if (!sessionId) {
      this.permissionState = null;
      this.permissionsError = '';
      this.requestUpdate();
      return;
    }

    try {
      this.permissionState = await api.getPermissions(sessionId);
      this.permissionsError = '';
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) {
        this.permissionState = null;
        this.permissionsError = '';
      } else {
        this.permissionsError = error instanceof Error ? error.message : 'Failed to load permissions';
      }
    }
    this.requestUpdate();
  }

  private handleShellClick = (event: Event): void => {
    const mouseEvent = event as MouseEvent;
    if (mouseEvent.defaultPrevented || mouseEvent.button !== 0 || mouseEvent.metaKey || mouseEvent.ctrlKey || mouseEvent.shiftKey || mouseEvent.altKey) {
      return;
    }

    const path = mouseEvent.composedPath();
    const anchor = path.find((entry): entry is HTMLAnchorElement => entry instanceof HTMLAnchorElement);
    if (!anchor || anchor.target || anchor.hasAttribute('download')) {
      return;
    }

    const url = new URL(anchor.href, window.location.href);
    if (url.origin !== window.location.origin) {
      return;
    }

    if (!url.pathname.startsWith(runtimeConfig.routerBase) && runtimeConfig.routerBase !== '/') {
      return;
    }

    mouseEvent.preventDefault();
    window.history.pushState({}, '', url.pathname + url.search + url.hash);
    window.dispatchEvent(new PopStateEvent('popstate'));
  };

  private openSidebar = (): void => {
    this.sidebarOpen = true;
    this.requestUpdate();
  };

  private closeSidebar = (): void => {
    this.sidebarOpen = false;
    this.requestUpdate();
  };

  private toggleSidebar = (): void => {
    this.sidebarOpen = !this.sidebarOpen;
    this.requestUpdate();
  };

  private onLoginSecretInput = (event: Event): void => {
    this.loginSecret = (event.target as HTMLInputElement).value;
    this.requestUpdate();
  };

  private onNewSessionPromptInput = (event: Event): void => {
    this.newSessionPrompt = (event.target as HTMLTextAreaElement).value;
    this.requestUpdate();
  };

  private onDenyReasonInput = (event: Event): void => {
    this.denyReason = (event.target as HTMLTextAreaElement).value;
    this.requestUpdate();
  };

  private async submitLogin(event: Event): Promise<void> {
    event.preventDefault();
    if (!this.loginSecret.trim() || this.loginBusy) {
      return;
    }

    this.loginBusy = true;
    this.loginError = '';
    this.requestUpdate();

    try {
      const status = await api.login(this.loginSecret);
      if (!status.authenticated) {
        this.loginError = 'Login failed.';
        return;
      }

      this.authState = 'authenticated';
      this.loginSecret = '';
      await this.refreshSessions();
      this.startSessionsPolling();
      navigate({ kind: 'home' }, true);
      this.route = { kind: 'home' };
      this.resetPermissionPolling();
    } catch (error) {
      this.loginError = error instanceof Error ? error.message : 'Login failed';
    } finally {
      this.loginBusy = false;
      this.requestUpdate();
    }
  }

  private openNewSessionModal = (): void => {
    this.sidebarOpen = false;
    this.newSessionOpen = true;
    this.newSessionError = '';
    this.requestUpdate();
  };

  private closeNewSessionModal = (): void => {
    if (this.newSessionBusy) {
      return;
    }

    this.newSessionOpen = false;
    this.newSessionPrompt = '';
    this.newSessionError = '';
    this.requestUpdate();
  };

  private async createSession(event: Event): Promise<void> {
    event.preventDefault();
    const prompt = this.newSessionPrompt.trim();
    if (!prompt || this.newSessionBusy) {
      return;
    }

    this.newSessionBusy = true;
    this.newSessionError = '';
    this.requestUpdate();

    try {
      const created = await api.createSession(prompt);
      this.newSessionOpen = false;
      this.newSessionPrompt = '';
      await this.refreshSessions();
      navigate({ kind: 'session', sessionId: created.id }, false);
      this.route = { kind: 'session', sessionId: created.id };
      this.resetPermissionPolling();
    } catch (error) {
      this.newSessionError = error instanceof Error ? error.message : 'Failed to create session';
    } finally {
      this.newSessionBusy = false;
      this.requestUpdate();
    }
  }

  private async resolvePermission(decision: PermissionDecisionPayload): Promise<void> {
    const sessionId = activeSessionId(this.route);
    const pending = this.pendingPermission();
    if (!sessionId || !pending || this.resolvingPermission) {
      return;
    }

    this.resolvingPermission = true;
    this.permissionsError = '';
    this.requestUpdate();

    try {
      await api.resolvePermission(sessionId, pending, decision);
      this.denyReason = '';
      await this.refreshPermissions();
    } catch (error) {
      this.permissionsError = error instanceof Error ? error.message : 'Failed to resolve permission';
    } finally {
      this.resolvingPermission = false;
      this.requestUpdate();
    }
  }

  private pendingPermission(): PendingPermissionInfo | null {
    return this.permissionState?.pending?.[0] ?? null;
  }

  private renderLogin() {
    return html`
      <div class="login-shell" @click=${this.handleShellClick}>
        <form class="login-card" @submit=${this.submitLogin}>
          <h1 class="page-title">TCode</h1>
          <p class="page-subtitle">
            Same-origin cookie auth for the remote tcode session server. API and router bases are
            centralized so later hosting changes stay localized, though cross-origin/static hosting
            would still need backend support.
          </p>
          ${this.loginError ? html`<div class="inline-alert error">${this.loginError}</div>` : nothing}
          <label>
            <span class="muted">Shared secret</span>
            <input type="password" .value=${this.loginSecret} @input=${this.onLoginSecretInput} />
          </label>
          <div class="modal-actions">
            <button class="button" type="submit" ?disabled=${this.loginBusy || !this.loginSecret.trim()}>
              ${this.loginBusy ? 'Logging in…' : 'Log in'}
            </button>
          </div>
        </form>
      </div>
    `;
  }

  private renderHome() {
    return html`
      <section class="page">
        <div class="empty-state">
          <div class="empty-copy">
            <h1 class="page-title">No conversation selected</h1>
            <p>Select a conversation from the sidebar or start a new one to get going.</p>
          </div>
        </div>
      </section>
    `;
  }

  private renderMainView() {
    switch (this.route.kind) {
      case 'home':
        return this.renderHome();
      case 'session':
        return html`
          <tcode-session-view
            .sessionId=${this.route.sessionId}
            @sessions-refresh-requested=${() => {
              void this.refreshSessions();
            }}
            @permissions-refresh-requested=${() => {
              void this.refreshPermissions();
            }}
          ></tcode-session-view>
        `;
      case 'subagent':
        return html`
          <tcode-subagent-view
            .sessionId=${this.route.sessionId}
            .subagentId=${this.route.subagentId}
            @permissions-refresh-requested=${() => {
              void this.refreshPermissions();
            }}
          ></tcode-subagent-view>
        `;
      case 'tool':
        return html`
          <tcode-tool-call-view
            .sessionId=${this.route.sessionId}
            .toolCallId=${this.route.toolCallId}
          ></tcode-tool-call-view>
        `;
      case 'subagent-tool':
        return html`
          <tcode-tool-call-view
            .sessionId=${this.route.sessionId}
            .subagentId=${this.route.subagentId}
            .toolCallId=${this.route.toolCallId}
          ></tcode-tool-call-view>
        `;
      case 'login':
        return this.renderLogin();
    }
  }

  private renderSidebar() {
    const currentSession = activeSessionId(this.route);

    return html`
      <aside class="sidebar">
        <section class="sidebar-header">
          <div class="brand">
            <a class="brand-title" href="${hrefForRoute({ kind: 'home' })}" @click=${this.closeSidebar}>TCode</a>
          </div>
          <div class="sidebar-actions">
            <button class="button" @click=${this.openNewSessionModal}>New conversation</button>
          </div>
          ${this.sessionsError ? html`<div class="inline-alert error">${this.sessionsError}</div>` : nothing}
        </section>

        <section class="session-list">
          ${this.sessions.length
            ? this.sessions.map(
                (session) => html`
                  <a
                    class="session-link ${currentSession === session.id ? 'active' : ''}"
                    href="${hrefForRoute({ kind: 'session', sessionId: session.id })}"
                    @click=${this.closeSidebar}
                  >
                    <div class="session-link-title">${session.description || 'Untitled conversation'}</div>
                  </a>
                `,
              )
            : html`<div class="empty-copy">No sessions yet. Start a new conversation to populate the sidebar.</div>`}
        </section>
      </aside>
    `;
  }

  private renderMobileTopbar() {
    return html`
      <header class="mobile-topbar">
        <button class="button ghost mobile-topbar-button" type="button" @click=${this.toggleSidebar} aria-label="Open conversations">
          ☰
        </button>
        <button class="button ghost mobile-topbar-button" type="button" @click=${this.openNewSessionModal}>New</button>
      </header>
    `;
  }

  private renderPermissionModal() {
    const pending = this.pendingPermission();
    if (!pending || this.authState !== 'authenticated') {
      return nothing;
    }

    return html`
      <div class="modal-backdrop">
        <section class="modal-card">
          <div>
            <h2 class="page-title">Permission approval required</h2>
            <p class="page-subtitle">
              Request ${pending.request_id} for tool <code>${pending.tool}</code>. This PoC only exposes
              approval metadata here; diff/file previews are intentionally out of scope.
            </p>
          </div>
          ${this.permissionsError ? html`<div class="inline-alert error">${this.permissionsError}</div>` : nothing}
          <dl class="meta-list">
            <div>
              <dt>Prompt</dt>
              <dd>${pending.prompt}</dd>
            </div>
            <div>
              <dt>Key</dt>
              <dd>${pending.key}</dd>
            </div>
            <div>
              <dt>Value</dt>
              <dd><code>${pending.value}</code></dd>
            </div>
            <div>
              <dt>Queued requests</dt>
              <dd>${this.permissionState?.pending.length ?? 0}</dd>
            </div>
            <div>
              <dt>Once only</dt>
              <dd>${pending.once_only ? 'yes' : 'no'}</dd>
            </div>
          </dl>
          <label>
            <span class="muted">Optional deny reason</span>
            <textarea
              placeholder="Only used when denying this request"
              .value=${this.denyReason}
              @input=${this.onDenyReasonInput}
            ></textarea>
          </label>
          <div class="modal-actions">
            <button class="button success" @click=${() => void this.resolvePermission('AllowOnce')} ?disabled=${this.resolvingPermission}>
              Allow once
            </button>
            ${pending.once_only
              ? nothing
              : html`
                  <button class="button" @click=${() => void this.resolvePermission('AllowSession')} ?disabled=${this.resolvingPermission}>
                    Allow for session
                  </button>
                  <button class="button secondary" @click=${() => void this.resolvePermission('AllowProject')} ?disabled=${this.resolvingPermission}>
                    Allow for project
                  </button>
                `}
            <button
              class="button danger"
              @click=${() =>
                void this.resolvePermission({
                  Deny: { reason: this.denyReason.trim() || null },
                })}
              ?disabled=${this.resolvingPermission}
            >
              Deny
            </button>
          </div>
        </section>
      </div>
    `;
  }

  private renderNewSessionModal() {
    if (!this.newSessionOpen) {
      return nothing;
    }

    return html`
      <div class="modal-backdrop" @click=${this.closeNewSessionModal}>
        <form class="modal-card" @submit=${this.createSession} @click=${(event: Event) => event.stopPropagation()}>
          <div>
            <h2 class="page-title">New conversation</h2>
            <p class="page-subtitle">
              This action creates the session and immediately sends your first prompt to start the run.
            </p>
          </div>
          ${this.newSessionError ? html`<div class="inline-alert error">${this.newSessionError}</div>` : nothing}
          <textarea
            placeholder="Example: Review the repo and propose a cleanup plan."
            .value=${this.newSessionPrompt}
            @input=${this.onNewSessionPromptInput}
          ></textarea>
          <div class="modal-actions">
            <button class="button" type="submit" ?disabled=${this.newSessionBusy || !this.newSessionPrompt.trim()}>
              ${this.newSessionBusy ? 'Creating…' : 'Create session'}
            </button>
            <button class="button secondary" type="button" @click=${this.closeNewSessionModal} ?disabled=${this.newSessionBusy}>
              Close
            </button>
          </div>
        </form>
      </div>
    `;
  }

  render() {
    if (this.authState === 'loading') {
      return html`
        <div class="login-shell">
          <div class="login-card">
            <h1 class="page-title">TCode</h1>
            <p class="page-subtitle">Checking authentication session…</p>
          </div>
        </div>
      `;
    }

    if (this.authState === 'unauthenticated') {
      return this.renderLogin();
    }

    return html`
      <div class="app-shell ${this.sidebarOpen ? 'sidebar-open' : ''}" @click=${this.handleShellClick}>
        <div class="sidebar-backdrop" @click=${this.closeSidebar}></div>
        ${this.renderSidebar()}
        <main class="main-column">
          ${this.renderMobileTopbar()}
          ${this.renderMainView()}
        </main>
      </div>
      ${this.renderNewSessionModal()} ${this.renderPermissionModal()}
    `;
  }
}

customElements.define('tcode-app', TcodeApp);
