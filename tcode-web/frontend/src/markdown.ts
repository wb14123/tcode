import DOMPurify from 'dompurify';
import katex, { type KatexOptions } from 'katex';
import { marked, type TokenizerAndRendererExtension, type Tokens } from 'marked';

const allowedProtocols = new Set(['http:', 'https:']);
const externalLinkRel = 'noopener noreferrer nofollow ugc';
const safeMarkdownTags = [
  'a',
  'blockquote',
  'br',
  'code',
  'del',
  'em',
  'h1',
  'h2',
  'h3',
  'h4',
  'h5',
  'h6',
  'hr',
  'li',
  'ol',
  'p',
  'pre',
  'span',
  'strong',
  'table',
  'tbody',
  'td',
  'th',
  'thead',
  'tr',
  'ul',
];
const safeMarkdownAttributes = ['class', 'data-display', 'data-tex', 'href', 'start', 'title'];
const preservedAnchorAttributes = new Set(['href', 'title']);
const markdownSanitizationConfig = {
  ALLOWED_ATTR: safeMarkdownAttributes,
  ALLOWED_TAGS: safeMarkdownTags,
  ALLOW_DATA_ATTR: true,
};

const parenthesizedMath = /^\\\(([\s\S]+?)\\\)/;
const bracketedMath = /^\\\[([\s\S]+?)\\\]/;
const doubleDollarMath = /^\$\$([\s\S]+?)\$\$/;
// Guarded `$...$` span: these guards keep currency prose such as `costs $5 and $10` literal.
const singleDollarMath = /^\$(?!\s)((?:\\.|[^\\\n$])+?)(?<!\s)\$(?!\d)/;

const katexOptions: KatexOptions = {
  output: 'htmlAndMathml',
  strict: 'ignore',
  throwOnError: true,
  trust: false,
  maxSize: 10,
};

type MathToken = {
  type: 'math-inline' | 'math-block';
  raw: string;
  tex: string;
  display: boolean;
};

let lastMarkdownSource = '';
let lastRenderedHtml = '';

function escapeAttributeValue(value: string): string {
  return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function mathPlaceholder(tex: string, display: boolean): string {
  const displayMode = display ? 'block' : 'inline';
  return `<span class="math-src" data-display="${displayMode}" data-tex="${escapeAttributeValue(tex)}"></span>`;
}

function renderMathPlaceholder(token: Tokens.Generic): string {
  const math = token as MathToken;
  return mathPlaceholder(math.tex, math.display);
}

function matchInlineMath(src: string): MathToken | undefined {
  const parenthesized = parenthesizedMath.exec(src);
  if (parenthesized) {
    return { type: 'math-inline', raw: parenthesized[0], tex: parenthesized[1], display: false };
  }

  const bracketed = bracketedMath.exec(src);
  if (bracketed) {
    return { type: 'math-inline', raw: bracketed[0], tex: bracketed[1], display: true };
  }

  const doubleDollar = doubleDollarMath.exec(src);
  if (doubleDollar) {
    return { type: 'math-inline', raw: doubleDollar[0], tex: doubleDollar[1], display: true };
  }

  const singleDollar = singleDollarMath.exec(src);
  if (singleDollar) {
    return { type: 'math-inline', raw: singleDollar[0], tex: singleDollar[1], display: false };
  }

  return undefined;
}

function matchBlockMath(src: string): MathToken | undefined {
  const bracketed = bracketedMath.exec(src);
  if (bracketed) {
    return { type: 'math-block', raw: bracketed[0], tex: bracketed[1], display: true };
  }

  const doubleDollar = doubleDollarMath.exec(src);
  if (doubleDollar) {
    return { type: 'math-block', raw: doubleDollar[0], tex: doubleDollar[1], display: true };
  }

  return undefined;
}

const mathInlineExtension: TokenizerAndRendererExtension = {
  name: 'math-inline',
  level: 'inline',
  start(src) {
    return src.search(/\$|\\[(\[]/);
  },
  tokenizer(src) {
    return matchInlineMath(src);
  },
  renderer: renderMathPlaceholder,
};

const mathBlockExtension: TokenizerAndRendererExtension = {
  name: 'math-block',
  level: 'block',
  start(src) {
    // A display delimiter only survives at a line start; bound the paragraph there so it
    // can be tokenized even when it spans a blank line.
    return src.search(/\n[ \t]*(?:\\\[|\$\$)/);
  },
  tokenizer(src) {
    return matchBlockMath(src);
  },
  renderer: renderMathPlaceholder,
};

marked.setOptions({
  breaks: true,
  gfm: true,
});
marked.use({ extensions: [mathInlineExtension, mathBlockExtension] });

function stripUnsafeLink(anchor: HTMLAnchorElement): void {
  anchor.removeAttribute('href');
  anchor.removeAttribute('target');
  anchor.removeAttribute('rel');
  anchor.removeAttribute('referrerpolicy');
}

function sanitizeAnchor(anchor: HTMLAnchorElement): void {
  for (const attribute of [...anchor.attributes]) {
    if (!preservedAnchorAttributes.has(attribute.name)) {
      anchor.removeAttribute(attribute.name);
    }
  }

  const rawHref = anchor.getAttribute('href');
  if (!rawHref) {
    stripUnsafeLink(anchor);
    return;
  }

  let url: URL;
  try {
    url = new URL(rawHref, window.location.href);
  } catch {
    stripUnsafeLink(anchor);
    return;
  }

  if (!allowedProtocols.has(url.protocol)) {
    stripUnsafeLink(anchor);
    return;
  }

  anchor.setAttribute('href', url.toString());
  anchor.setAttribute('referrerpolicy', 'no-referrer');

  if (url.origin !== window.location.origin) {
    anchor.setAttribute('target', '_blank');
    anchor.setAttribute('rel', externalLinkRel);
    return;
  }

  anchor.removeAttribute('target');
  anchor.removeAttribute('rel');
}

function sanitizeAnchors(container: ParentNode): void {
  const anchors = container.querySelectorAll('a');
  for (const anchor of anchors) {
    sanitizeAnchor(anchor);
  }
}

function renderMathNode(tex: string, displayMode: boolean): Node {
  try {
    const markup = katex.renderToString(tex, { ...katexOptions, displayMode });
    const template = document.createElement('template');
    template.innerHTML = markup;
    return template.content;
  } catch (error) {
    console.warn(`tcode: failed to typeset LaTeX: ${tex}`, error);
    const fallback = document.createElement('span');
    fallback.className = 'math-fallback';
    fallback.textContent = tex;
    return fallback;
  }
}

function typesetMath(container: ParentNode): void {
  const placeholders = container.querySelectorAll('span.math-src');
  for (const placeholder of placeholders) {
    const tex = placeholder.getAttribute('data-tex') ?? '';
    const displayMode = placeholder.getAttribute('data-display') === 'block';
    placeholder.replaceWith(renderMathNode(tex, displayMode));
  }
}

export function renderMarkdownToHtml(markdown: string): string {
  if (!markdown) {
    lastMarkdownSource = markdown;
    lastRenderedHtml = '';
    return '';
  }

  if (markdown === lastMarkdownSource) {
    return lastRenderedHtml;
  }

  const renderedMarkdown = marked.parse(markdown) as string;
  const sanitizedHtml = DOMPurify.sanitize(renderedMarkdown, markdownSanitizationConfig);
  const template = document.createElement('template');
  template.innerHTML = sanitizedHtml;
  typesetMath(template.content);
  sanitizeAnchors(template.content);

  lastMarkdownSource = markdown;
  lastRenderedHtml = template.innerHTML;
  return lastRenderedHtml;
}
