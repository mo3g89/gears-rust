import { Fragment, useState } from 'react';
import { TestResult, getStatusColor } from '@/api/types';
import { useJiraConfig } from '@/api/hooks';
import { Badge } from '@/components/ui/badge';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { extractTestErrors, parseError } from '@/lib/testError';
import { cn } from '@/lib/utils';

interface TestResultsTableProps {
  results: TestResult[];
}

/**
 * Test names are plain text rather than links: the test catalog's detail view has
 * no backend in this deployment and was removed (REMOVED-SURFACES.md, Task 8a C1).
 */
export function TestResultsTable({ results }: TestResultsTableProps) {
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const { data: jiraConfig } = useJiraConfig();
  const jiraBase = (jiraConfig?.url ?? '').trim().replace(/\/+$/, '');
  const ticketHref = (ticket: string) => (jiraBase ? `${jiraBase}/browse/${ticket}` : null);

  if (results.length === 0) {
    return (
      <div className="text-center py-8 text-muted-foreground">
        No test results available
      </div>
    );
  }

  const toggle = (index: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(index)) next.delete(index);
      else next.add(index);
      return next;
    });
  };

  const passedCount = results.filter(r => r.status === 'PASSED').length;
  const failedCount = results.filter(r => r.status === 'FAILED').length;
  const skippedCount = results.filter(r => r.status === 'SKIPPED').length;
  const errorCount = results.filter(r => r.status === 'ERROR').length;
  const runningCount = results.filter(r => r.status === 'RUNNING').length;
  const pendingCount = results.filter(r => r.status === 'PENDING').length;

  return (
    <div className="space-y-4">
      <div className="flex gap-4 text-sm">
        <div className="flex items-center gap-2">
          <Badge className="bg-green-100 text-green-800">PASSED</Badge>
          <span className="text-muted-foreground">{passedCount}</span>
        </div>
        <div className="flex items-center gap-2">
          <Badge className="bg-red-100 text-red-800">FAILED</Badge>
          <span className="text-muted-foreground">{failedCount}</span>
        </div>
        <div className="flex items-center gap-2">
          <Badge className="bg-yellow-100 text-yellow-800">SKIPPED</Badge>
          <span className="text-muted-foreground">{skippedCount}</span>
        </div>
        {runningCount > 0 && (
          <div className="flex items-center gap-2">
            <Badge className="bg-blue-100 text-blue-800">RUNNING</Badge>
            <span className="text-muted-foreground">{runningCount}</span>
          </div>
        )}
        {pendingCount > 0 && (
          <div className="flex items-center gap-2">
            <Badge className="bg-slate-100 text-slate-800">PENDING</Badge>
            <span className="text-muted-foreground">{pendingCount}</span>
          </div>
        )}
        {errorCount > 0 && (
          <div className="flex items-center gap-2">
            <Badge className="bg-red-100 text-red-800">ERROR</Badge>
            <span className="text-muted-foreground">{errorCount}</span>
          </div>
        )}
      </div>

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Test Name</TableHead>
            <TableHead>Duration</TableHead>
            <TableHead>Status</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {results.map((result, index) => {
            const isFailure = result.status === 'FAILED' || result.status === 'ERROR';
            // `logs` has no source in this deployment (CONTRACT-DIFF §8-C2), so this
            // is empty and the row shows no error detail — the run detail page says so.
            const errors = (isFailure && result.logs ? extractTestErrors(result.logs) : []).map(
              parseError
            );
            const headline = errors[0]?.summary ?? null;
            const isExpanded = expanded.has(index);
            const cases = result.cases ?? [];
            // Non-passed case counts, surfaced inline so e.g. an xfail inside an
            // otherwise-green file is visible without expanding the row.
            const caseSummary = cases.reduce<Record<string, number>>((acc, c) => {
              if (c.status !== 'PASSED') acc[c.status] = (acc[c.status] ?? 0) + 1;
              return acc;
            }, {});
            const canExpand = cases.length > 0 || errors.length > 0;

            return (
              <Fragment key={index}>
                <TableRow
                  className={cn(canExpand && 'cursor-pointer')}
                  onClick={canExpand ? () => toggle(index) : undefined}
                >
                  <TableCell className="font-medium align-top">
                    <div className="flex items-start gap-1.5">
                      {canExpand ? (
                        isExpanded ? (
                          <ChevronDown className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                        ) : (
                          <ChevronRight className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                        )
                      ) : (
                        <span className="w-3.5 shrink-0" />
                      )}
                      <div className="min-w-0">
                        <span>{result.name}</span>
                        {headline && !isExpanded && (
                          <p className="mt-0.5 line-clamp-2 font-mono text-xs font-normal text-red-600 dark:text-red-400">
                            {headline}
                            {errors.length > 1 && (
                              <span className="text-muted-foreground"> (+{errors.length - 1} more)</span>
                            )}
                          </p>
                        )}
                      </div>
                    </div>
                  </TableCell>
                  <TableCell className="whitespace-nowrap text-muted-foreground align-top">
                    {result.duration || '-'}
                  </TableCell>
                  <TableCell className="align-top">
                    <div className="flex flex-wrap items-center gap-1">
                      <Badge className={getStatusColor(result.status)}>
                        {result.status}
                      </Badge>
                      {['FAILED', 'ERROR', 'XFAIL', 'XPASS', 'SKIPPED']
                        .filter((s) => caseSummary[s] && s !== result.status)
                        .map((s) => (
                          <Badge
                            key={s}
                            className={`${getStatusColor(s)} text-[10px]`}
                            title={`${caseSummary[s]} ${s} test${caseSummary[s] > 1 ? 's' : ''} — expand for details`}
                          >
                            {caseSummary[s]} {s}
                          </Badge>
                        ))}
                    </div>
                  </TableCell>
                </TableRow>
                {isExpanded && (
                  <TableRow className="bg-muted/30 hover:bg-muted/30">
                    <TableCell colSpan={3} className="space-y-3 py-3">
                      {cases.length > 0 && (
                        <div className="space-y-1.5">
                          {cases.map((c, i) => {
                            const href = c.ticket ? ticketHref(c.ticket) : null;
                            return (
                              <div key={i} className="flex flex-wrap items-center gap-2 text-xs">
                                <Badge className={getStatusColor(c.status)}>{c.status}</Badge>
                                <span className="font-mono text-foreground/80">{c.name}</span>
                                {c.duration && (
                                  <span className="text-muted-foreground">{c.duration}</span>
                                )}
                                {c.ticket &&
                                  (href ? (
                                    <a
                                      href={href}
                                      target="_blank"
                                      rel="noreferrer"
                                      className="font-medium text-blue-700 hover:underline dark:text-blue-300"
                                    >
                                      {c.ticket}
                                    </a>
                                  ) : (
                                    <Badge variant="outline" className="text-[10px] uppercase tracking-wide">
                                      {c.ticket}
                                    </Badge>
                                  ))}
                                {c.reason && (
                                  <span className="truncate text-muted-foreground" title={c.reason}>
                                    {c.reason}
                                  </span>
                                )}
                              </div>
                            );
                          })}
                        </div>
                      )}
                      {errors.length > 0 && (
                        <ul className="space-y-3">
                          {errors.map((err, i) => (
                            <li key={i} className="space-y-1">
                              <p className="whitespace-pre-wrap break-words font-mono text-xs font-medium text-red-600 dark:text-red-400">
                                {err.prefix}
                              </p>
                              {err.json && (
                                <pre className="overflow-x-auto rounded-md border bg-background p-2 font-mono text-xs text-muted-foreground">
                                  {err.json}
                                </pre>
                              )}
                            </li>
                          ))}
                        </ul>
                      )}
                    </TableCell>
                  </TableRow>
                )}
              </Fragment>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}
