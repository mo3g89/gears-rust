import { useEffect, useMemo, useRef, useState } from 'react';
import { Input } from '@/components/ui/input';
import { Button } from '@/components/ui/button';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { X } from 'lucide-react';
import { migratePrefixedKeys } from '@/lib/storageKeys';

interface FqlQueryInputProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  fields: string[];
  valueSuggestions?: Record<string, string[]>;
  savedFiltersKey?: string;
}

type SuggestionKind = 'field' | 'operator' | 'value' | 'keyword';

interface Suggestion {
  label: string;
  insertText: string;
  replaceFrom: number;
  replaceTo: number;
  kind: SuggestionKind;
}

interface SavedFilterItem {
  id: string;
  name: string;
  query: string;
  updatedAt: number;
}

const OPERATORS = ['=', '!=', '~', '!~', '>', '>=', '<', '<=', 'in', 'not in', ':'];
const KEYWORDS = ['AND', 'OR'];
const SAVED_FILTERS_STORAGE_PREFIX = 'qa:fql:saved:';
/** The pre-Task-22 prefix. Migrated wholesale on first read: these are values
 *  a user typed, not values the server can send again. */
const LEGACY_SAVED_FILTERS_STORAGE_PREFIX = 'vhp:fql:saved:';

function quoteIfNeeded(v: string): string {
  if (/\s/.test(v)) {
    return `"${v.replace(/"/g, '\\"')}"`;
  }
  return v;
}

function uniq(values: string[]): string[] {
  return Array.from(new Set(values));
}

function loadSavedFilters(savedFiltersKey: string): SavedFilterItem[] {
  if (typeof window === 'undefined') return [];
  try {
    // Wholesale rather than per-key: the suffix is user-chosen, so this is the
    // only place that can know the full set. Idempotent, so calling it on every
    // load costs one `localStorage.length` walk once the old keys are gone.
    migratePrefixedKeys(SAVED_FILTERS_STORAGE_PREFIX, LEGACY_SAVED_FILTERS_STORAGE_PREFIX);
    const raw = window.localStorage.getItem(`${SAVED_FILTERS_STORAGE_PREFIX}${savedFiltersKey}`);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (item): item is SavedFilterItem =>
          Boolean(item) &&
          typeof item.id === 'string' &&
          typeof item.name === 'string' &&
          typeof item.query === 'string' &&
          typeof item.updatedAt === 'number'
      )
      .sort((a, b) => b.updatedAt - a.updatedAt);
  } catch {
    return [];
  }
}

function persistSavedFilters(savedFiltersKey: string, filters: SavedFilterItem[]) {
  if (typeof window === 'undefined') return;
  window.localStorage.setItem(`${SAVED_FILTERS_STORAGE_PREFIX}${savedFiltersKey}`, JSON.stringify(filters));
}

function detectSuggestions(
  value: string,
  caret: number,
  fields: string[],
  valueSuggestions: Record<string, string[]>
): Suggestion[] {
  const left = value.slice(0, caret);
  const trailingWordMatch = left.match(/([A-Za-z_][\w]*)$/);
  const trailingWord = trailingWordMatch ? trailingWordMatch[1] : '';
  const trailingWordFrom = trailingWordMatch ? caret - trailingWord.length : caret;
  const suggestions: Suggestion[] = [];

  const valueMatch = left.match(/^.*?([A-Za-z_][\w]*)\s*(not\s+in|in|!=|!~|>=|<=|=|~|:|>|<)\s+([^()\s,]*)$/i);
  if (valueMatch) {
    const field = valueMatch[1].toLowerCase();
    const partial = valueMatch[3] || '';
    const replaceFrom = caret - partial.length;
    const options = uniq(valueSuggestions[field] || []);
    const filtered = options
      .filter((candidate) => candidate.toLowerCase().includes(partial.toLowerCase()))
      .slice(0, 10)
      .map((candidate) => ({
        label: candidate,
        insertText: quoteIfNeeded(candidate),
        replaceFrom,
        replaceTo: caret,
        kind: 'value' as const,
      }));

    if (filtered.length > 0) {
      suggestions.push(...filtered);
    }
  }

  const opMatch = left.match(/^.*?([A-Za-z_][\w]*)\s+([A-Za-z!<>=~:]*)$/i);
  if (opMatch) {
    const partial = opMatch[2] || '';
    const replaceFrom = caret - partial.length;
    suggestions.push(
      ...OPERATORS
      .filter((op) => op.toLowerCase().includes(partial.toLowerCase()))
      .map((op) => ({
        label: op,
        insertText: `${op} `,
        replaceFrom,
        replaceTo: caret,
        kind: 'operator' as const,
      }))
      .slice(0, 10)
    );
  }

  const fieldMatch = left.match(/([A-Za-z_][\w]*)?$/);
  if (fieldMatch) {
    const partial = fieldMatch[1] || '';
    const replaceFrom = caret - partial.length;
    const before = left.slice(0, replaceFrom);
    const isClauseStart = /(^|\s|\(|\bAND\b|\bOR\b)$/i.test(before);
    if (isClauseStart) {
      suggestions.push(
        ...fields
          .filter((field) => field.toLowerCase().includes(partial.toLowerCase()))
          .map((field) => ({
          label: field,
          insertText: `${field} `,
          replaceFrom,
          replaceTo: caret,
          kind: 'field' as const,
          }))
          .slice(0, 10)
      );
    }
  }

  if (/\s+$/.test(left) || trailingWord) {
    const replaceFrom = trailingWord ? trailingWordFrom : caret;
    const replaceTo = trailingWord ? caret : caret;
    const normalizedPartial = trailingWord.toLowerCase();
    suggestions.push(
      ...KEYWORDS.filter((kw) => !normalizedPartial || kw.toLowerCase().includes(normalizedPartial)).map((kw) => ({
        label: kw,
        insertText: `${kw} `,
        replaceFrom,
        replaceTo,
        kind: 'keyword' as const,
      }))
    );
  }

  const deduped = Array.from(
    new Map(suggestions.map((s) => [`${s.kind}:${s.label.toLowerCase()}`, s])).values()
  );
  return deduped.slice(0, 12);
}

export function FqlQueryInput({
  value,
  onChange,
  placeholder,
  fields,
  valueSuggestions = {},
  savedFiltersKey,
}: FqlQueryInputProps) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [isFocused, setIsFocused] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [caretPos, setCaretPos] = useState(0);
  const [savedFilters, setSavedFilters] = useState<SavedFilterItem[]>([]);
  const [selectedSavedFilterId, setSelectedSavedFilterId] = useState('');
  const [savedFilterName, setSavedFilterName] = useState('');

  useEffect(() => {
    if (!savedFiltersKey) {
      setSavedFilters([]);
      setSelectedSavedFilterId('');
      return;
    }
    setSavedFilters(loadSavedFilters(savedFiltersKey));
    setSelectedSavedFilterId('');
  }, [savedFiltersKey]);

  const suggestions = useMemo(
    () => detectSuggestions(value, caretPos, fields, valueSuggestions),
    [value, caretPos, fields, valueSuggestions]
  );

  const showSuggestions = isFocused && suggestions.length > 0;

  const saveCurrentFilter = () => {
    if (!savedFiltersKey) return;
    const query = value.trim();
    if (!query) return;

    const name = savedFilterName.trim() || `Filter ${savedFilters.length + 1}`;
    const now = Date.now();
    const existing = savedFilters.find((item) => item.name.toLowerCase() === name.toLowerCase());

    let next: SavedFilterItem[];
    let selectedId: string;

    if (existing) {
      selectedId = existing.id;
      next = savedFilters
        .map((item) =>
          item.id === existing.id
            ? {
                ...item,
                query,
                updatedAt: now,
              }
            : item
        )
        .sort((a, b) => b.updatedAt - a.updatedAt);
    } else {
      selectedId = `${now}-${Math.random().toString(36).slice(2, 8)}`;
      next = [
        {
          id: selectedId,
          name,
          query,
          updatedAt: now,
        },
        ...savedFilters,
      ].sort((a, b) => b.updatedAt - a.updatedAt);
    }

    setSavedFilters(next);
    setSelectedSavedFilterId(selectedId);
    setSavedFilterName('');
    persistSavedFilters(savedFiltersKey, next);
  };

  const applySavedFilter = (id: string) => {
    setSelectedSavedFilterId(id);
    const selected = savedFilters.find((item) => item.id === id);
    if (!selected) return;

    onChange(selected.query);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      const nextPos = selected.query.length;
      inputRef.current?.setSelectionRange(nextPos, nextPos);
      setCaretPos(nextPos);
      setActiveIndex(0);
    });
  };

  const deleteSelectedSavedFilter = () => {
    if (!savedFiltersKey || !selectedSavedFilterId) return;
    const next = savedFilters.filter((item) => item.id !== selectedSavedFilterId);
    setSavedFilters(next);
    setSelectedSavedFilterId('');
    persistSavedFilters(savedFiltersKey, next);
  };

  const applySuggestion = (suggestion: Suggestion) => {
    const next =
      value.slice(0, suggestion.replaceFrom) +
      suggestion.insertText +
      value.slice(suggestion.replaceTo);
    onChange(next);

    requestAnimationFrame(() => {
      const nextPos = suggestion.replaceFrom + suggestion.insertText.length;
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(nextPos, nextPos);
      setCaretPos(nextPos);
      setActiveIndex(0);
    });
  };

  return (
    <div className="space-y-2">
      <div className="flex flex-col gap-2 md:flex-row md:items-start">
        <div className="relative max-w-4xl flex-1">
          <Input
            ref={inputRef}
            value={value}
            onChange={(e) => {
              onChange(e.target.value);
              setCaretPos(e.target.selectionStart || e.target.value.length);
            }}
            onFocus={(e) => {
              setIsFocused(true);
              setCaretPos(e.target.selectionStart || value.length);
            }}
            onBlur={() => {
              setTimeout(() => setIsFocused(false), 120);
            }}
            onClick={(e) => setCaretPos((e.target as HTMLInputElement).selectionStart || value.length)}
            onKeyUp={(e) => setCaretPos((e.target as HTMLInputElement).selectionStart || value.length)}
            onKeyDown={(e) => {
              if (!showSuggestions) {
                if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === ' ') {
                  e.preventDefault();
                  setIsFocused(true);
                }
                return;
              }

              if (e.key === 'ArrowDown') {
                e.preventDefault();
                setActiveIndex((prev) => (prev + 1) % suggestions.length);
                return;
              }
              if (e.key === 'ArrowUp') {
                e.preventDefault();
                setActiveIndex((prev) => (prev - 1 + suggestions.length) % suggestions.length);
                return;
              }
              if (e.key === 'Enter' || e.key === 'Tab') {
                e.preventDefault();
                const selected = suggestions[activeIndex] || suggestions[0];
                if (selected) applySuggestion(selected);
                return;
              }
              if (e.key === 'Escape') {
                e.preventDefault();
                setIsFocused(false);
              }
            }}
            placeholder={placeholder}
            className="pr-8"
          />

          {value && (
            <button
              type="button"
              aria-label="Clear filter query"
              className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
              onClick={() => {
                onChange('');
                setActiveIndex(0);
                inputRef.current?.focus();
              }}
            >
              <X className="h-4 w-4" />
            </button>
          )}

          {showSuggestions && (
            <div className="absolute z-20 mt-1 w-full max-w-4xl rounded-md border bg-popover shadow-md">
              <ul className="max-h-56 overflow-auto py-1 text-sm">
                {suggestions.map((suggestion, idx) => (
                  <li key={`${suggestion.kind}-${suggestion.label}-${idx}`}>
                    <button
                      type="button"
                      className={`w-full text-left px-3 py-1.5 hover:bg-accent ${
                        idx === activeIndex ? 'bg-accent' : ''
                      }`}
                      onMouseEnter={() => setActiveIndex(idx)}
                      onClick={() => applySuggestion(suggestion)}
                    >
                      <span className="font-medium">{suggestion.label}</span>
                      <span className="ml-2 text-xs text-muted-foreground uppercase">{suggestion.kind}</span>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
        {savedFiltersKey && (
          <div className="flex w-full items-center gap-2 md:w-[380px]">
            <Input
              value={savedFilterName}
              onChange={(e) => setSavedFilterName(e.target.value)}
              placeholder="Save current filter as..."
              className="h-10"
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  saveCurrentFilter();
                }
              }}
            />
            <Button type="button" size="sm" variant="secondary" onClick={saveCurrentFilter} disabled={!value.trim()}>
              Save
            </Button>
          </div>
        )}
      </div>
      {savedFiltersKey && (
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          <div className="sm:w-72">
            <Select
              value={selectedSavedFilterId || '__none__'}
              onValueChange={(value) => {
                if (value === '__none__') {
                  setSelectedSavedFilterId('');
                  return;
                }
                applySavedFilter(value);
              }}
            >
              <SelectTrigger className="h-9">
                <SelectValue placeholder="Choose saved filter..." />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">Choose saved filter...</SelectItem>
                {savedFilters.map((item) => (
                  <SelectItem key={item.id} value={item.id}>
                    {item.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex flex-1 items-center gap-2">
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={deleteSelectedSavedFilter}
              disabled={!selectedSavedFilterId}
            >
              Delete
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
