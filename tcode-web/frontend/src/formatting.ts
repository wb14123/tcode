export function formatTimestamp(timestamp: number | null | undefined): string {
  if (timestamp === null || timestamp === undefined) {
    return '—';
  }

  return new Date(timestamp).toLocaleString();
}

export function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
