export interface SpecialToolArgsPresentation {
  collapsedSummary: string;
  expandedText: string;
}

type RecognizedWebToolName = 'web_search' | 'web_fetch';

function normalizedToolName(toolName: string): RecognizedWebToolName | null {
  const normalized = toolName.trim().split(/[./]/).pop();
  if (normalized === 'web_search' || normalized === 'web_fetch') {
    return normalized;
  }
  return null;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }

  return null;
}

function nonEmptyString(value: unknown): string | null {
  if (typeof value !== 'string') {
    return null;
  }

  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function hasDisplayableValue(value: unknown): boolean {
  if (value === null || value === undefined) {
    return false;
  }
  if (typeof value === 'string') {
    return value.trim() !== '';
  }
  if (Array.isArray(value)) {
    return value.length > 0;
  }
  if (typeof value === 'object') {
    return Object.keys(value as Record<string, unknown>).length > 0;
  }
  return true;
}

function compactValue(value: unknown): string {
  if (typeof value === 'string') {
    return value;
  }

  try {
    return JSON.stringify(value) ?? String(value);
  } catch {
    return String(value);
  }
}

function compactExtras(record: Record<string, unknown>, hiddenKeys: Set<string>): string | null {
  const extras = Object.fromEntries(
    Object.entries(record).filter(([key, value]) => !hiddenKeys.has(key) && hasDisplayableValue(value)),
  );

  if (Object.keys(extras).length === 0) {
    return null;
  }

  return compactValue(extras);
}

function parseArgs(toolArgs: string): Record<string, unknown> | null {
  try {
    return asRecord(JSON.parse(toolArgs) as unknown);
  } catch {
    return null;
  }
}

function webSearchPresentation(record: Record<string, unknown>): SpecialToolArgsPresentation | null {
  const query = nonEmptyString(record.query);
  if (!query) {
    return null;
  }

  const lines = [`Query: ${query}`];
  const extras = compactExtras(record, new Set(['query']));
  if (extras) {
    lines.push(`Extra: ${extras}`);
  }

  return {
    collapsedSummary: query,
    expandedText: lines.join('\n'),
  };
}

function webFetchPresentation(record: Record<string, unknown>): SpecialToolArgsPresentation | null {
  const url = nonEmptyString(record.url);
  if (!url) {
    return null;
  }

  const lines = [`URL: ${url}`];
  for (const key of ['max_length', 'skip_chars']) {
    if (Object.hasOwn(record, key) && hasDisplayableValue(record[key])) {
      lines.push(`${key}: ${compactValue(record[key])}`);
    }
  }

  const extras = compactExtras(record, new Set(['url', 'max_length', 'skip_chars']));
  if (extras) {
    lines.push(`Extra: ${extras}`);
  }

  return {
    collapsedSummary: url,
    expandedText: lines.join('\n'),
  };
}

export function isSpecialWebToolName(toolName: string): boolean {
  return normalizedToolName(toolName) !== null;
}

export function specialToolArgsPresentation(toolName: string, toolArgs: string): SpecialToolArgsPresentation | null {
  const recognizedName = normalizedToolName(toolName);
  if (!recognizedName || !toolArgs.trim()) {
    return null;
  }

  const record = parseArgs(toolArgs);
  if (!record) {
    return null;
  }

  if (recognizedName === 'web_search') {
    return webSearchPresentation(record);
  }

  return webFetchPresentation(record);
}
