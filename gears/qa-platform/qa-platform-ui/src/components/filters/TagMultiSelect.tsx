import { useEffect, useMemo, useRef, useState } from 'react';
import { X } from 'lucide-react';

interface TagMultiSelectProps {
  value: string[];
  onChange: (tags: string[]) => void;
  /** Known tags offered for selection. */
  suggestions?: string[];
  placeholder?: string;
  id?: string;
}

/**
 * Chips-style multi-select for tags. Click to open a dropdown of existing tags
 * and pick with the mouse, or type to filter / add a free-form tag (Enter or
 * comma). Click a chip's × to remove.
 */
export function TagMultiSelect({ value, onChange, suggestions = [], placeholder, id }: TagMultiSelectProps) {
  const [draft, setDraft] = useState('');
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handle = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', handle);
    return () => document.removeEventListener('mousedown', handle);
  }, [open]);

  const add = (raw: string) => {
    const tag = raw.trim();
    if (!tag) return;
    if (!value.some((v) => v.toLowerCase() === tag.toLowerCase())) {
      onChange([...value, tag]);
    }
    setDraft('');
  };

  const remove = (tag: string) => onChange(value.filter((v) => v !== tag));

  const normalized = draft.trim().toLowerCase();
  const available = useMemo(
    () =>
      suggestions.filter(
        (s) =>
          !value.some((v) => v.toLowerCase() === s.toLowerCase()) &&
          s.toLowerCase().includes(normalized)
      ),
    [suggestions, value, normalized]
  );
  const showAddRow =
    draft.trim().length > 0 && !suggestions.some((s) => s.toLowerCase() === normalized);

  return (
    <div ref={rootRef} className="relative">
      <div
        className="flex flex-wrap items-center gap-1.5 rounded-md border border-input bg-background px-2 py-1.5"
        onClick={() => setOpen(true)}
      >
        {value.map((tag) => (
          <span
            key={tag}
            className="inline-flex items-center gap-1 rounded-full bg-accent px-2 py-0.5 text-xs text-accent-foreground"
          >
            {tag}
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                remove(tag);
              }}
              className="text-muted-foreground hover:text-foreground"
              aria-label={`Remove ${tag}`}
            >
              <X className="h-3 w-3" />
            </button>
          </span>
        ))}
        <input
          id={id}
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ',') {
              e.preventDefault();
              add(draft);
            } else if (e.key === 'Backspace' && !draft && value.length > 0) {
              remove(value[value.length - 1]);
            } else if (e.key === 'Escape') {
              setOpen(false);
            }
          }}
          placeholder={value.length === 0 ? placeholder : ''}
          autoComplete="off"
          className="min-w-[80px] flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
        />
      </div>

      {open && (available.length > 0 || showAddRow) && (
        <div className="absolute z-50 mt-1 max-h-52 w-full overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md">
          {available.map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => add(s)}
              className="block w-full rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent hover:text-accent-foreground"
            >
              {s}
            </button>
          ))}
          {showAddRow && (
            <button
              type="button"
              onClick={() => add(draft)}
              className="block w-full rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent hover:text-accent-foreground"
            >
              Add “<span className="font-medium">{draft.trim()}</span>”
            </button>
          )}
        </div>
      )}
    </div>
  );
}
