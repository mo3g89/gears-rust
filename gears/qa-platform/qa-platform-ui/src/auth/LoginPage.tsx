/*
 * The only new page in this task. Reached in two ways:
 *   - as the `/login` route, which is where RP-initiated logout returns to; and
 *   - rendered in place by `RequireAuth` when a session cannot be used and
 *     bouncing back to the IdP would loop.
 *
 * Idiom follows the existing pages (see `src/pages/settings/SettingsJiraPage.tsx`):
 * shadcn `Card`/`CardHeader`/`CardTitle`/`CardDescription`/`CardContent`, the
 * `Button` from `@/components/ui/button`, a `lucide-react` icon, and the house
 * error branch `<p className="text-destructive">` that `deploy/compose/ui-gate.js`
 * detects structurally.
 *
 * It fetches nothing. That is a requirement, not an accident: it is one of the
 * two routes outside `RequireAuth`, so a data hook here would be a hook running
 * unauthenticated.
 */
import { Navigate } from 'react-router-dom';
import { LogIn } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { useAuth } from './useAuth';

export function LoginPage() {
  const { isAuthenticated, isLoading, error, login } = useAuth();

  // Only reachable via the `/login` route: `RequireAuth` renders this component
  // exclusively when unauthenticated.
  if (!isLoading && isAuthenticated) {
    return <Navigate to="/" replace />;
  }

  return (
    <main className="flex min-h-screen items-center justify-center bg-background p-4">
      <Card className="w-full max-w-sm">
        <CardHeader className="items-center text-center">
          <img
            src="/virtuozzo-logo.png"
            alt="Virtuozzo Logo"
            className="mx-auto h-10 w-10 object-contain"
          />
          <CardTitle className="mt-2">VHP Test Manager</CardTitle>
          <CardDescription>Sign in to continue.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {error ? (
            <div className="text-center py-2">
              <p className="text-destructive text-sm">{error}</p>
            </div>
          ) : null}
          <Button className="w-full" onClick={login} disabled={isLoading}>
            <LogIn className="h-4 w-4" />
            Sign in
          </Button>
        </CardContent>
      </Card>
    </main>
  );
}
