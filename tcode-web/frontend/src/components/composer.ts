import { LitElement, html, nothing, type TemplateResult } from 'lit';
import { SUPPORTED_IMAGE_TYPES } from '../image-types';

/** Resolve a file's MIME type, falling back to extension for files with empty type (e.g. HEIC on Chrome). */
function resolveImageType(file: File): string {
  if (file.type) return file.type;
  const name = file.name.toLowerCase();
  if (name.endsWith('.png')) return 'image/png';
  if (name.endsWith('.jpg') || name.endsWith('.jpeg')) return 'image/jpeg';
  if (name.endsWith('.gif')) return 'image/gif';
  if (name.endsWith('.webp')) return 'image/webp';
  if (name.endsWith('.heic') || name.endsWith('.heif')) return 'image/heic';
  return '';
}

export interface MessageSubmitDetail {
  text: string;
  imageFiles?: File[];
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
    hideImageAttach: { type: Boolean },
    imageFiles: { type: Array, attribute: false },
  };

  disabled = false;
  disconnected = false;
  sending = false;
  generating = false;
  cancelling = false;
  placeholder = 'Message…';
  hideImageAttach = false;
  resetToken = 0;
  secondaryAction: unknown = nothing;
  private text = '';
  private maxTextareaHeight: number | null = null;
  declare imageFiles: File[];

  createRenderRoot(): this {
    return this;
  }

  protected firstUpdated(): void {
    this.syncTextareaHeight();
  }

  protected willUpdate(changed: Map<string, unknown>): void {
    if (changed.has('resetToken')) {
      this.text = '';
      this.clearImageFiles();
    }
  }

  protected updated(changed: Map<string, unknown>): void {
    if (changed.has('resetToken')) {
      this.syncTextareaHeight();
    }
  }

  connectedCallback(): void {
    super.connectedCallback();
    if (this.imageFiles === undefined) {
      this.imageFiles = [];
    }
  }

  disconnectedCallback(): void {
    super.disconnectedCallback();
    this.clearImageFiles();
  }

  private imageFileUrls = new Map<File, string>();

  private clearImageFiles(): void {
    for (const url of this.imageFileUrls.values()) {
      URL.revokeObjectURL(url);
    }
    this.imageFileUrls.clear();
    this.imageFiles = [];
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
    return (Boolean(this.trimmedText) || this.imageFiles.length > 0)
      && !this.inputDisabled && !this.sending && !this.generating;
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
    const picker = this.querySelector<HTMLInputElement>('[data-role="image-picker"]');
    picker?.click();
  }

  private onFilePicked(event: Event): void {
    const input = event.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) {
      return;
    }

    const imageFiles: File[] = [];
    for (let i = 0; i < files.length; i++) {
      const file = files[i];
      if (file.type.startsWith('image/') || file.type === '') {
        imageFiles.push(file);
      }
    }

    if (imageFiles.length > 0) {
      this.addImageFiles(imageFiles);
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

    const imageFiles: File[] = [];
    for (let i = 0; i < files.length; i++) {
      if (files[i].type.startsWith('image/') || files[i].type === '') {
        imageFiles.push(files[i]);
      }
    }

    if (imageFiles.length > 0) {
      this.addImageFiles(imageFiles);
    }
  };

  private onPaste = (event: ClipboardEvent): void => {
    const items = event.clipboardData?.items;
    if (!items) {
      return;
    }

    const imageFiles: File[] = [];
    for (let i = 0; i < items.length; i++) {
      if (items[i].type.startsWith('image/') || items[i].type === '') {
        const file = items[i].getAsFile();
        if (file) {
          imageFiles.push(file);
        }
      }
    }

    if (imageFiles.length > 0) {
      event.preventDefault();
      this.addImageFiles(imageFiles);
    }
  };

  private addImageFiles(files: File[]): void {
    const MAX_FILE_SIZE = 20 * 1024 * 1024; // 20MB
    const oversized = files.filter(f => f.size > MAX_FILE_SIZE);
    if (oversized.length > 0) {
      this.notify(`Some files exceed 20MB limit and were skipped: ${oversized.map(f => f.name).join(', ')}`, 'info');
      files = files.filter(f => f.size <= MAX_FILE_SIZE);
    }
    const unsupported = files.filter(f => !SUPPORTED_IMAGE_TYPES.includes(resolveImageType(f)));
    if (unsupported.length > 0) {
      this.notify(`Unsupported image type(s): ${unsupported.map(f => `${f.name} (${resolveImageType(f) || 'unknown'})`).join(', ')}. Supported formats: PNG, JPEG/JPG, GIF, WebP.`, 'info');
      files = files.filter(f => SUPPORTED_IMAGE_TYPES.includes(resolveImageType(f)));
    }
    if (files.length === 0) return;
    for (const file of files) {
      this.imageFileUrls.set(file, URL.createObjectURL(file));
    }
    this.imageFiles = [...this.imageFiles, ...files];
    this.requestUpdate();
  }

  private removeImage(index: number): void {
    const file = this.imageFiles[index];
    const url = this.imageFileUrls.get(file);
    if (url) {
      URL.revokeObjectURL(url);
      this.imageFileUrls.delete(file);
    }
    this.imageFiles = this.imageFiles.filter((_, i) => i !== index);
    this.requestUpdate();
  }

  private emitSubmit(): void {
    if (!this.canSubmit) {
      return;
    }

    this.dispatchEvent(
      new CustomEvent<MessageSubmitDetail>('message-submit', {
        detail: { text: this.trimmedText, imageFiles: this.imageFiles.length > 0 ? [...this.imageFiles] : undefined },
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
        ${this.imageFiles.length > 0 ? html`
          <div class="image-preview-row">
            ${this.imageFiles.map((file, index) => html`
              <div class="image-preview-item">
                <img src=${this.imageFileUrls.get(file)} alt="Preview" class="image-preview-thumb">
                <button class="image-preview-remove" type="button" @click=${() => this.removeImage(index)} aria-label="Remove image">×</button>
              </div>
            `)}
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
            ${this.hideImageAttach ? nothing : html`
              <button class="button chat-composer-action" type="button" @click=${this.openFilePicker}
                ?disabled=${this.inputDisabled} aria-label="Attach images" title="Attach images">
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
        ${this.hideImageAttach ? nothing : html`
          <input type="file" accept="image/*" multiple hidden data-role="image-picker" @change=${this.onFilePicked}>
        `}
      </form>
    `;
  }
}

customElements.define('tcode-composer', TcodeComposer);
