/**
 * Extract the primary failure reason from a test's captured pytest log.
 *
 * A single "test" row corresponds to one test file, whose log may contain
 * several pytest cases. Pytest prints a `short test summary info` block at the
 * end with one concise `FAILED <nodeid> - <message>` (or `ERROR …`) line per
 * failure — those are the cleanest summaries. We fall back to the first `E   `
 * assertion line when no summary block is present.
 */
export function extractTestErrors(logs: string | null | undefined): string[] {
  if (!logs) return [];
  const lines = logs.split('\n');

  const summaryIdx = lines.findIndex((l) => l.includes('short test summary info'));
  if (summaryIdx !== -1) {
    const summary = lines
      .slice(summaryIdx + 1)
      .map((l) => l.trim())
      .filter((l) => /^(FAILED|ERROR)\b/.test(l))
      // Drop the leading "FAILED <nodeid> - " marker, keeping the message when present.
      .map((l) => {
        const dash = l.indexOf(' - ');
        return dash !== -1 ? l.slice(dash + 3).trim() : l;
      });
    if (summary.length) return summary;
  }

  const eLines = lines
    .filter((l) => /^E\s/.test(l))
    .map((l) => l.replace(/^E\s+/, '').trim())
    .filter(Boolean);
  if (eLines.length) return [eLines[0]];

  return [];
}

/** The single most relevant error line (first failure), or null. */
export function primaryTestError(logs: string | null | undefined): string | null {
  return extractTestErrors(logs)[0] ?? null;
}

export interface ParsedError {
  /** The original error line. */
  raw: string;
  /** Text before the embedded JSON body (or the whole line when there is none). */
  prefix: string;
  /** Pretty-printed JSON body, when the line embeds one. */
  json: string | null;
  /** Concise one-liner for collapsed display (JSON replaced by `title — detail`). */
  summary: string;
}

/**
 * Many API failures embed a problem+json body in the assertion message, e.g.
 * `… -> 500: {"detail": "...", "title": "Internal", "trace_id": "..."}`.
 * Split that into a readable prefix + pretty JSON, and build a concise summary.
 */
export function parseError(raw: string): ParsedError {
  const start = raw.indexOf('{');
  const end = raw.lastIndexOf('}');
  if (start !== -1 && end > start) {
    const candidate = raw.slice(start, end + 1);
    try {
      const obj = JSON.parse(candidate) as Record<string, unknown>;
      const prefix = raw.slice(0, start).trim();
      const title = typeof obj.title === 'string' ? obj.title : '';
      const detail = typeof obj.detail === 'string' ? obj.detail : '';
      const tail = [title, detail].filter(Boolean).join(' — ');
      const summary = (tail ? `${prefix} ${tail}` : prefix || raw).trim();
      return { raw, prefix, json: JSON.stringify(obj, null, 2), summary };
    } catch {
      // Not valid JSON — fall through to the raw form.
    }
  }
  return { raw, prefix: raw, json: null, summary: raw };
}
