import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
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
import { PlatformsPage } from './pages/PlatformsPage';
import { PlatformDetailPage } from './pages/PlatformDetailPage';
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
import { ApiError } from './api/client';

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      /**
       * One retry for a transient failure, and **none for a 401**.
       *
       * A 401 is a statement about the session, not about this request, so
       * retrying it cannot succeed — it only doubles the request volume of a
       * broken session. Measured on this stack before this rule and
       * `RequireAuth` existed, by counting an unauthenticated headless browser's
       * requests on `/runs`: **20 `/qa/v1` requests in 25 seconds** on an
       * unseeded deployment and **34 in 25 seconds** once `smoke.sh` had seeded
       * it — every one a 401, with no end condition, because `refetchInterval`
       * keeps polling through errors and `retry: 1` doubled each cycle. The
       * gateway's rate limiter answered 429 with a twelve-hour `retry_after` to
       * *every* client on the host as a result.
       *
       * `RequireAuth` is the structural half of the fix (an unauthenticated app
       * mounts no queries at all) and `@/auth/tokenState` handles a token that
       * dies mid-session; this is the cheap third guard for anything that gets
       * past both.
       */
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status === 401) return false;
        return failureCount < 1;
      },
      staleTime: 30000, // 30 seconds
    },
  },
});

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
            {/* Runs and schedules are not product-scoped in this deployment — neither
                carries a product key, so gating them on a selected product would gate a
                tenant-wide list on a scope it does not honour (REMOVED-SURFACES.md, Task 8a C7). */}
            <Route path="/runs" element={<RunsPage />} />
            <Route path="/runs/:name" element={<RunDetailPage />} />
            <Route path="/schedules" element={<SchedulesPage />} />
            <Route path="/custom-plans" element={<RequireProduct><CustomPlansPage /></RequireProduct>} />
            <Route path="/plans/custom/new" element={<RequireProduct><CustomPlanEditorPage /></RequireProduct>} />
            <Route path="/plans/custom/:id/edit" element={<RequireProduct><CustomPlanEditorPage /></RequireProduct>} />
            <Route path="/custom-plans/:id" element={<RequireProduct><CustomPlanDetailPage /></RequireProduct>} />
            <Route path="/plans/custom/:id" element={<RequireProduct><CustomPlanDetailPage /></RequireProduct>} />
            <Route path="/platforms" element={<RequireProduct><PlatformsPage /></RequireProduct>} />
            <Route path="/platforms/:name" element={<RequireProduct><PlatformDetailPage /></RequireProduct>} />
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
