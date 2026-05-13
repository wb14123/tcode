import { LitElement, html, nothing } from 'lit'

import { ApiError, api, openEventStream } from '../api'
import { parseStreamLine, rawVariant } from '../messages'
import type { PendingPermissionInfo, PermissionKey, PermissionState } from '../types'
import { ALL_SCOPES } from '../types'

import './add-permission-form'

interface TreeValue {
  value: string
  status: 'pending' | 'session' | 'project'
  isWildcard: boolean
  prompt?: string
  requestId?: string
  onceOnly?: boolean
}

interface TreeKey {
  key: string
  values: TreeValue[]
}

interface TreeTool {
  tool: string
  keys: TreeKey[]
  pendingCount: number
}

const PERMISSION_VARIANTS = new Set([
  'PermissionUpdated',
  'ToolRequestPermission',
  'ToolPermissionApproved',
  'SubAgentWaitingPermission',
  'SubAgentPermissionApproved',
  'SubAgentPermissionDenied',
])

function statusSortOrder(status: string): number {
  switch (status) {
    case 'pending':
      return 0
    case 'session':
      return 1
    case 'project':
      return 2
    default:
      return 3
  }
}

class TcodePermissionTree extends LitElement {
  static properties = {
    sessionId: { type: String },
    permissionState: { type: Object },
    error: { type: String },
  }

  sessionId = ''

  private permissionState: PermissionState | null = null
  private error = ''
  private collapsedTools = new Set<string>()
  private collapsedKeys = new Map<string, Set<string>>()
  private filterMode: 'all' | 'pending' = 'all'
  private eventSource: EventSource | null = null
  private loading = false
  private revokingValues = new Set<string>()
  private showAddModal = false
  private selectedPending: PendingPermissionInfo | null = null
  private denyReason = ''
  private resolvingPermission = false

  createRenderRoot(): this {
    return this
  }

  disconnectedCallback(): void {
    super.disconnectedCallback()
    this.closeSseStream()
  }

  updated(changed: Map<string, unknown>): void {
    if (changed.has('sessionId')) {
      this.closeSseStream()
      this.permissionState = null
      this.error = ''
      this.collapsedTools.clear()
      this.collapsedKeys.clear()
      this.selectedPending = null
      this.showAddModal = false
      this.denyReason = ''
      this.resolvingPermission = false
      void this.refreshPermissions(true)
      this.openSseStream()
    }
  }

  private openSseStream(): void {
    if (this.eventSource || !this.sessionId) {
      return
    }

    const source = openEventStream(`api/sessions/${encodeURIComponent(this.sessionId)}/display.jsonl`)
    this.eventSource = source

    source.onmessage = (message) => {
      if (this.eventSource !== source) {
        return
      }

      const raw = message.data
      if (typeof raw !== 'string') {
        return
      }

      const parsed = parseStreamLine(raw)
      if (!parsed) {
        return
      }

      const variant = rawVariant(parsed)
      if (variant && PERMISSION_VARIANTS.has(variant)) {
        void this.refreshPermissions(false)
      }
    }

    source.onerror = () => {
      if (this.eventSource === source && source.readyState === EventSource.CLOSED) {
        // Permanent close (e.g., 404) — browser won't retry
        this.eventSource = null
      }
    }
  }

  private closeSseStream(): void {
    this.eventSource?.close()
    this.eventSource = null
  }

  private async refreshPermissions(showLoading: boolean): Promise<void> {
    if (!this.sessionId) {
      this.permissionState = null
      this.error = ''
      this.requestUpdate()
      return
    }

    if (showLoading) {
      this.loading = true
      this.requestUpdate()
    }

    try {
      this.permissionState = await api.getPermissions(this.sessionId)
      this.error = ''
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        this.permissionState = null
        this.error = ''
      } else {
        const message = err instanceof Error ? err.message : 'Failed to load permissions'
        this.error = message
      }
    } finally {
      if (showLoading) {
        this.loading = false
        this.requestUpdate()
      }
    }

    this.requestUpdate()
  }

  private buildTree(): TreeTool[] {
    const state = this.permissionState
    const isPendingFilter = this.filterMode === 'pending'

    // Collect all tool+key pairs from ALL_SCOPES and permission state
    const toolKeys = new Map<string, Set<string>>()
    for (const [tool, keys] of Object.entries(ALL_SCOPES)) {
      const keySet = toolKeys.get(tool) ?? new Set<string>()
      for (const key of keys) {
        keySet.add(key)
      }
      toolKeys.set(tool, keySet)
    }

    // Defensive: add tools/keys from permission state not in ALL_SCOPES
    for (const perm of state?.pending ?? []) {
      const keySet = toolKeys.get(perm.tool) ?? new Set<string>()
      keySet.add(perm.key)
      toolKeys.set(perm.tool, keySet)
    }
    for (const perm of state?.session ?? []) {
      const keySet = toolKeys.get(perm.tool) ?? new Set<string>()
      keySet.add(perm.key)
      toolKeys.set(perm.tool, keySet)
    }
    for (const perm of state?.project ?? []) {
      const keySet = toolKeys.get(perm.tool) ?? new Set<string>()
      keySet.add(perm.key)
      toolKeys.set(perm.tool, keySet)
    }

    // Build value maps: tool -> key -> TreeValue[]
    const pendingMap = new Map<string, Map<string, TreeValue[]>>()
    for (const p of state?.pending ?? []) {
      const keyMap = pendingMap.get(p.tool) ?? new Map<string, TreeValue[]>()
      const values = keyMap.get(p.key) ?? []
      values.push({
        value: p.value,
        status: 'pending',
        isWildcard: p.value === '*',
        prompt: p.prompt,
        requestId: p.request_id,
        onceOnly: p.once_only,
      })
      keyMap.set(p.key, values)
      pendingMap.set(p.tool, keyMap)
    }

    const sessionMap = new Map<string, Map<string, TreeValue[]>>()
    for (const s of state?.session ?? []) {
      const keyMap = sessionMap.get(s.tool) ?? new Map<string, TreeValue[]>()
      const values = keyMap.get(s.key) ?? []
      values.push({
        value: s.value,
        status: 'session',
        isWildcard: s.value === '*',
      })
      keyMap.set(s.key, values)
      sessionMap.set(s.tool, keyMap)
    }

    const projectMap = new Map<string, Map<string, TreeValue[]>>()
    for (const p of state?.project ?? []) {
      const keyMap = projectMap.get(p.tool) ?? new Map<string, TreeValue[]>()
      const values = keyMap.get(p.key) ?? []
      values.push({
        value: p.value,
        status: 'project',
        isWildcard: p.value === '*',
      })
      keyMap.set(p.key, values)
      projectMap.set(p.tool, keyMap)
    }

    // Build the tree
    const pendingValueSet = new Set<string>()
    for (const p of state?.pending ?? []) {
      pendingValueSet.add(`${p.tool}|${p.key}|${p.value}`)
    }

    const tools: TreeTool[] = []

    for (const [tool, keys] of toolKeys) {
      const treeKeys: TreeKey[] = []
      let toolPendingCount = 0

      for (const key of keys) {
        const pendingValues = pendingMap.get(tool)?.get(key) ?? []
        const sessionValues = sessionMap.get(tool)?.get(key) ?? []
        const projectValues = projectMap.get(tool)?.get(key) ?? []

        // Deduplicate: skip session/project values that also appear as pending
        const sessionFiltered = sessionValues.filter(
          (sv) => !pendingValueSet.has(`${tool}|${key}|${sv.value}`),
        )
        const projectFiltered = projectValues.filter(
          (pv) => !pendingValueSet.has(`${tool}|${key}|${pv.value}`) &&
            !sessionValues.some((sv) => sv.value === pv.value),
        )

        if (isPendingFilter && pendingValues.length === 0) {
          continue
        }

        let allValues: TreeValue[]
        if (isPendingFilter) {
          allValues = [...pendingValues]
        } else {
          allValues = [...pendingValues, ...sessionFiltered, ...projectFiltered]
        }

        // Sort values: wildcard first, then pending, session, project, alphabetical
        allValues.sort((a, b) => {
          if (a.isWildcard !== b.isWildcard) {
            return a.isWildcard ? -1 : 1
          }
          const statusOrder = statusSortOrder(a.status) - statusSortOrder(b.status)
          if (statusOrder !== 0) {
            return statusOrder
          }
          return a.value.localeCompare(b.value)
        })

        toolPendingCount += pendingValues.length

        treeKeys.push({
          key,
          values: allValues,
        })
      }

      // Sort keys alphabetically
      treeKeys.sort((a, b) => a.key.localeCompare(b.key))

      if (isPendingFilter && toolPendingCount === 0) {
        continue
      }

      tools.push({
        tool,
        keys: treeKeys,
        pendingCount: toolPendingCount,
      })
    }

    // Sort tools: pending first (desc by count), then alphabetical
    tools.sort((a, b) => {
      const aHasPending = a.pendingCount > 0
      const bHasPending = b.pendingCount > 0
      if (aHasPending !== bHasPending) {
        return aHasPending ? -1 : 1
      }
      if (aHasPending && bHasPending) {
        return b.pendingCount - a.pendingCount
      }
      return a.tool.localeCompare(b.tool)
    })

    return tools
  }

  private toggleTool(tool: string): void {
    if (this.collapsedTools.has(tool)) {
      this.collapsedTools.delete(tool)
    } else {
      this.collapsedTools.add(tool)
    }
    this.requestUpdate()
  }

  private toggleKey(tool: string, key: string): void {
    const keySet = this.collapsedKeys.get(tool)
    if (keySet?.has(key)) {
      keySet.delete(key)
      if (keySet.size === 0) {
        this.collapsedKeys.delete(tool)
      }
    } else {
      if (!keySet) {
        this.collapsedKeys.set(tool, new Set([key]))
      } else {
        keySet.add(key)
      }
    }
    this.requestUpdate()
  }

  private toggleFilter(): void {
    this.filterMode = this.filterMode === 'all' ? 'pending' : 'all'
    this.requestUpdate()
  }

  private openAddModal(): void {
    this.showAddModal = true
    this.requestUpdate()
  }

  private closeAddModal(): void {
    this.showAddModal = false
    this.requestUpdate()
  }

  private selectPending(value: TreeValue, tool: string, key: string): void {
    this.selectedPending = {
      tool,
      key,
      value: value.value,
      request_id: value.requestId ?? '',
      prompt: value.prompt ?? '',
      once_only: value.onceOnly ?? false,
    }
    this.denyReason = ''
    this.requestUpdate()
  }

  private onDenyReasonInput = (event: Event): void => {
    this.denyReason = (event.target as HTMLTextAreaElement).value
    this.requestUpdate()
  }

  private dismissApprovalModal = (): void => {
    this.selectedPending = null
    this.denyReason = ''
    this.requestUpdate()
  }

  private async approvePending(decision: 'AllowOnce' | 'AllowSession' | 'AllowProject'): Promise<void> {
    const pending = this.selectedPending
    if (!pending || !this.sessionId || this.resolvingPermission) {
      return
    }

    this.resolvingPermission = true
    this.error = ''
    this.requestUpdate()

    try {
      await api.resolvePermission(this.sessionId, pending, decision)
      this.selectedPending = null
      this.denyReason = ''
      await this.refreshPermissions(true)
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to resolve permission'
      this.error = message
    } finally {
      this.resolvingPermission = false
      this.requestUpdate()
    }
  }

  private async denyPending(): Promise<void> {
    const pending = this.selectedPending
    if (!pending || !this.sessionId || this.resolvingPermission) {
      return
    }

    this.resolvingPermission = true
    this.error = ''
    this.requestUpdate()

    try {
      await api.resolvePermission(this.sessionId, pending, {
        Deny: { reason: this.denyReason.trim() || null },
      })
      this.selectedPending = null
      this.denyReason = ''
      await this.refreshPermissions(true)
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to resolve permission'
      this.error = message
    } finally {
      this.resolvingPermission = false
      this.requestUpdate()
    }
  }

  private renderApprovalModal() {
    const pending = this.selectedPending
    if (!pending) {
      return nothing
    }

    const busy = this.resolvingPermission

    return html`
      <div class="modal-backdrop permission-modal-backdrop" @click=${this.dismissApprovalModal}>
        <section
          class="modal-card permission-modal-card"
          role="dialog"
          aria-modal="true"
          aria-labelledby="tree-permission-modal-title"
          @click=${(e: Event) => e.stopPropagation()}
        >
          <header class="permission-modal-header">
            <div>
              <h2 id="tree-permission-modal-title" class="page-title">Permission required</h2>
              <p class="page-subtitle">
                Request <code>${pending.request_id}</code> · Tool <code>${pending.tool}</code>
              </p>
            </div>
          </header>
          <div class="permission-modal-content">
            ${this.error ? html`<div class="inline-alert error">${this.error}</div>` : nothing}
            <section class="permission-prompt-card" aria-label="Permission prompt">
              <div class="permission-section-label">Prompt</div>
              <div class="permission-prompt">${pending.prompt}</div>
            </section>
            <dl class="meta-list permission-meta-list">
              <div>
                <dt>Key</dt>
                <dd>${pending.key}</dd>
              </div>
              <div>
                <dt>Value</dt>
                <dd><code class="permission-code-value">${pending.value}</code></dd>
              </div>
              <div>
                <dt>Once only</dt>
                <dd>${pending.once_only ? 'yes' : 'no'}</dd>
              </div>
            </dl>
            <label class="permission-deny-reason">
              <span class="muted">Optional deny reason</span>
              <textarea
                rows="2"
                placeholder="Only used when denying this request"
                .value=${this.denyReason}
                @input=${this.onDenyReasonInput}
              ></textarea>
            </label>
          </div>
          <div class="modal-actions permission-modal-actions">
            <div class="permission-allow-actions">
              <button
                type="button"
                class="button success"
                @click=${() => void this.approvePending('AllowOnce')}
                ?disabled=${busy}
              >
                Allow Once
              </button>
              ${pending.once_only
                ? nothing
                : html`
                    <button
                      type="button"
                      class="button"
                      @click=${() => void this.approvePending('AllowSession')}
                      ?disabled=${busy}
                    >
                      Allow Session
                    </button>
                    <button
                      type="button"
                      class="button secondary"
                      @click=${() => void this.approvePending('AllowProject')}
                      ?disabled=${busy}
                    >
                      Allow Project
                    </button>
                  `}
            </div>
            <div class="permission-deny-actions">
              <button
                type="button"
                class="button danger"
                @click=${() => void this.denyPending()}
                ?disabled=${busy}
              >
                Deny
              </button>
            </div>
          </div>
        </section>
      </div>
    `
  }

  private async handleAddPermissionSubmit(event: CustomEvent): Promise<void> {
    const key = event.detail?.key as PermissionKey | undefined
    if (!this.sessionId || !key) {
      return
    }

    try {
      await api.addPermission(this.sessionId, key)
      this.closeAddModal()
      await this.refreshPermissions(true)
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to add permission'
      this.error = message
      this.requestUpdate()
      ;(event.target as HTMLElement & { busy?: boolean }).busy = false
    }
  }

  private async revokeValue(key: PermissionKey): Promise<void> {
    if (!this.sessionId) {
      return
    }

    const actionKey = `revoke\x00${key.tool}\x00${key.key}\x00${key.value}`
    if (this.revokingValues.has(actionKey)) {
      return
    }

    this.revokingValues.add(actionKey)
    this.requestUpdate()

    try {
      await api.revokePermission(this.sessionId, key)
      await this.refreshPermissions(true)
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to revoke permission'
      this.error = message
    } finally {
      this.revokingValues.delete(actionKey)
      this.requestUpdate()
    }
  }

  private isToolCollapsed(tool: string): boolean {
    return this.collapsedTools.has(tool)
  }

  private isKeyCollapsed(tool: string, key: string): boolean {
    return this.collapsedKeys.get(tool)?.has(key) ?? false
  }

  private renderLoadingBar() {
    if (!this.loading) {
      return nothing
    }
    return html`<div class="permission-tree-loading-bar"></div>`
  }

  private renderError() {
    if (!this.error) {
      return nothing
    }
    return html`<div class="inline-alert error">${this.error}</div>`
  }

  private renderEmpty() {
    if (this.loading) {
      return nothing
    }

    if (this.permissionState === null) {
      return html`<div class="empty-copy">No permissions to display.</div>`
    }

    const tree = this.buildTree()
    if (tree.length === 0) {
      if (this.filterMode === 'pending') {
        return html`<div class="empty-copy">No pending permissions</div>`
      }
      return html`<div class="empty-copy">No permissions to display.</div>`
    }

    return nothing
  }

  private renderFooter() {
    return html`
      <div class="permission-tree-footer">
        <button type="button" class="button small ghost" @click=${this.toggleFilter}>
          ${this.filterMode === 'pending' ? 'Show all' : 'Pending only'}
        </button>
        <button type="button" class="button small" @click=${this.openAddModal}>+ Add Permission</button>
      </div>
    `
  }

  private renderAddModal() {
    if (!this.showAddModal) {
      return nothing
    }

    return html`
      <tcode-add-permission-form
        @tcode-add-permission-submit=${this.handleAddPermissionSubmit}
        @tcode-add-permission-cancel=${this.closeAddModal}
      ></tcode-add-permission-form>
    `
  }

  private renderStatusBadge(status: 'pending' | 'session' | 'project') {
    if (status === 'pending') {
      return html`<span class="status-badge pending">?</span>`
    }
    if (status === 'session') {
      return html`<span class="perm-tag session">session</span>`
    }
    return html`<span class="perm-tag project">project</span>`
  }

  private renderValueActions(value: TreeValue, tool: string, key: string) {
    if (value.status === 'pending') {
      return html`
        ${value.prompt ? html`<div class="tree-value-prompt">${value.prompt}</div>` : nothing}
        <div class="tree-value-actions">
          <span class="tree-pending-hint">Click to review →</span>
        </div>
      `
    }

    const revokeKey: PermissionKey = { tool, key, value: value.value }
    const actionKey = `revoke\x00${tool}\x00${key}\x00${value.value}`
    const busy = this.revokingValues.has(actionKey)

    return html`
      <a
        class="revoke-link"
        @click=${(e: Event) => {
          e.preventDefault()
          if (!busy) void this.revokeValue(revokeKey)
        }}
      >
        revoke
      </a>
    `
  }

  private renderValueLeaf(value: TreeValue, tool: string, key: string) {
    const valueClass = value.isWildcard ? 'tree-value-text wildcard' : 'tree-value-text'
    const isPending = value.status === 'pending'

    return html`
      <div class="tree-value">
        <div
          class="tree-value-row${isPending ? ' pending-row' : ''}"
          @click=${isPending ? () => this.selectPending(value, tool, key) : undefined}
        >
          ${this.renderStatusBadge(value.status)}
          <span class="${valueClass}">${value.value}</span>
          ${value.isWildcard ? html`<span class="wildcard-label">(allow all)</span>` : nothing}
          ${this.renderValueActions(value, tool, key)}
        </div>
      </div>
    `
  }

  private renderKeyNode(treeTool: TreeTool, treeKey: TreeKey) {
    const keyCollapsed = this.isKeyCollapsed(treeTool.tool, treeKey.key)
    const toggle = keyCollapsed ? '+' : '−'

    return html`
      <div class="tree-key">
        <div class="tree-node tree-key-node" @click=${() => this.toggleKey(treeTool.tool, treeKey.key)}>
          <span class="tree-toggle">${toggle}</span>
          <span class="tree-label">${treeKey.key}</span>
        </div>
        ${keyCollapsed
          ? nothing
          : html`
              <div class="tree-children">
                ${treeKey.values.map(
                  (v) => this.renderValueLeaf(v, treeTool.tool, treeKey.key),
                )}
              </div>
            `}
      </div>
    `
  }

  private renderToolNode(tool: TreeTool) {
    const collapsed = this.isToolCollapsed(tool.tool)
    const toggle = collapsed ? '+' : '−'

    return html`
      <div class="tree-tool">
        <div class="tree-node tree-tool-node" @click=${() => this.toggleTool(tool.tool)}>
          <span class="tree-toggle">${toggle}</span>
          <span class="tree-label">${tool.tool}</span>
          ${tool.pendingCount > 0
            ? html`<span class="tree-badge">${tool.pendingCount}</span>`
            : nothing}
        </div>
        ${collapsed
          ? nothing
          : html`
              <div class="tree-children">
                ${tool.keys.map((k) => this.renderKeyNode(tool, k))}
              </div>
            `}
      </div>
    `
  }

  private renderTree() {
    if (this.loading) {
      return nothing
    }

    if (this.permissionState === null) {
      return this.renderEmpty()
    }

    const tree = this.buildTree()
    if (tree.length === 0) {
      return this.renderEmpty()
    }

    return html`
      <div class="permission-tree">
        ${tree.map((tool) => this.renderToolNode(tool))}
      </div>
    `
  }

  render() {
    return html`
      <div class="permission-tree-page">
        ${this.renderLoadingBar()}
        ${this.renderError()}
        ${this.renderTree()}
        ${this.renderFooter()}
      </div>
      ${this.renderAddModal()}
      ${this.renderApprovalModal()}
    `
  }
}

customElements.define('tcode-permission-tree', TcodePermissionTree)
