import { LitElement, html, nothing } from 'lit';

import type { PermissionKey } from '../types';
import { ALL_SCOPES } from '../types';

export interface AddPermissionSubmitDetail {
  key: PermissionKey;
}

class TcodeAddPermissionForm extends LitElement {
  static properties = {
    cancelLabel: { type: String },
    busy: { type: Boolean },
  };

  cancelLabel = 'Cancel';

  private tool: string | null = null;
  private key: string | null = null;
  private value = '';
  private wildcard = false;
  private busy = false;

  createRenderRoot(): this {
    return this;
  }

  private onToolSelect(tool: string): void {
    this.tool = tool;
    this.key = null;
    this.value = '';
    this.wildcard = false;
    this.requestUpdate();
  }

  private onKeySelect(key: string): void {
    this.key = key;
    this.value = '';
    this.wildcard = false;
    this.requestUpdate();
  }

  private onValueInput(event: Event): void {
    this.value = (event.target as HTMLInputElement).value;
    this.requestUpdate();
  }

  private onWildcardToggle(event: Event): void {
    this.wildcard = (event.target as HTMLInputElement).checked;
    if (this.wildcard) {
      this.value = '';
    }
    this.requestUpdate();
  }

  private onSubmit(): void {
    if (!this.tool || !this.key || this.busy) {
      return;
    }

    const value = this.wildcard ? '*' : this.value.trim();
    if (!value) {
      return;
    }

    this.busy = true;
    this.requestUpdate();

    const key: PermissionKey = {
      tool: this.tool,
      key: this.key,
      value,
    };

    this.dispatchEvent(
      new CustomEvent<AddPermissionSubmitDetail>('tcode-add-permission-submit', {
        detail: { key },
        bubbles: true,
        composed: true,
      }),
    );
  }

  private onCancel(): void {
    this.dispatchEvent(
      new CustomEvent('tcode-add-permission-cancel', {
        bubbles: true,
        composed: true,
      }),
    );
  }

  render() {
    const toolKeys = Object.keys(ALL_SCOPES);
    const selectedToolKeys = this.tool ? (ALL_SCOPES[this.tool] ?? []) : [];
    const canSubmit = this.tool !== null
      && this.key !== null
      && (this.wildcard || this.value.trim().length > 0);

    return html`
      <div class="modal-backdrop add-permission-form-backdrop" @click=${this.onCancel}>
        <section
          class="modal-card add-permission-form-card"
          role="dialog"
          aria-modal="true"
          aria-labelledby="add-permission-modal-title"
          @click=${(e: Event) => e.stopPropagation()}
        >
          <header class="add-permission-form-header">
            <div>
              <h2 id="add-permission-modal-title" class="page-title">Add session permission</h2>
              <p class="page-subtitle">Manually grant a permission for this session</p>
            </div>
          </header>
          <div class="add-permission-form">
            <div class="add-permission-content">
              <div class="add-permission-step">
                <div class="add-permission-step-label">1. Select tool</div>
                <div class="add-permission-tool-options">
                  ${toolKeys.map(
                    (tool) => html`
                      <button
                        type="button"
                        class="add-permission-option-button ${this.tool === tool ? 'selected' : ''}"
                        @click=${() => this.onToolSelect(tool)}
                      >
                        ${tool}
                      </button>
                    `,
                  )}
                </div>
              </div>

              ${this.tool
                ? html`
                    <div class="add-permission-step">
                      <div class="add-permission-step-label">2. Select key</div>
                      <div class="add-permission-key-options">
                        ${selectedToolKeys.map(
                          (key) => html`
                            <button
                              type="button"
                              class="add-permission-option-button ${this.key === key ? 'selected' : ''}"
                              @click=${() => this.onKeySelect(key)}
                            >
                              ${key}
                            </button>
                          `,
                        )}
                      </div>
                    </div>
                  `
                : nothing}

              ${this.key
                ? html`
                    <div class="add-permission-step">
                      <div class="add-permission-step-label">3. Enter value</div>
                      <div class="add-permission-value-row">
                        <input
                          type="text"
                          class="add-permission-value-input"
                          placeholder="e.g. /home/user/*"
                          .value=${this.value}
                          @input=${this.onValueInput}
                          ?disabled=${this.wildcard}
                        />
                      </div>
                      <label class="add-permission-wildcard">
                        <input
                          type="checkbox"
                          .checked=${this.wildcard}
                          @change=${this.onWildcardToggle}
                        />
                        Allow all values (*)
                      </label>
                    </div>
                  `
                : nothing}
            </div>

            <div class="modal-actions add-permission-actions">
              <button
                type="button"
                class="button add-permission-submit"
                @click=${this.onSubmit}
                ?disabled=${!canSubmit || this.busy}
              >
                ${this.busy ? 'Adding…' : 'Add to session'}
              </button>
              <button
                type="button"
                class="button ghost add-permission-back"
                @click=${this.onCancel}
                ?disabled=${this.busy}
              >
                ${this.cancelLabel}
              </button>
            </div>
          </div>
        </section>
      </div>
    `;
  }
}

customElements.define('tcode-add-permission-form', TcodeAddPermissionForm);
