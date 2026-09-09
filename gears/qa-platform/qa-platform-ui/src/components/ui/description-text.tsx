import { useState } from 'react';
import { cn } from '@/lib/utils';

interface DescriptionTextProps {
  text: string;
  className?: string;
  /** When true, collapse to a single line with a "…" expander. */
  collapsible?: boolean;
}

type Block = { type: 'ul'; items: string[] } | { type: 'p'; text: string };

/** Lightweight formatter for plan/test descriptions: preserves line breaks,
 *  breaks inline "* " bullets onto their own lines, and renders bullet runs as
 *  lists — without pulling in a full Markdown dependency. */
export function DescriptionText({ text, className, collapsible }: DescriptionTextProps) {
  const [open, setOpen] = useState(false);

  const normalized = text
    .replace(/\r\n/g, '\n')
    .replace(/\s+\*\s+/g, '\n* ')
    .replace(/\s+•\s+/g, '\n* ');

  const blocks: Block[] = [];
  for (const line of normalized.split('\n')) {
    const t = line.trim();
    if (!t) continue;
    const bullet = t.match(/^[-*•]\s+(.*)$/);
    if (bullet) {
      const last = blocks[blocks.length - 1];
      if (last && last.type === 'ul') last.items.push(bullet[1]);
      else blocks.push({ type: 'ul', items: [bullet[1]] });
    } else {
      blocks.push({ type: 'p', text: t });
    }
  }

  if (blocks.length === 0) return null;

  // Collapsed: a single truncated line with a "…" expander.
  if (collapsible && !open) {
    const oneLine = text.replace(/\s+/g, ' ').trim();
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        title="Expand"
        className={cn('flex w-full items-start gap-1 text-left text-sm leading-relaxed', className)}
      >
        <span className="line-clamp-1 min-w-0 flex-1">{oneLine}</span>
        <span className="shrink-0 text-muted-foreground">…</span>
      </button>
    );
  }

  const content = (
    <div className={cn('space-y-2 text-sm leading-relaxed', className)}>
      {blocks.map((b, i) =>
        b.type === 'ul' ? (
          <ul key={i} className="list-disc space-y-1 pl-5">
            {b.items.map((it, j) => (
              <li key={j}>{it}</li>
            ))}
          </ul>
        ) : (
          <p key={i}>{b.text}</p>
        )
      )}
    </div>
  );

  if (!collapsible) return content;

  return (
    <div>
      {content}
      <button
        type="button"
        onClick={() => setOpen(false)}
        className="mt-1 text-xs font-medium text-primary hover:underline"
      >
        Show less
      </button>
    </div>
  );
}
