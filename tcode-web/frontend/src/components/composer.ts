import { LitElement, html, nothing, type TemplateResult } from 'lit';
import { processImageFile } from '../image-processing';

export interface MessageSubmitDetail {
  text: string;
  mediaFiles?: File[];
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
    hideMediaAttach: { type: Boolean },
    processingMedia: { type: Boolean },
    mediaFiles: { type: Array, attribute: false },
  };

  disabled = false;
  disconnected = false;
  sending = false;
  generating = false;
  cancelling = false;
  placeholder = 'Message…';
  hideMediaAttach = false;
  processingMedia = false;
  resetToken = 0;
  secondaryAction: unknown = nothing;
  private text = '';
  private maxTextareaHeight: number | null = null;
  declare mediaFiles: File[];

  createRenderRoot(): this {
    return this;
  }

  protected firstUpdated(): void {
    this.syncTextareaHeight();
  }

  protected willUpdate(changed: Map<string, unknown>): void {
    if (changed.has('resetToken')) {
      this.text = '';
      this.clearMediaFiles();
    }
  }

  protected updated(changed: Map<string, unknown>): void {
    if (changed.has('resetToken')) {
      this.syncTextareaHeight();
    }
  }

  connectedCallback(): void {
    super.connectedCallback();
    if (this.mediaFiles === undefined) {
      this.mediaFiles = [];
    }
  }

  disconnectedCallback(): void {
    super.disconnectedCallback();
    this.clearMediaFiles();
  }

  private mediaFileUrls = new Map<File, string>();

  private clearMediaFiles(): void {
    for (const url of this.mediaFileUrls.values()) {
      URL.revokeObjectURL(url);
    }
    this.mediaFileUrls.clear();
    this.mediaFiles = [];
  }

  /** Dispatch a notification event so parent views can show a toast. */
  private notify(message: string, tone: 'error' | 'info'): void {
    this.dispatchEvent(
      new CustomEvent('composer-notification', {
        detail: { message, tone },
        bubbles: true,
      }),
    );
  }

  private get inputDisabled(): boolean {
    return this.disabled || this.disconnected;
  }

  private get trimmedText(): string {
    return this.text.trim();
  }

  private get canSubmit(): boolean {
    return (Boolean(this.trimmedText) || this.mediaFiles.length > 0)
      && !this.inputDisabled && !this.sending && !this.generating && !this.processingMedia;
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

  private openFilePicker(): void {
    const picker = this.querySelector<HTMLInputElement>('[data-role="media-picker"]');
    picker?.click();
  }

  private onFilePicked(event: Event): void {
    const input = event.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) {
      return;
    }

    const mediaFiles: File[] = [];
    for (let i = 0; i < files.length; i++) {
      const file = files[i];
      if (file.type.startsWith('image/') || file.type === 'application/pdf' || file.type === '') {
        mediaFiles.push(file);
      }
    }

    if (mediaFiles.length > 0) {
      void this.addMediaFiles(mediaFiles);
    }

    // Reset so the same file can be picked again
    input.value = '';
  }

  private onDragOver = (event: DragEvent): void => {
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = 'copy';
    }
  };

  private onDrop = (event: DragEvent): void => {
    event.preventDefault();
    const files = event.dataTransfer?.files;
    if (!files || files.length === 0) {
      return;
    }

    const mediaFiles: File[] = [];
    for (let i = 0; i < files.length; i++) {
      if (files[i].type.startsWith('image/') || files[i].type === 'application/pdf' || files[i].type === '') {
        mediaFiles.push(files[i]);
      }
    }

    if (mediaFiles.length > 0) {
      void this.addMediaFiles(mediaFiles);
    }
  };

  private onPaste = (event: ClipboardEvent): void => {
    const items = event.clipboardData?.items;
    if (!items) {
      return;
    }

    const mediaFiles: File[] = [];
    for (let i = 0; i < items.length; i++) {
      if (items[i].type.startsWith('image/') || items[i].type === 'application/pdf' || items[i].type === '') {
        const file = items[i].getAsFile();
        if (file) {
          mediaFiles.push(file);
        }
      }
    }

    if (mediaFiles.length > 0) {
      event.preventDefault();
      void this.addMediaFiles(mediaFiles);
    }
  };

  private async addMediaFiles(files: File[]): Promise<void> {
    if (this.processingMedia) {
      this.notify('Still processing previous files, please wait.', 'info');
      return;
    }
    const MAX_FILE_SIZE = 20 * 1024 * 1024; // 20 MB
    const oversized = files.filter(f => f.size > MAX_FILE_SIZE);
    if (oversized.length > 0) {
      this.notify(
        `Some files exceed 20 MB limit and were skipped: ${oversized.map(f => f.name).join(', ')}`,
        'info',
      );
      files = files.filter(f => f.size <= MAX_FILE_SIZE);
    }

    // Filter to supported types (image/*, application/pdf, or empty type)
    files = files.filter(f => f.type.startsWith('image/') || f.type === 'application/pdf' || f.type === '');
    if (files.length === 0) return;

    this.processingMedia = true;
    this.requestUpdate();

    const processed: File[] = [];
    const errors: string[] = [];
    for (const file of files) {
      try {
        if (file.type === 'application/pdf') {
          // Skip image processing for PDFs — keep the file as-is
          processed.push(file);
        } else {
          const result = await processImageFile(file);
          processed.push(result);
        }
      } catch (err) {
        errors.push(err instanceof Error ? err.message : `Failed to process ${file.name}`);
      }
    }

    this.processingMedia = false;

    if (errors.length > 0) {
      this.notify(errors.join(' '), 'error');
    }

    if (processed.length === 0) {
      this.requestUpdate();
      return;
    }

    for (const file of processed) {
      this.mediaFileUrls.set(file, URL.createObjectURL(file));
    }
    this.mediaFiles = [...this.mediaFiles, ...processed];
    this.requestUpdate();
  }

  private removeMedia(index: number): void {
    const file = this.mediaFiles[index];
    const url = this.mediaFileUrls.get(file);
    if (url) {
      URL.revokeObjectURL(url);
      this.mediaFileUrls.delete(file);
    }
    this.mediaFiles = this.mediaFiles.filter((_, i) => i !== index);
    this.requestUpdate();
  }

  private emitSubmit(): void {
    if (!this.canSubmit) {
      return;
    }

    this.dispatchEvent(
      new CustomEvent<MessageSubmitDetail>('message-submit', {
        detail: { text: this.trimmedText, mediaFiles: this.mediaFiles.length > 0 ? [...this.mediaFiles] : undefined },
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

  private renderAttachIcon(): TemplateResult {
    return html`
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M21.44 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l9.19-9.19a4 4 0 015.66 5.66l-9.2 9.19a2 2 0 01-2.83-2.83l8.49-8.48"></path>
      </svg>
    `;
  }

  render(): TemplateResult {
    return html`
      <form class="panel chat-composer" @submit=${this.onSubmit}>
        ${this.processingMedia ? html`
          <div class="media-preview-row">
            <span class="media-processing-text">Processing…</span>
          </div>
        ` : nothing}
        ${this.mediaFiles.length > 0 ? html`
          <div class="media-preview-row">
            ${this.mediaFiles.map((file, index) => {
              const isPdf = file.type === 'application/pdf';
              if (isPdf) {
                return html`
                  <div class="pdf-preview-item">
                    <span class="pdf-preview-icon">📄</span>
                    <span class="pdf-preview-name">${file.name}</span>
                    <button class="media-preview-remove" type="button" @click=${() => this.removeMedia(index)} aria-label="Remove file">×</button>
                  </div>
                `;
              }
              return html`
                <div class="media-preview-item">
                  <img src=${this.mediaFileUrls.get(file)} alt="Preview" class="media-preview-thumb">
                  <button class="media-preview-remove" type="button" @click=${() => this.removeMedia(index)} aria-label="Remove image">×</button>
                </div>
              `;
            })}
          </div>
        ` : nothing}
        <div class="chat-composer-row">
          <textarea
            class="chat-composer-input"
            rows="1"
            placeholder=${this.placeholder}
            .value=${this.text}
            ?disabled=${this.inputDisabled}
            @input=${this.onInput}
            @keydown=${this.onKeyDown}
            @dragover=${this.onDragOver}
            @drop=${this.onDrop}
            @paste=${this.onPaste}
          ></textarea>
          <div class="chat-composer-actions">
            ${this.secondaryAction}
            ${this.hideMediaAttach ? nothing : html`
              <button class="button chat-composer-action" type="button" @click=${this.openFilePicker}
                ?disabled=${this.inputDisabled} aria-label="Attach files" title="Attach files">
                ${this.renderAttachIcon()}
              </button>
            `}
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
        ${this.hideMediaAttach ? nothing : html`
          <input type="file" accept="image/*,application/pdf,.pdf" multiple hidden data-role="media-picker" @change=${this.onFilePicked}>
        `}
      </form>
    `;
  }
}

customElements.define('tcode-composer', TcodeComposer);
