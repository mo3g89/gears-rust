import { Badge } from '@/components/ui/badge';
import { cn } from '@/lib/utils';

interface SourceCellProps {
  kind: 'repository' | 'folder';
  value: string;
  className?: string;
}

const sourceStyles = {
  repository: {
    label: 'Repository',
    badgeClassName: 'bg-slate-50 text-slate-700 border-slate-200',
  },
  folder: {
    label: 'Folder',
    badgeClassName: 'bg-emerald-50 text-emerald-700 border-emerald-200',
  },
} as const;

export function SourceCell({ kind, value, className }: SourceCellProps) {
  const config = sourceStyles[kind];

  return (
    <div className={cn('flex items-center gap-2 min-w-0', className)}>
      <Badge variant="outline" className={cn('text-[11px] font-medium', config.badgeClassName)}>
        {config.label}
      </Badge>
      <span className="min-w-0 truncate text-sm font-medium text-foreground" title={value}>
        {value}
      </span>
    </div>
  );
}
