import DOMPurify from 'dompurify';
import { marked } from 'marked';

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
  'strong',
  'table',
  'tbody',
  'td',
  'th',
  'thead',
  'tr',
  'ul',
];
const safeMarkdownAttributes = ['class', 'href', 'start', 'title'];
const preservedAnchorAttributes = new Set(['href', 'title']);
const markdownSanitizationConfig = {
  ALLOWED_ATTR: safeMarkdownAttributes,
  ALLOWED_TAGS: safeMarkdownTags,
};

let lastMarkdownSource = '';
let lastRenderedHtml = '';

marked.setOptions({
  breaks: true,
  gfm: true,
});

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
  sanitizeAnchors(template.content);

  lastMarkdownSource = markdown;
  lastRenderedHtml = template.innerHTML;
  return lastRenderedHtml;
}
