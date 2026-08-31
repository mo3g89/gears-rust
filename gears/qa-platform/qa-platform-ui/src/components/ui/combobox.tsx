import * as React from 'react';
import { Check, ChevronDown, Search } from 'lucide-react';
import { cn } from '@/lib/utils';

export type ComboboxOption = { value: string; label: string };

export interface ComboboxProps {
  value: string;
  onChange: (value: string) => void;
  /** Options as plain strings (value === label) or `{ value, label }` pairs. */
  options: Array<string | ComboboxOption>;
  placeholder?: string;
  /** Allow committing a typed value that is not in `options` (e.g. a branch
   *  that exists on the remote but isn't in the cached list yet). */
  allowCustom?: boolean;
  disabled?: boolean;
  className?: string;
  /** Extra classes merged onto the trigger button (e.g. to shrink its height
   *  to match a compact toolbar of `Select`s). */
  triggerClassName?: string;
  id?: string;
  /** Text shown in the dropdown when there are no matches. */
  emptyText?: string;
  /** Fired when an option is hovered/focused — useful for prefetching. */
  onOptionHover?: (value: string) => void;
}

function normalizeOption(opt: string | ComboboxOption): ComboboxOption {
  return typeof opt === 'string' ? { value: opt, label: opt } : opt;
}

/**
 * Searchable single-select (combobox) styled to match the app's Select.
 * Self-contained — no extra deps — built on a button trigger + a filtered
 * popover list with a search input and click-outside handling.
 */
export function Combobox({
  value,
  onChange,
  options,
  placeholder = 'Select…',
  allowCustom = false,
  disabled = false,
  className,
  triggerClassName,
  id,
  emptyText = 'No matches',
  onOptionHover,
}: ComboboxProps) {
  const [open, setOpen] = React.useState(false);
  const [search, setSearch] = React.useState('');
  const rootRef = React.useRef<HTMLDivElement>(null);
  const inputRef = React.useRef<HTMLInputElement>(null);

  const items = React.useMemo(() => options.map(normalizeOption), [options]);

  React.useEffect(() => {
    if (!open) return;
    const handle = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', handle);
    return () => document.removeEventListener('mousedown', handle);
  }, [open]);

  React.useEffect(() => {
    if (open) {
      setSearch('');
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  const normalized = search.trim().toLowerCase();
  const filtered = React.useMemo(
    () =>
      items.filter(
        (opt) =>
          opt.label.toLowerCase().includes(normalized) ||
          opt.value.toLowerCase().includes(normalized)
      ),
    [items, normalized]
  );

  const selected = items.find((opt) => opt.value === value);
  const triggerLabel = selected?.label ?? (value || '');

  const showCustomRow =
    allowCustom &&
    search.trim().length > 0 &&
    !items.some((opt) => opt.value.toLowerCase() === normalized);

  const commit = (next: string) => {
    onChange(next);
    setOpen(false);
  };

  return (
    <div ref={rootRef} className={cn('relative', className)}>
      <button
        type="button"
        id={id}
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
        className={cn(
          'flex h-9 w-full items-center justify-between rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50',
          triggerClassName
        )}
      >
        <span className={cn('line-clamp-1 text-left', !triggerLabel && 'text-muted-foreground')}>
          {triggerLabel || placeholder}
        </span>
        <ChevronDown className="h-4 w-4 shrink-0 opacity-50" />
      </button>

      {open && (
        <div className="absolute z-50 mt-1 w-full overflow-hidden rounded-md border bg-popover text-popover-foreground shadow-md">
          <div className="flex items-center gap-2 border-b px-2">
            <Search className="h-4 w-4 shrink-0 text-muted-foreground" />
            <input
              ref={inputRef}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Escape') {
                  setOpen(false);
                } else if (e.key === 'Enter') {
                  e.preventDefault();
                  if (filtered.length > 0) commit(filtered[0].value);
                  else if (showCustomRow) commit(search.trim());
                }
              }}
              placeholder="Search…"
              className="h-9 w-full bg-transparent py-2 text-sm outline-none placeholder:text-muted-foreground"
            />
          </div>
          <div className="max-h-60 overflow-y-auto p-1">
            {filtered.length === 0 && !showCustomRow ? (
              <div className="px-2 py-2 text-sm text-muted-foreground">{emptyText}</div>
            ) : (
              filtered.map((opt) => (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() => commit(opt.value)}
                  onMouseEnter={() => onOptionHover?.(opt.value)}
                  onFocus={() => onOptionHover?.(opt.value)}
                  className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent hover:text-accent-foreground"
                >
                  <Check className={cn('h-4 w-4 shrink-0', opt.value === value ? 'opacity-100' : 'opacity-0')} />
                  <span className="line-clamp-1">{opt.label}</span>
                </button>
              ))
            )}
            {showCustomRow && (
              <button
                type="button"
                onClick={() => commit(search.trim())}
                className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent hover:text-accent-foreground"
              >
                <Check className="h-4 w-4 shrink-0 opacity-0" />
                <span className="line-clamp-1">
                  Use “<span className="font-medium">{search.trim()}</span>”
                </span>
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
