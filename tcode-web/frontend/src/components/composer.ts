import { LitElement, html, nothing, type TemplateResult } from 'lit';

export interface MessageSubmitDetail {
  text: string;
}

class TcodeComposer extends LitElement {
  static properties = {
    disabled: { type: Boolean },
    disconnected: { type: Boolean },
    sending: { type: Boolean },
    generating: { type: Boolean },
    cancelling: { type: Boolean },
    placeholder: { type: String },
    resetToken: { type: Number },
    secondaryAction: { attribute: false },
  };

  disabled = false;
  disconnected = false;
  sending = false;
  generating = false;
  cancelling = false;
  placeholder = 'Message…';
  resetToken = 0;
  secondaryAction: unknown = nothing;
  private text = '';
  private maxTextareaHeight: number | null = null;

  createRenderRoot(): this {
    return this;
  }

  protected firstUpdated(): void {
    this.syncTextareaHeight();
  }

  protected willUpdate(changed: Map<string, unknown>): void {
    if (changed.has('resetToken')) {
      this.text = '';
    }
  }

  protected updated(changed: Map<string, unknown>): void {
    if (changed.has('resetToken')) {
      this.syncTextareaHeight();
    }
  }

  private get inputDisabled(): boolean {
    return this.disabled || this.disconnected;
  }

  private get trimmedText(): string {
    return this.text.trim();
  }

  private get canSubmit(): boolean {
    return Boolean(this.trimmedText) && !this.inputDisabled && !this.sending && !this.generating;
  }

  private syncTextareaHeight(textarea?: HTMLTextAreaElement | null): void {
    const input = textarea ?? this.querySelector<HTMLTextAreaElement>('.chat-composer-input');
    if (!input) {
      return;
    }

    input.style.height = 'auto';
    if (this.maxTextareaHeight === null) {
      this.maxTextareaHeight = Number.parseFloat(window.getComputedStyle(input).maxHeight) || 160;
    }
    const scrollHeight = input.scrollHeight;
    input.style.height = `${Math.min(scrollHeight, this.maxTextareaHeight)}px`;
    input.style.overflowY = scrollHeight > this.maxTextareaHeight ? 'auto' : 'hidden';
  }

  private onInput(event: Event): void {
    const target = event.target as HTMLTextAreaElement;
    this.text = target.value;
    this.syncTextareaHeight(target);
    this.requestUpdate();
  }

  private onKeyDown = (event: KeyboardEvent): void => {
    if (
      event.key !== 'Enter' ||
      event.shiftKey ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.isComposing
    ) {
      return;
    }

    event.preventDefault();
    this.emitSubmit();
  };

  private onSubmit(event: SubmitEvent): void {
    event.preventDefault();
    this.emitSubmit();
  }

  private emitSubmit(): void {
    if (!this.canSubmit) {
      return;
    }

    this.dispatchEvent(
      new CustomEvent<MessageSubmitDetail>('message-submit', {
        detail: { text: this.trimmedText },
        bubbles: true,
        composed: true,
      }),
    );
  }

  private emitCancel(): void {
    if (this.inputDisabled || this.cancelling) {
      return;
    }

    this.dispatchEvent(
      new CustomEvent('cancel-requested', {
        detail: {},
        bubbles: true,
        composed: true,
      }),
    );
  }

  private renderSendIcon(): TemplateResult {
    return html`
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M3.4 20.4 19.85 13.35a1.5 1.5 0 0 0 0-2.76L3.4 3.6a1 1 0 0 0-1.37 1.22l2.36 6.49a1 1 0 0 0 .94.66h7.36a1 1 0 1 1 0 2H5.33a1 1 0 0 0-.94.66l-2.36 6.49A1 1 0 0 0 3.4 20.4Z"></path>
      </svg>
    `;
  }

  private renderCancelIcon(): TemplateResult {
    return html`
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M7 7h10v10H7z"></path>
      </svg>
    `;
  }

  render(): TemplateResult {
    return html`
      <form class="panel chat-composer" @submit=${this.onSubmit}>
        <div class="chat-composer-row">
          <textarea
            class="chat-composer-input"
            rows="1"
            placeholder=${this.placeholder}
            .value=${this.text}
            ?disabled=${this.inputDisabled}
            @input=${this.onInput}
            @keydown=${this.onKeyDown}
          ></textarea>
          <div class="chat-composer-actions">
            ${this.secondaryAction}
            ${this.generating
              ? html`
                  <button
                    class="button danger chat-composer-action"
                    type="button"
                    @click=${this.emitCancel}
                    ?disabled=${this.inputDisabled || this.cancelling}
                    aria-label=${this.cancelling ? 'Cancelling conversation' : 'Cancel conversation'}
                    title=${this.cancelling ? 'Cancelling…' : 'Cancel conversation'}
                  >
                    ${this.renderCancelIcon()}
                  </button>
                `
              : html`
                  <button
                    class="button chat-composer-action"
                    type="submit"
                    ?disabled=${!this.canSubmit}
                    aria-label=${this.sending ? 'Sending message' : 'Send message'}
                    title=${this.sending ? 'Sending…' : 'Send message'}
                  >
                    ${this.renderSendIcon()}
                  </button>
                `}
          </div>
        </div>
      </form>
    `;
  }
}

customElements.define('tcode-composer', TcodeComposer);
