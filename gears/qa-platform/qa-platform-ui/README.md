# VHP Test Manager UI

Modern React-based UI for the VHP Test Manager system.

## Tech Stack

- **React 19** with TypeScript
- **Vite** for build tooling
- **React Router v7** for routing
- **Tailwind CSS** + **shadcn/ui** for styling
- **TanStack Query** for data fetching
- **Recharts** for data visualization
- **WebSocket** for real-time log streaming

## Development

### Prerequisites

- Node.js 20+
- npm or yarn

### Setup

```bash
# Install dependencies
npm install

# Start development server
npm run dev
```

The development server will start on `http://localhost:3000` and proxy `/qa/v1` requests to
`http://localhost:8087`, where the gears answer on the host (see
`gears/qa-platform/deploy/compose/docker-compose.yml`). Port 8080 is the compose stack's own
`ui` container, which serves the *built* bundle -- not something the dev server should talk to.

### Build

```bash
# Production build
npm run build

# Preview production build
npm run preview
```

## Docker Deployment

### Build the Docker image

```bash
docker build -t vhp-test-manager-ui:latest .
```

### Run locally

```bash
docker run -p 3000:80 vhp-test-manager-ui:latest
```

## Kubernetes Deployment

Deploy to Kubernetes using the manifests in `k8s/manager-ui/`:

```bash
kubectl apply -f k8s/manager-ui/
```

## Architecture

The UI is a standalone SPA that communicates with the Rust backend via:
- REST API endpoints at `/api/*`
- WebSocket endpoint at `/api/runs/{name}/ws` for real-time logs

### Key Features

- **Dashboard**: Overview with stats cards and recent runs
- **Test Plans**: Browse and execute test plans
- **Test Runs**: View and manage workflow executions with auto-refresh
- **Schedules**: Create and manage cron schedules
- **Real-time Logs**: WebSocket-based log streaming with fallback to polling
- **Charts**: Pass rate trends and status distribution

## Authentication

The UI signs in against Keycloak with the OpenID Connect authorization-code flow and PKCE
(`oidc-client-ts`, public client). `src/auth/` holds the whole subsystem: `provider.tsx`
(the `UserManager` and the React state), `RequireAuth.tsx` (the route guard -- an
unauthenticated app mounts no data queries at all), `LoginPage.tsx`, and `tokenState.ts`
(the access token, in memory, plus the "exactly one silent refresh per run of 401s, then
back to the sign-in screen" rule). `deploy/compose/ui-gate.js` drives the real flow through
a real browser and gates it.

## Configuration

Environment variables (optional), all read at BUILD time -- vite inlines `import.meta.env`,
so these cannot be supplied to the running container. See `.env.example`, and the build
`ARG`s in `../deploy/docker/qa-platform-ui.Dockerfile`.

- `VITE_API_URL`: Backend API URL (default: `/qa/v1`, set at `src/api/client.ts:6`)
- `VITE_OIDC_ISSUER`: the OIDC issuer **as the browser reaches it** (default
  `http://localhost:8180/realms/qa-platform`). Not the gears' in-cluster discovery URL.
- `VITE_OIDC_CLIENT_ID`: the realm's public client (default `qa-platform-ui`)

## Project Structure

```
src/
  api/          # API client, types, and hooks
  components/   # React components
    ui/         # shadcn/ui components
    layout/     # App shell, sidebar, header
    dashboard/  # Dashboard components
    plans/      # Test plans components
    runs/       # Test runs components
    schedules/  # Schedules components
  pages/        # Page components
  hooks/        # Custom hooks (WebSocket)
  lib/          # Utility functions
```
