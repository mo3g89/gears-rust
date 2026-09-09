import { BrowserRouter, Routes, Route, Navigate, useParams } from 'react-router-dom';
import { QueryClientProvider } from '@tanstack/react-query';
import { Toaster } from 'sonner';
import { ThemeProvider } from './components/theme-provider';
import { ConfirmProvider } from './components/ui/confirm-dialog';
import { AppShell } from './components/layout/AppShell';
import { RequireProduct } from './components/layout/RequireProduct';
import { DashboardPage } from './pages/DashboardPage';
import { PlansPage } from './pages/PlansPage';
import { PlanDetailPage } from './pages/PlanDetailPage';
import { RunsPage } from './pages/RunsPage';
import { RunDetailPage } from './pages/RunDetailPage';
import { SchedulesPage } from './pages/SchedulesPage';
import { CustomPlansPage } from './pages/CustomPlansPage';
import { CustomPlanDetailPage } from './pages/CustomPlanDetailPage';
import { CustomPlanEditorPage } from './pages/CustomPlanEditorPage';
import { EnvironmentsPage } from './pages/EnvironmentsPage';
import { EnvironmentDetailPage } from './pages/EnvironmentDetailPage';
import { ProductsPage } from './pages/ProductsPage';
import { ProductDetailPage } from './pages/ProductDetailPage';
import { TestResultsPage } from './pages/TestResultsPage';
import { AnalyticsPage } from './pages/AnalyticsPage';
import { SettingsLayoutPage } from './pages/settings/SettingsLayoutPage';
import { SettingsVariablesPage } from './pages/settings/SettingsVariablesPage';
import { SettingsSshKeysPage } from './pages/settings/SettingsSshKeysPage';
import { SettingsJiraPage } from './pages/settings/SettingsJiraPage';
import { NotificationsLayoutPage } from './pages/notifications/NotificationsLayoutPage';
import { NotificationsEmailPage } from './pages/notifications/NotificationsEmailPage';
import { NotificationsSlackPage } from './pages/notifications/NotificationsSlackPage';
import { AuthProvider, AuthCallbackPage, LoginPage, RequireAuth } from './auth';
import { queryClient } from './api/queryClient';

/** `/platforms` -> `/environments` (D5's route rename), kept reachable so a
 *  bookmarked or shared link still lands.
 *
 *  A named component rather than a `<Navigate>` written inline in the route,
 *  **so that a test can pin the real one**. The first version of `App.test.ts`
 *  re-implemented this redirect inside the test and therefore asserted against
 *  its own copy: changing the route's actual target here survived all 231 tests
 *  (re-review, N-1). `RedirectToEnvironmentDetail` below was always imported and
 *  was never exposed to that. */
export function RedirectToEnvironments() {
  return <Navigate to="/environments" replace />;
}

/** `/platforms/:name` -> `/environments/:name`, preserving the path parameter so a
 *  bookmarked or shared link to one environment's detail page still lands (D5's
 *  route rename). Separate from [`RedirectToEnvironments`] only because this one
 *  needs the segment carried over rather than dropped. */
export function RedirectToEnvironmentDetail() {
  const { name } = useParams<{ name: string }>();
  return <Navigate to={`/environments/${name ?? ''}`} replace />;
}

function App() {
  return (
    <ThemeProvider>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        {/* AuthProvider sits INSIDE BrowserRouter (it navigates) and inside
            QueryClientProvider (logout clears the cache, so the next user never
            sees the previous one's rows). */}
        <AuthProvider>
        <ConfirmProvider>
        <Toaster position="top-right" richColors closeButton />
        <Routes>
          {/* The two routes outside the guard, and the only two. Neither
              fetches anything: they are reachable without a token by
              construction, so a data hook on either would be a hook running
              unauthenticated -- which is the thing RequireAuth exists to
              prevent. `/auth/callback` is served by nginx's SPA fallback
              (`try_files $uri $uri/ /index.html`); it matches neither the
              `location /qa/v1/` prefix nor the static-asset extension regex. */}
          <Route path="/auth/callback" element={<AuthCallbackPage />} />
          <Route path="/login" element={<LoginPage />} />
          {/* Everything below needs a token. The guard wraps AppShell rather
              than each page, so no hook under it -- and every data hook in this
              app is under it -- can mount before there is one. */}
          <Route element={<RequireAuth><AppShell /></RequireAuth>}>
            <Route path="/" element={<RequireProduct><DashboardPage /></RequireProduct>} />
            <Route path="/plans" element={<RequireProduct><PlansPage /></RequireProduct>} />
            <Route path="/plans/:id" element={<RequireProduct><PlanDetailPage /></RequireProduct>} />
            {/* Both lists are product-scoped (`useRuns`/`useSchedules` resolve each
                row's product through its target), and both hooks are `enabled: !!product`
                — a disabled React Query v5 query reports `isLoading: false` with
                `data: undefined`, so without this gate each page fell through its
                `if (isLoading)` branch to a false "No test runs found" on every visit
                before `/products` resolved, and permanently on a deployment with no
                products at all. The earlier claim that these lists are tenant-wide is
                withdrawn — see REMOVED-SURFACES.md's closing C7 entry.
                `/runs/:name` is deliberately NOT gated: spec D4 hides an unattributable
                run from the list and keeps it reachable by direct URL, and the detail
                page reads no product-scoped hook. */}
            <Route path="/runs" element={<RequireProduct><RunsPage /></RequireProduct>} />
            <Route path="/runs/:name" element={<RunDetailPage />} />
            <Route path="/schedules" element={<RequireProduct><SchedulesPage /></RequireProduct>} />
            <Route path="/custom-plans" element={<RequireProduct><CustomPlansPage /></RequireProduct>} />
            <Route path="/plans/custom/new" element={<RequireProduct><CustomPlanEditorPage /></RequireProduct>} />
            <Route path="/plans/custom/:id/edit" element={<RequireProduct><CustomPlanEditorPage /></RequireProduct>} />
            <Route path="/custom-plans/:id" element={<RequireProduct><CustomPlanDetailPage /></RequireProduct>} />
            <Route path="/plans/custom/:id" element={<RequireProduct><CustomPlanDetailPage /></RequireProduct>} />
            <Route path="/environments" element={<RequireProduct><EnvironmentsPage /></RequireProduct>} />
            <Route path="/environments/:name" element={<RequireProduct><EnvironmentDetailPage /></RequireProduct>} />
            {/* Old paths, kept reachable: a bookmarked or shared link to either must still land. */}
            <Route path="/platforms" element={<RedirectToEnvironments />} />
            <Route path="/platforms/:name" element={<RedirectToEnvironmentDetail />} />
            <Route path="/products" element={<ProductsPage />} />
            <Route path="/products/:id" element={<ProductDetailPage />} />
            <Route path="/analytics" element={<RequireProduct><AnalyticsPage /></RequireProduct>} />
            <Route path="/analytics/plan/:planId" element={<RequireProduct><TestResultsPage /></RequireProduct>} />
            <Route path="/notifications" element={<NotificationsLayoutPage />}>
              <Route index element={<Navigate to="email" replace />} />
              <Route path="email" element={<NotificationsEmailPage />} />
              <Route path="slack" element={<NotificationsSlackPage />} />
            </Route>
            <Route path="/settings" element={<SettingsLayoutPage />}>
              <Route index element={<Navigate to="variables" replace />} />
              <Route path="variables" element={<SettingsVariablesPage />} />
              <Route path="ssh-keys" element={<SettingsSshKeysPage />} />
              <Route path="jira" element={<SettingsJiraPage />} />
              <Route path="notifications" element={<Navigate to="/notifications/email" replace />} />
              <Route path="jira-poller" element={<Navigate to="/settings/jira" replace />} />
            </Route>
          </Route>
        </Routes>
        </ConfirmProvider>
        </AuthProvider>
      </BrowserRouter>
    </QueryClientProvider>
    </ThemeProvider>
  );
}

export default App;
