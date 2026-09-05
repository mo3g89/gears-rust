import { useState } from 'react';
import { TestFileInfo, CustomPlanTest } from '../../api/types';
import { Checkbox } from '../ui/checkbox';
import { Input } from '../ui/input';
import { Label } from '../ui/label';
import { ScrollArea } from '../ui/scroll-area';
import { Badge } from '../ui/badge';

interface TestSelectorProps {
  tests: TestFileInfo[];
  productId?: string;
  selectedTests: CustomPlanTest[];
  onSelectionChange: (tests: CustomPlanTest[]) => void;
}

export function TestSelector({ tests, productId, selectedTests, onSelectionChange }: TestSelectorProps) {
  const [search, setSearch] = useState('');

  const filteredTests = tests.filter((test) => {
    if (productId && test.product_id !== productId) {
      return false;
    }

    const searchLower = search.toLowerCase();
    return (
      test.plan_name.toLowerCase().includes(searchLower) ||
      test.test_file.toLowerCase().includes(searchLower) ||
      test.tags.some((tag) => tag.toLowerCase().includes(searchLower)) ||
      (test.product_name || '').toLowerCase().includes(searchLower) ||
      (test.product_key || '').toLowerCase().includes(searchLower)
    );
  });

  // Group tests by plan
  const groupedTests = filteredTests.reduce((acc, test) => {
    if (!acc[test.plan_id]) {
      acc[test.plan_id] = { name: test.plan_name, source: test.source, repo_name: test.repo_name || undefined, tests: [] };
    }
    acc[test.plan_id].tests.push(test);
    return acc;
  }, {} as Record<string, { name: string; source: string; repo_name?: string; tests: TestFileInfo[] }>);

  const isSelected = (test: TestFileInfo) =>
    selectedTests.some((t) => t.plan_id === test.plan_id && t.test_file === test.test_file);

  const toggleTest = (test: TestFileInfo) => {
    const newSelection = isSelected(test)
      ? selectedTests.filter((t) => !(t.plan_id === test.plan_id && t.test_file === test.test_file))
      : [...selectedTests, { plan_id: test.plan_id, test_file: test.test_file }];
    onSelectionChange(newSelection);
  };

  const toggleGroup = (planTests: TestFileInfo[], allSelected: boolean) => {
    if (allSelected) {
      onSelectionChange(
        selectedTests.filter(
          (t) => !planTests.some((pt) => pt.plan_id === t.plan_id && pt.test_file === t.test_file)
        )
      );
    } else {
      const additions = planTests
        .filter((pt) => !isSelected(pt))
        .map((pt) => ({ plan_id: pt.plan_id, test_file: pt.test_file }));
      onSelectionChange([...selectedTests, ...additions]);
    }
  };

  return (
    <div className="space-y-4">
      <Input
        placeholder="Search tests..."
        value={search}
        onChange={(e) => setSearch(e.target.value)}
      />

      <div className="text-sm text-muted-foreground">
        {selectedTests.length} test{selectedTests.length !== 1 ? 's' : ''} selected
      </div>

      <ScrollArea className="h-[calc(100vh-22rem)] min-h-[300px] border rounded-md p-3">
        {Object.keys(groupedTests).length === 0 ? (
          <p className="px-1 py-8 text-center text-sm text-muted-foreground">No matching tests</p>
        ) : (
          <div className="space-y-5">
            {Object.entries(groupedTests).map(([planId, { name, source, repo_name, tests: planTests }]) => {
              const allSelected = planTests.every((t) => isSelected(t));
              return (
                <div key={planId} className="space-y-1.5">
                  <div className="flex items-center gap-2">
                    <Checkbox
                      id={`group-${planId}`}
                      checked={allSelected}
                      onCheckedChange={() => toggleGroup(planTests, allSelected)}
                    />
                    <Label htmlFor={`group-${planId}`} className="cursor-pointer font-semibold text-sm">
                      {name}
                    </Label>
                    {source === 'repo' && repo_name ? (
                      <Badge variant="outline" className="text-[10px] px-1.5 py-0">
                        {repo_name}
                      </Badge>
                    ) : (
                      <span className="text-[10px] text-muted-foreground uppercase tracking-wide">Local</span>
                    )}
                    <span className="ml-auto text-[10px] text-muted-foreground">
                      {planTests.filter((t) => isSelected(t)).length}/{planTests.length}
                    </span>
                  </div>
                  <div className="space-y-1 border-l pl-3 ml-2">
                    {planTests.map((test) => (
                      <label
                        key={`${test.plan_id}-${test.test_file}`}
                        className="flex cursor-pointer items-start gap-2 rounded px-1 py-1 hover:bg-accent"
                      >
                        <Checkbox
                          checked={isSelected(test)}
                          onCheckedChange={() => toggleTest(test)}
                          className="mt-0.5"
                        />
                        <div className="min-w-0 flex-1">
                          <span className="block break-all font-mono text-xs">{test.test_file}</span>
                        </div>
                      </label>
                    ))}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </ScrollArea>
    </div>
  );
}
