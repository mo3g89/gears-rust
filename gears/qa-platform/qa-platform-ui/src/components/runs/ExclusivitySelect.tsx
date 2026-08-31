import { Label } from '@/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';

/**
 * Three states, not a checkbox. A checkbox has only two, so "unchecked" would
 * send an explicit `false` and override a destructive test's own
 * `TEST_META["exclusive"]`. `auto` sends nothing and lets the plan and the test
 * files decide.
 */
export type Exclusivity = 'auto' | 'parallel' | 'exclusive';

/** The wire value: `undefined` for auto, so the parameter is omitted entirely. */
export function exclusivityToParam(value: Exclusivity): boolean | undefined {
  if (value === 'exclusive') return true;
  if (value === 'parallel') return false;
  return undefined;
}

/** Map a stored tri-state (e.g. a schedule's) back to the control's value. */
export function exclusivityFromValue(value: boolean | null | undefined): Exclusivity {
  if (value === true) return 'exclusive';
  if (value === false) return 'parallel';
  return 'auto';
}

interface ExclusivitySelectProps {
  value: Exclusivity;
  onChange: (value: Exclusivity) => void;
  disabled?: boolean;
  id?: string;
}

export function ExclusivitySelect({
  value,
  onChange,
  disabled,
  id = 'exclusivity',
}: ExclusivitySelectProps) {
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>Platform access</Label>
      <Select value={value} onValueChange={(next) => onChange(next as Exclusivity)} disabled={disabled}>
        <SelectTrigger id={id}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="auto">Auto (from tests)</SelectItem>
          <SelectItem value="parallel">Parallel</SelectItem>
          <SelectItem value="exclusive">Exclusive</SelectItem>
        </SelectContent>
      </Select>
      <p className="text-xs text-muted-foreground">
        Auto follows plan.yaml and each test's TEST_META. Exclusive waits until the platform is
        free, then holds it alone — anything launched meanwhile waits behind it. Either way a run
        is never rejected for a busy platform: it queues and starts automatically.
      </p>
    </div>
  );
}
