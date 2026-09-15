# Image for the qa-platform UI: a node build stage that produces the static
# bundle, and an nginx stage that serves it and proxies /qa/v1 to the `gears`
# service (see gears/qa-platform/qa-platform-ui/default.conf.template -- the
# envsubst template that replaced the old static nginx.conf; there is no
# nginx.conf in the tree any more).
#
# Copied from legacy's manager-ui/Dockerfile, which was already right in shape,
# with one change: the dependency install goes through the lockfile the repo
# commits. Legacy did `COPY package.json ./` + `npm install`, which re-resolves
# every range afresh at image-build time; Tasks 9, 10 and 11 each added dev
# dependencies (@testing-library/react, openapi-typescript, jsdom, vitest), so
# a re-resolving install is now a real risk of building the UI against
# different versions than the ones `npm test` and `tsc` were run against.
#
# BUILD CONTEXT: gears/qa-platform/qa-platform-ui (deploy-k8s.sh builds with
# that directory as the context). Every COPY below is
# relative to that directory, not to this file's own, which is why they read as
# bare `package.json` / `.` rather than `qa-platform-ui/...`. That context also
# brings along qa-platform-ui/.dockerignore, which keeps node_modules and dist
# out of the transferred context.

# Build stage
FROM node:20-alpine AS builder

WORKDIR /app

# Copy package files
COPY package.json package-lock.json ./

# Install dependencies from the lockfile
RUN npm ci

# Copy source files
COPY . .

# The OIDC endpoint the BROWSER will use. vite inlines `import.meta.env` at
# build time, so these cannot be supplied as container environment variables
# later -- they have to be here or not at all. Both are declared with no default
# on purpose: an unset ARG becomes an empty string in the bundle, and
# src/auth/provider.tsx falls back to its own local-development values on a
# falsy read, so the default lives in exactly one place (that file) rather than
# two that can drift.
#
# THE ISSUER IS THE BROWSER'S URL. `https://keycloak:8443/realms/qa-platform` is
# the *gears'* discovery URL -- private CA, in-cluster only -- and must never
# be passed here. See qa-platform-ui/.env.example.
ARG VITE_OIDC_ISSUER
ARG VITE_OIDC_CLIENT_ID
ENV VITE_OIDC_ISSUER=$VITE_OIDC_ISSUER
ENV VITE_OIDC_CLIENT_ID=$VITE_OIDC_CLIENT_ID

# Build the application
RUN npm run build

# Production stage
FROM nginx:alpine

# Copy built assets from builder
COPY --from=builder /app/dist /usr/share/nginx/html

# nginx:alpine runs envsubst over /etc/nginx/templates/*.template at start-up and
# writes the result to /etc/nginx/conf.d/. NGINX_ENVSUBST_FILTER is an ALLOW-LIST:
# without it envsubst also eats nginx's own runtime variables -- $sse_authorization,
# $uri, $gears -- and the SSE auth bridge disappears with no error naming this file.
COPY default.conf.template /etc/nginx/templates/default.conf.template
ENV NGINX_ENVSUBST_FILTER='^(NGINX_RESOLVER|GEARS_UPSTREAM)$'

# NO ENVIRONMENT-SPECIFIC DEFAULTS FOR THESE TWO. They used to be
# `ENV NGINX_RESOLVER=127.0.0.11` (Docker's embedded DNS) and
# `ENV GEARS_UPSTREAM=http://gears:8087` (a bare service name) -- both baked
# into an image that also ships to Kubernetes, where each is wrong and fails at
# REQUEST time rather than at start-up.
#
# `NGINX_RESOLVER` is now derived from the container's own /etc/resolv.conf by
# the entrypoint hook below, which is correct under Docker and under any
# Kubernetes cluster without being told. `GEARS_UPSTREAM` has no sane default at
# all -- only the deployer knows where the gears are -- so it is left unset, and
# an unset value renders `proxy_pass ;`, which nginx refuses at start-up. Loud is
# the point.
#
# It lives under the UI app directory, not deploy/docker/, because this image's
# build context is gears/qa-platform/qa-platform-ui/ (deploy-k8s.sh builds it
# with `-f ../deploy/docker/qa-platform-ui.Dockerfile .` from there), so
# anything under deploy/ is outside the context and a COPY of it fails.
COPY docker-entrypoint.d/15-resolver-from-resolv-conf.envsh /docker-entrypoint.d/
RUN chmod +x /docker-entrypoint.d/15-resolver-from-resolv-conf.envsh

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]
