function normalizeApiBase(value: string | undefined): string {
  if (!value) {
    return '';
  }

  return value.endsWith('/') ? value : `${value}/`;
}

function normalizeRouterBase(value: string | undefined): string {
  const fallback = import.meta.env?.BASE_URL || '/';
  const raw = value || fallback || '/';
  const withLeadingSlash = raw.startsWith('/') ? raw : `/${raw}`;
  return withLeadingSlash.endsWith('/') ? withLeadingSlash : `${withLeadingSlash}/`;
}

const runtime = globalThis.window?.__TCODE_WEB_CONFIG__ ?? {};

export const runtimeConfig = {
  apiBase: normalizeApiBase(runtime.apiBase),
  routerBase: normalizeRouterBase(runtime.routerBase),
  eventSourceWithCredentials: runtime.eventSourceWithCredentials ?? true,
};
