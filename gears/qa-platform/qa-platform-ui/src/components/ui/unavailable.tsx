import type { ReactNode } from 'react';
import { CircleSlash } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * The one shared "this deployment does not serve it" treatment.
 *
 * Every surface the backend cannot feed uses this component rather than its own
 * bespoke empty state, so a reader learns the visual once: a muted, dashed-border
 * block that states what is missing and — where it helps — what is still true.
 * It exists because five different "unavailable" designs across five surfaces read
 * as five different bugs.
 *
 * Two sizes, one design:
 * - `panel` (default) sits inside a card or a panel body and replaces the content
 *   that field would have filled;
 * - `page` sits at the top of a page and qualifies everything below it.
 *
 * Do not use this to label a control that still accepts input and still sends it:
 * where the input itself is the misrepresentation the affordance is removed, not
 * labelled (see `REMOVED-SURFACES.md`).
 */
export function UnavailableNotice({
  title,
  children,
  size = 'panel',
  className,
}: {
  /** What is unavailable, as a statement — e.g. "Cluster health is not available in this deployment". */
  title: string;
  /** Optional detail: why nothing is shown, and what the reader can still rely on. */
  children?: ReactNode;
  size?: 'panel' | 'page';
  className?: string;
}) {
  return (
    <div
      className={cn(
        'flex items-start gap-3 rounded-md border border-dashed bg-muted/30 text-muted-foreground',
        size === 'page' ? 'px-4 py-3' : 'px-3 py-2.5',
        className
      )}
      role="note"
    >
      <CircleSlash
        className={cn('mt-0.5 shrink-0', size === 'page' ? 'h-4 w-4' : 'h-3.5 w-3.5')}
        aria-hidden
      />
      <div className="min-w-0 space-y-1">
        <p
          className={cn(
            'font-medium text-foreground/80',
            size === 'page' ? 'text-sm' : 'text-xs'
          )}
        >
          {title}
        </p>
        {children ? (
          <div className={cn('leading-relaxed', size === 'page' ? 'text-sm' : 'text-xs')}>
            {children}
          </div>
        ) : null}
      </div>
    </div>
  );
}

/**
 * Tooltip/aria text for a control that is left visible but disabled, so a disabled
 * button says the same thing as the notice above rather than inventing its own words.
 */
export function unavailableTitle(what: string): string {
  return `${what} is not available in this deployment`;
}
