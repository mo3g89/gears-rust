import { useMemo } from 'react';
import type { ApexOptions } from 'apexcharts';
import { useTheme } from '@/components/theme-provider';

/**
 * Theme-aware ApexCharts fragments, because none of the four charts in this app were.
 *
 * The visible bug was on the dashboard's Run Tests By Status chart: its tooltip is a
 * `tooltip.custom` function returning bare HTML with no colours of its own, and ApexCharts
 * wraps custom tooltip HTML in `.apexcharts-tooltip.apexcharts-theme-light`, which sets
 * `background: rgba(255,255,255,.96)` and **no** `color`
 * (`apexcharts/dist/apexcharts.css:102-105`). In dark mode the text therefore inherited the
 * page's near-white foreground and rendered white-on-white — a blank white square where the
 * tooltip should be. The same class made every chart's axis labels, legend and default
 * tooltip light-on-dark, and all four charts hardcoded the light grid colour `#e2e8f0`.
 *
 * `theme.mode` is the fix rather than a pile of per-element colours: ApexCharts'
 * `updateThemeOptions` (`apexcharts/src/modules/Theme.js:199-221`) derives `chart.foreColor`
 * and `tooltip.theme` from it, and `.apexcharts-theme-dark` carries `color: #fff` with a dark
 * background. Note that it *overwrites* any `chart.foreColor` you pass alongside it, so do not
 * set one. An explicit `colors: [...]` still wins over the palette `theme.mode` selects, so
 * the green/red series keep their meaning in both modes.
 */
export interface ChartTheme {
  /** For `options.theme.mode`. */
  mode: 'light' | 'dark';
  /** For `options.grid.borderColor`. */
  gridBorderColor: string;
  /** Inline styles for a `tooltip.custom` container, so it does not depend on which
   *  `.apexcharts-theme-*` class ApexCharts happens to put around it. */
  tooltipStyle: string;
}

/** Memoised on the resolved theme, because callers put it in `useMemo` dependency
 *  arrays: a fresh object every render would invalidate every chart's memo on every
 *  render, which is worse than the hardcoded colours this replaces. */
export function useChartTheme(): ChartTheme {
  const { resolvedTheme } = useTheme();
  return useMemo<ChartTheme>(() => {
    const dark = resolvedTheme === 'dark';
    return {
      mode: dark ? 'dark' : 'light',
      gridBorderColor: dark ? '#334155' : '#e2e8f0',
      tooltipStyle: dark
        ? 'background:#1e293b;color:#e2e8f0;border:1px solid #334155;'
        : 'background:#ffffff;color:#0f172a;border:1px solid #e2e8f0;',
    };
  }, [resolvedTheme]);
}

/** The options every chart in this app shares, merged into each chart's own options. */
export function chartThemeOptions(theme: ChartTheme): Pick<ApexOptions, 'theme' | 'grid'> {
  return {
    theme: { mode: theme.mode },
    grid: { borderColor: theme.gridBorderColor, strokeDashArray: 4 },
  };
}
