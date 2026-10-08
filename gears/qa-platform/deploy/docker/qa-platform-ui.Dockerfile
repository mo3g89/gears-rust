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

# BASE IMAGES ARE PINNED BY DIGEST, with the human-readable tag beside it.
#
# What drifts if they are not: `node:20-alpine` and `nginx:alpine` are moving
# tags, so two builds a week apart are two different images with one name, and
# nothing in this repository changes when the bytes do. The nginx stage is the
# one that matters most: the SSE access-log redaction and the envsubst
# allow-list below are measured against a specific nginx, and a floating tag
# turns "the image behaves as tested" into "as tested on the day of the last
# build". A tag is also mutable at the registry, so it is not an integrity
# claim; a digest is.
#
# What broke: nothing yet -- this pin closes a gap the second review found
# (#95), it does not fix an incident. The failure it forecloses is the one the
# PDFIUM_VERSION block in the root .cargo/config.toml records, where an upstream
# release changed output on every open branch at once with no diff in the repo.
#
# The tag is documentation and the digest is the pin: docker resolves
# `name:tag@sha256:...` by the digest alone, so a tag that has since moved on
# still builds the bytes recorded here. `check_image_pins.py --online` proves
# tag and digest still name the same image.
#
# To bump (deliberately, in one commit):
#   1. Pick the new tag (e.g. `node:20.21.0-alpine`) and read its digest:
#        docker buildx imagetools inspect node:20.21.0-alpine
#      Take the top-level `Digest:` (the multi-arch index), not a per-platform one.
#   2. Replace BOTH the tag and the digest on the FROM line, never one alone.
#   3. `make helm-tests`, then
#        python3 gears/qa-platform/deploy/helm/tests/check_image_pins.py --online
#   4. Rebuild the image and run the UI once (`nginx -t` runs in the entrypoint).
#
# Build stage
FROM node:20.20.2-alpine@sha256:fb4cd12c85ee03686f6af5362a0b0d56d50c58a04632e6c0fb8363f609372293 AS builder

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
FROM nginx:1.31.6-alpine@sha256:df221db836e1754089190208cee7eeda94f233197056426eda74a43ab1abeac2

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
