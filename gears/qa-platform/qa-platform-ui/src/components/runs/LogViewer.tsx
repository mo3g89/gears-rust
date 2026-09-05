import { useEffect, useMemo, useRef, useState } from 'react';
import { useRunLogStream } from '@/hooks/useRunLogStream';
import { useRunLogs } from '@/api/hooks';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { cn } from '@/lib/utils';
import { Download, Loader2, ArrowDownToLine, WrapText, Search, ChevronRight, Maximize2, Minimize2 } from 'lucide-react';

interface LogViewerProps {
  runName: string;
  /**
   * The run has reached a terminal state, so its log is complete and archived.
   *
   * A terminal run is fetched **once** as a whole body instead of streamed, which is
   * exactly what legacy did (`manager/src/routes/runs.rs`'s `api_logs` returns the full
   * text; `manager-ui`'s `useRunLogs` fetches it and splits it once). Replaying a
   * finished 22k-line log through `EventSource` re-ran the join/split/`buildBlocks`
   * chain once per line for no benefit — the content cannot change again.
   *
   * Optional and defaulting to `false` so the streaming path stays the default for any
   * caller that does not know the run's state.
   */
  isTerminal?: boolean;
}

type Severity = 'pass' | 'fail' | 'warn' | 'skip' | 'section' | 'internal' | 'info';
type BlockStatus = 'pass' | 'fail' | 'skip' | 'running' | 'setup';

interface LogLine {
  i: number;
  raw: string;
  sev: Severity;
  internal: boolean;
}

interface LogBlock {
  id: string;
  title: string;
  status: BlockStatus;
  lines: LogLine[];
  hasFail: boolean;
}

// Runner markers that bracket a single test's output — consumed into block
// headers rather than rendered as body lines.
const FILE_START_RE = /^===\s*TEST_FILE:\s*(.+?)\s*===\s*$/;
const RESULT_RE = /^===\s*TEST_RESULT:\s*(.+?)\s+(PASSED|FAILED|ERROR|SKIPPED)\b/;

// OUR RUNNER SPEAKS A DIFFERENT PROTOCOL THAN LEGACY'S, and this file was
// copied from legacy before that was true. Legacy runs pytest once per file in
// a shell loop and brackets each with `=== TEST_FILE: ... ===` /
// `=== TEST_RESULT: <title> PASSED ===`, which is what FILE_START_RE and
// RESULT_RE above split on. Ours runs pytest ONCE over the whole bundle and
// emits one `=== TEST_CASE: <base64 json> ===` per test from
// deploy/runner/pytest_markers.py -- and NO TEST_RESULT line at all.
//
// Measured on a real run (am-validation-smoke-1): 2 TEST_FILE markers, 0
// TEST_RESULT, 19 TEST_CASE. With only the legacy patterns, nothing ever
// closed a block, so every test's output piled into one section instead of
// getting its own -- the symptom this handles.
//
// The payload is
//   {nodeid, file, name, outcome, duration, reason}
// and `outcome` is pytest's own lowercase vocabulary, not legacy's upper-case.
const TEST_CASE_RE = /^===\s*TEST_CASE:\s*(\S+)\s*===\s*$/;

// EVERY LINE ARRIVES PREFIXED WITH ITS CONTAINER, and all three marker
// patterns above are ^-anchored, so without stripping it none of them ever
// match. qa-runs multiplexes the workflow pod's containers into one stream and
// tags each line, so what the browser actually receives is
//
//   [repo-d4addf78-9b4d-49d8-940d-b1aef6d27781] === TEST_CASE: <base64> ===
//
// not the bare marker the runner wrote. Measured against the live SSE stream
// for run authentication-1: 30 TEST_CASE markers present, 0 matched.
//
// This is also why the source system's TEST_FILE/TEST_RESULT patterns would
// not have grouped anything here either -- the mismatch is the prefix, not
// just the protocol.
//
// Only the marker TEST is stripped; `raw` keeps the prefix, because which
// container emitted a line is real information when reading a failure.
const CONTAINER_PREFIX_RE = /^\[[^\]]+\]\s+/;

function markerText(raw: string): string {
  return raw.replace(CONTAINER_PREFIX_RE, '');
}

type TestCaseMarker = { name: string; file: string; outcome: string; nodeid: string };

// Never throws: a marker we cannot decode falls through and is rendered as an
// ordinary log line, which is strictly better than losing the whole viewer to
// one malformed token.
function decodeTestCase(token: string): TestCaseMarker | null {
  try {
    const json = JSON.parse(atob(token)) as Partial<TestCaseMarker> | null;
    if (!json || typeof json !== 'object') return null;
    const name = typeof json.name === 'string' ? json.name : '';
    const file = typeof json.file === 'string' ? json.file : '';
    const outcome = typeof json.outcome === 'string' ? json.outcome : '';
    const nodeid = typeof json.nodeid === 'string' ? json.nodeid : '';
    if (!name && !file) return null;
    return { name, file, outcome, nodeid };
  } catch {
    return null;
  }
}

// pytest's vocabulary, deliberately not legacy's. An unknown outcome is a
// failure rather than a pass: a test whose result we cannot read must not show
// a green dot.
function statusForOutcome(outcome: string): BlockStatus {
  switch (outcome) {
    case 'passed':
      return 'pass';
    case 'skipped':
      return 'skip';
    default:
      return 'fail';
  }
}

// Internal scaffolding the runner prints for the manager's benefit — hidden
// behind the "Show internal" toggle.
function isInternalLine(raw: string): boolean {
  return (
    /^===\s*(TEST_DISCOVERED|TEST_LAUNCH_ID|TEST_START|TEST_FILES|ATTRIBUTES|Tests root|Running test plan|Running specific test files)\b/.test(raw) ||
    /^DEBUG:/.test(raw) ||
    /^=== (Tests with known bugs|Coverage)/.test(raw) ||
    /^rp_(endpoint|project|launch|api_key|client_type)/.test(raw)
  );
}

function classify(raw: string): Severity {
  if (isInternalLine(raw)) return 'internal';
  const t = raw.trim();
  if (/^=+\s.*\s=+$/.test(t)) return 'section';
  if (/\b(FAILED|ERROR)\b/.test(raw) || /^E\s/.test(raw) || /Traceback/.test(raw)) return 'fail';
  if (/\bPASSED\b/.test(raw)) return 'pass';
  if (/\bWARNING\b/i.test(raw) || /warnings summary/i.test(raw)) return 'warn';
  if (/\b(SKIPPED|xfailed|xpassed|xfail|xpass)\b/i.test(raw)) return 'skip';
  return 'info';
}

function sevClass(sev: Severity): string {
  switch (sev) {
    case 'pass':
      return 'text-emerald-600 dark:text-emerald-400';
    case 'fail':
      return 'text-red-600 dark:text-red-400';
    case 'warn':
      return 'text-amber-600 dark:text-amber-400';
    case 'skip':
      return 'text-yellow-600 dark:text-yellow-500';
    case 'section':
      return 'font-semibold text-foreground';
    case 'internal':
      return 'text-muted-foreground/50';
    default:
      return 'text-foreground/80';
  }
}

function FilterChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'rounded-full border px-2.5 py-1 text-[11px] transition-colors',
        active
          ? 'border-primary/50 bg-primary/15 text-primary'
          : 'border-border bg-muted/30 text-muted-foreground hover:bg-muted/60'
      )}
    >
      {children}
    </button>
  );
}

function blockDotClass(status: BlockStatus): string {
  switch (status) {
    case 'pass':
      return 'bg-emerald-500';
    case 'fail':
      return 'bg-red-500';
    case 'skip':
      return 'bg-yellow-500';
    case 'running':
      return 'bg-blue-500';
    default:
      return 'bg-muted-foreground/40';
  }
}

// Exported for tests only: the grouping is the whole point of this component
// and it is driven by markers the runner emits, so it must be provable against
// real marker text rather than by rendering and eyeballing.
export function buildBlocks(logLines: string[]): LogBlock[] {
  const blocks: LogBlock[] = [];
  let cur: LogBlock = { id: 'setup', title: 'Run setup', status: 'setup', lines: [], hasFail: false };

  logLines.forEach((raw, i) => {
    const fm = FILE_START_RE.exec(markerText(raw));
    if (fm) {
      blocks.push(cur);
      cur = { id: `f${i}`, title: fm[1], status: 'running', lines: [], hasFail: false };
      return;
    }
    const rm = RESULT_RE.exec(markerText(raw));
    if (rm && cur.status === 'running') {
      cur.status = rm[2] === 'PASSED' ? 'pass' : rm[2] === 'SKIPPED' ? 'skip' : 'fail';
      blocks.push(cur);
      cur = { id: `o${i}`, title: 'Output', status: 'setup', lines: [], hasFail: false };
      return;
    }
    // Our runner's marker arrives AFTER the test it describes -- it is a
    // pytest report hook, so there is no start marker to open a block with.
    // The block that has been accumulating this test's output is therefore
    // retitled and closed here, which produces the same shape legacy gets from
    // its START/RESULT pair: one collapsible section per test, carrying that
    // test's output, with a status dot.
    const cm = TEST_CASE_RE.exec(markerText(raw));
    if (cm) {
      const meta = decodeTestCase(cm[1]);
      if (meta) {
        // THE FIRST TEST IS THE AMBIGUOUS ONE. Because the marker follows the
        // test it describes and there is no start marker, the lines before it
        // are simply "everything so far" -- which for the first test means the
        // run's setup AND that test's own output in one undivided run.
        //
        // pytest's progress line names the nodeid, and the marker carries the
        // same nodeid, so where the suite runs verbosely we can find exactly
        // where setup stopped and the first test began, and split there.
        // Without -v there is no such line, and the lines stay under "Run
        // setup" rather than being silently attributed to a test they may not
        // belong to.
        if (cur.id === 'setup' && blocks.length === 0 && meta.nodeid) {
          const at = cur.lines.findIndex((l) => l.raw.includes(meta.nodeid));
          if (at >= 0) {
            const testLines = cur.lines.slice(at);
            cur.lines = cur.lines.slice(0, at);
            blocks.push(cur);
            cur = {
              id: `c${i}`,
              title: meta.name || meta.file,
              status: statusForOutcome(meta.outcome),
              lines: testLines,
              hasFail: false,
            };
            blocks.push(cur);
            cur = { id: `o${i}`, title: 'Output', status: 'setup', lines: [], hasFail: false };
            return;
          }
          blocks.push(cur);
          cur = {
            id: `c${i}`,
            title: meta.name || meta.file,
            status: statusForOutcome(meta.outcome),
            lines: [],
            hasFail: false,
          };
          blocks.push(cur);
          cur = { id: `o${i}`, title: 'Output', status: 'setup', lines: [], hasFail: false };
          return;
        }
        cur.title = meta.name || meta.file || cur.title;
        cur.status = statusForOutcome(meta.outcome);
        blocks.push(cur);
        cur = { id: `o${i}`, title: 'Output', status: 'setup', lines: [], hasFail: false };
        return;
      }
    }
    cur.lines.push({ i, raw, sev: classify(raw), internal: isInternalLine(raw) });
  });
  blocks.push(cur);

  // A resolved test is worth a section even with no output -- that is the
  // common case for a passing test. Anything unresolved and blank is noise:
  // notably the `=== TEST_FILE: ===` markers our entrypoint emits up front for
  // every requested path, which would otherwise leave a row of empty sections
  // above the real ones.
  return blocks
    .filter(
      (b) =>
        b.status === 'pass' ||
        b.status === 'fail' ||
        b.status === 'skip' ||
        b.lines.some((l) => l.raw.trim() !== '')
    )
    .map((b) => ({ ...b, hasFail: b.lines.some((l) => l.sev === 'fail') }));
}

export function LogViewer({ runName, isTerminal = false }: LogViewerProps) {
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const wasNearBottomRef = useRef(true);
  const previousLogsRef = useRef('');
  const [autoScroll, setAutoScroll] = useState(true);
  const [wrapLines, setWrapLines] = useState(true);
  const [hasUnreadUpdates, setHasUnreadUpdates] = useState(false);

  // Readability controls.
  const [query, setQuery] = useState('');
  const [failuresOnly, setFailuresOnly] = useState(false);
  const [hidePassed, setHidePassed] = useState(false);
  const [hideWarnings, setHideWarnings] = useState(false);
  const [showInternal, setShowInternal] = useState(false);
  const [manualOpen, setManualOpen] = useState<Record<string, boolean>>({});
  const [fullscreen, setFullscreen] = useState(false);

  // Esc exits fullscreen.
  useEffect(() => {
    if (!fullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setFullscreen(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [fullscreen]);

  // `null` keeps the hook from opening an `EventSource` at all for a terminal run — the
  // archived text comes from the single fetch below instead.
  const { isConnected, messages: wsMessages, hasGivenUp } = useRunLogStream(
    isTerminal ? null : runName
  );
  // Review round 1, finding 1: once the hook gives up (a finished run's empty stream, a
  // capped run, or a genuine lookup failure — see `useRunLogStream`'s own doc), this
  // switches the header below to the honest "Polling" branch instead of a permanent
  // "Connecting..." spinner that never resolves.
  const useWs = !isTerminal && !hasGivenUp;

  // A terminal run's log is immutable, so the 5s poll is turned off rather than left to
  // re-fetch and re-parse the same multi-megabyte body forever.
  const { data: polledLogs, isLoading } = useRunLogs(runName, { live: !isTerminal });

  const wsLogs = wsMessages.join('\n');
  const logs = useWs ? (wsLogs || polledLogs || '') : polledLogs || '';

  const logLines = useMemo(() => (logs ? logs.split('\n') : []), [logs]);
  const lineCount = logLines.length;

  const blocks = useMemo(() => buildBlocks(logLines), [logLines]);
  const failCount = useMemo(
    () => blocks.reduce((n, b) => n + b.lines.filter((l) => l.sev === 'fail').length, 0),
    [blocks]
  );

  const q = query.trim().toLowerCase();
  const filtersActive = !!q || hidePassed || hideWarnings || failuresOnly;

  const lineVisible = (l: LogLine): boolean => {
    if (!showInternal && l.internal) return false;
    if (q && !l.raw.toLowerCase().includes(q)) return false;
    if (failuresOnly && l.sev !== 'fail') return false;
    if (hidePassed && l.sev === 'pass') return false;
    if (hideWarnings && l.sev === 'warn') return false;
    return true;
  };

  // Re-apply auto open/collapse defaults whenever the active view changes.
  useEffect(() => {
    setManualOpen({});
  }, [query, failuresOnly, hidePassed, hideWarnings, showInternal]);

  const jumpToLatest = () => {
    const container = scrollContainerRef.current;
    if (!container) return;
    container.scrollTop = container.scrollHeight;
    wasNearBottomRef.current = true;
    setHasUnreadUpdates(false);
  };

  const handleScroll = () => {
    const container = scrollContainerRef.current;
    if (!container) return;
    const distanceFromBottom = container.scrollHeight - container.scrollTop - container.clientHeight;
    const nearBottom = distanceFromBottom <= 24;
    wasNearBottomRef.current = nearBottom;
    if (nearBottom) setHasUnreadUpdates(false);
  };

  useEffect(() => {
    const container = scrollContainerRef.current;
    if (!container) return;
    if (logs === previousLogsRef.current) return;
    previousLogsRef.current = logs;
    if (autoScroll && wasNearBottomRef.current) {
      requestAnimationFrame(() => jumpToLatest());
      return;
    }
    setHasUnreadUpdates(true);
  }, [logs, autoScroll]);

  const handleDownload = () => {
    const blob = new Blob([logs], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${runName}-logs.txt`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  };

  const renderedBlocks = blocks
    .map((block) => ({ block, visible: block.lines.filter(lineVisible) }))
    .filter(({ visible }) => !filtersActive || visible.length > 0);

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Logs</CardTitle>
            <CardDescription>
              {useWs ? (
                <span className="flex items-center gap-2">
                  {isConnected ? (
                    <>
                      <Badge variant="default" className="bg-green-100 text-green-800">
                        Live
                      </Badge>
                      Live stream connected
                    </>
                  ) : (
                    <>
                      <Loader2 className="h-3 w-3 animate-spin" />
                      Connecting...
                    </>
                  )}
                </span>
              ) : (
                <span className="flex items-center gap-2">
                  <Badge variant="secondary">Polling</Badge>
                  Refreshing every 5 seconds
                </span>
              )}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setAutoScroll((current) => {
                  const next = !current;
                  if (next) requestAnimationFrame(() => jumpToLatest());
                  return next;
                });
              }}
            >
              Auto-follow: {autoScroll ? 'ON' : 'OFF'}
            </Button>
            <Button variant="outline" size="sm" onClick={() => setWrapLines((c) => !c)}>
              <WrapText className="h-4 w-4 mr-2" />
              Wrap: {wrapLines ? 'ON' : 'OFF'}
            </Button>
            <Button variant="outline" size="sm" onClick={handleDownload}>
              <Download className="h-4 w-4 mr-2" />
              Download
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        <div className={cn(fullscreen && 'fixed inset-0 z-50 flex flex-col bg-background p-4')}>
        {/* Readability toolbar */}
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <div className="relative min-w-[200px] flex-1">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search logs..."
              className="h-8 pl-8 text-xs"
            />
          </div>
          <FilterChip active={failuresOnly} onClick={() => setFailuresOnly((v) => !v)}>
            Failures only{failCount > 0 ? ` (${failCount})` : ''}
          </FilterChip>
          <FilterChip active={hidePassed} onClick={() => setHidePassed((v) => !v)}>
            Hide passed
          </FilterChip>
          <FilterChip active={hideWarnings} onClick={() => setHideWarnings((v) => !v)}>
            Hide warnings
          </FilterChip>
          <FilterChip active={showInternal} onClick={() => setShowInternal((v) => !v)}>
            Show internal
          </FilterChip>
        </div>

        <div className="mb-3 flex items-center justify-between text-xs text-muted-foreground">
          <span>{lineCount.toLocaleString()} lines</span>
          <span>{logs.length.toLocaleString()} chars</span>
        </div>

        <div className={cn('overflow-hidden rounded-lg border bg-muted/20', fullscreen && 'flex min-h-0 flex-1 flex-col')}>
          <div className="flex items-center justify-between border-b bg-muted/40 px-4 py-2.5">
            <div className="flex items-center gap-2">
              <span className="h-2.5 w-2.5 rounded-full bg-border" />
              <span className="h-2.5 w-2.5 rounded-full bg-border" />
              <span className="h-2.5 w-2.5 rounded-full bg-border" />
            </div>
            <div className="text-[11px] font-medium uppercase tracking-[0.24em] text-muted-foreground">
              Run Log Stream
            </div>
            <div className="flex items-center gap-2">
              <span className="rounded-full border bg-background/80 px-2.5 py-1 text-[10px] uppercase tracking-[0.22em] text-muted-foreground">
                {useWs && isConnected ? 'Live Feed' : 'Replay'}
              </span>
              <button
                type="button"
                onClick={() => setFullscreen((v) => !v)}
                title={fullscreen ? 'Exit fullscreen (Esc)' : 'Expand to fullscreen'}
                aria-label={fullscreen ? 'Exit fullscreen' : 'Expand to fullscreen'}
                className="rounded-md p-1 text-muted-foreground hover:bg-muted/60 hover:text-foreground"
              >
                {fullscreen ? <Minimize2 className="h-3.5 w-3.5" /> : <Maximize2 className="h-3.5 w-3.5" />}
              </button>
            </div>
          </div>
          <div
            ref={scrollContainerRef}
            onScroll={handleScroll}
            className={cn(
              'relative overflow-auto bg-background p-3 font-mono text-xs leading-5 text-foreground',
              fullscreen ? 'min-h-0 flex-1' : 'h-[500px]'
            )}
          >
            {hasUnreadUpdates && (
              <div className="sticky top-2 z-10 mb-3 flex justify-end">
                <Button
                  size="sm"
                  variant="secondary"
                  className="h-7 gap-1 rounded-full px-3 text-[11px]"
                  onClick={jumpToLatest}
                >
                  <ArrowDownToLine className="h-3.5 w-3.5" />
                  Jump to latest
                </Button>
              </div>
            )}
            {isLoading && !logs ? (
              <div className="flex h-full items-center justify-center">
                <Loader2 className="h-6 w-6 animate-spin" />
              </div>
            ) : !logs ? (
              <div className="flex h-full items-center justify-center text-muted-foreground">
                No logs available
              </div>
            ) : renderedBlocks.length === 0 ? (
              <div className="flex h-full items-center justify-center text-muted-foreground">
                No lines match the current filter
              </div>
            ) : (
              <div className="space-y-1.5">
                {renderedBlocks.map(({ block, visible }) => {
                  const computedOpen = filtersActive
                    ? visible.length > 0
                    : block.status === 'fail' || block.hasFail;
                  const isOpen = block.id in manualOpen ? manualOpen[block.id] : computedOpen;
                  return (
                    <div
                      key={block.id}
                      className="overflow-hidden rounded-md border border-border/60 bg-muted/10"
                    >
                      <button
                        type="button"
                        onClick={() => setManualOpen((m) => ({ ...m, [block.id]: !isOpen }))}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-muted/40"
                      >
                        <ChevronRight
                          className={cn(
                            'h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform',
                            isOpen && 'rotate-90'
                          )}
                        />
                        <span className={cn('h-2 w-2 shrink-0 rounded-full', blockDotClass(block.status))} />
                        <span className="truncate text-xs font-medium">{block.title || 'Output'}</span>
                        <span className="ml-auto shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground">
                          {block.status !== 'setup' && block.status !== 'running'
                            ? `${block.status} · `
                            : ''}
                          {visible.length} lines
                        </span>
                      </button>
                      {isOpen && (
                        <div className="border-t border-border/40 py-1">
                          {visible.map((l) => (
                            <div
                              key={l.i}
                              className="grid grid-cols-[3rem,minmax(0,1fr)] items-start gap-3 px-3 py-0.5 hover:bg-muted/40"
                            >
                              <span className="select-none pr-1 text-right text-[10px] tabular-nums text-muted-foreground">
                                {l.i + 1}
                              </span>
                              <span
                                className={cn(
                                  'min-w-0',
                                  wrapLines ? 'whitespace-pre-wrap break-words' : 'whitespace-pre',
                                  sevClass(l.sev)
                                )}
                              >
                                {l.raw || ' '}
                              </span>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </div>
        </div>
      </CardContent>
    </Card>
  );
}
