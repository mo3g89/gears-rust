{{/* The bare host out of publicOrigin. gen-cert.sh REFUSES anything that is not a
     plain DNS hostname or IPv4 literal, so this strips scheme and any port and
     fails loudly rather than writing a bad value into a certificate's SANs. */}}
{{- define "qa-platform.publicHost" -}}
{{- $o := required "publicOrigin is required" .Values.publicOrigin -}}
{{- $noScheme := regexReplaceAll "^https?://" $o "" -}}
{{- $host := regexReplaceAll ":[0-9]+$" $noScheme "" -}}
{{- if or (contains "/" $host) (eq $host "") -}}
{{- fail (printf "publicOrigin %q must be scheme://host[:port] with no path" $o) -}}
{{- end -}}
{{- $host -}}
{{- end -}}

{{/* entrypoint.sh appends `/realms/qa-platform` to PUBLIC_ISSUER_ORIGIN itself,
     so the gears get the BARE ORIGIN. This helper is the full issuer, used for
     the UI build arg and the verification checks. */}}
{{- define "qa-platform.issuer" -}}
{{- printf "%s/realms/qa-platform" (required "publicOrigin is required -- set it with --set publicOrigin=https://host" .Values.publicOrigin) -}}
{{- end -}}

{{/* Selector / pod-template labels shared by all four stateful workloads
     (gears, ui, keycloak, postgres): name + component + instance. Call as
     `include "qa-platform.selectorLabels" (dict "root" $ "component" "gears")`.

     `instance` is what makes two releases of this chart, installed side by
     side, select only their own pods -- the whole point of this chart major
     version. It belongs in BOTH the Deployment's/StatefulSet's
     spec.selector.matchLabels AND the pod template's metadata.labels: the
     latter must be a superset of the former or Kubernetes rejects the
     object (a selector that never matches its own pod template).

     A Deployment's/StatefulSet's spec.selector is IMMUTABLE. This helper is
     therefore not a safe drop-in for an existing release -- see
     UPGRADING.md for the hand-run migration `helm upgrade` cannot do by
     itself.

     ALSO used, unmodified, for the four Services' (gears, ui, keycloak,
     postgres) `spec.selector` -- a flat map, the same shape this helper
     already emits, so no `matchLabels` wrapper is needed there the way it
     is for a Deployment/StatefulSet. Unlike spec.selector on a workload, a
     Service's selector is MUTABLE, so adding `instance` to it needed no
     migration and no chart version bump beyond this one's. */}}
{{- define "qa-platform.selectorLabels" -}}
app.kubernetes.io/name: qa-platform
app.kubernetes.io/component: {{ .component }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
{{- end -}}

{{/* The prefix of every runner-credential Secret name qa-environments' D4
     writer derives (`secret_name` in runner_secret_writer.rs). ONE source for
     two consumers that must agree: the qa-environments fragment's
     `argo.secret_prefix` (gears-argo-configmaps.yaml) and the admission
     policy that refuses any other name from the writer's identity
     (secret-writer-admission-policy.yaml). Equal to qa-runs'
     `secret_name_prefix` default, which the chart does not render -- the
     parity test in runner_secret_writer.rs pins the two Rust defaults.

     Not a value, on purpose: every legitimate name starts with this prefix
     only because it is already lower-case `[a-z0-9-]`, does not start with
     `-`, and is shorter than the 46-byte readable budget, so `sanitize` and
     `truncate` leave it intact. check_secret_writer_rbac.py asserts that. */}}
{{- define "qa-platform.runnerSecretPrefix" -}}
qa-platform-
{{- end -}}

{{/* A third-party image reference: `repository:tag@digest` when `digest` is set,
     else `repository:tag`. Call as `include "qa-platform.image" .Values.images.postgres`.
     The tag stays beside the digest for the reader; the digest is what the
     container runtime pulls. check_image_pins.py refuses a committed value
     with no digest. First-party images (gears, ui, runner) are built and
     imported locally and keep their tags. */}}
{{- define "qa-platform.image" -}}
{{- if .digest -}}
{{- printf "%s:%s@%s" .repository .tag .digest -}}
{{- else -}}
{{- printf "%s:%s" .repository .tag -}}
{{- end -}}
{{- end -}}

{{/* RUST_LOG for the gears containers, from .Values.gears.logLevel -- REFUSED
     when it makes the global level, or a target named pingora..., oagw...,
     cf_gears_oagw... or api-gateway, more verbose than info. pingora (which oagw proxies
     through) prints whole request lines at debug and trace, the api-gateway's
     debug lines name every path it routes, and the Slack webhook path is a
     credential (qa-insights' slack_oagw.rs header), so `--set
     gears.logLevel=debug` would write webhook credentials into the logs.
     `gears.allowVerboseProxyLogs=true` is the explicit opt-in.

     Parsed the way tracing's EnvFilter reads it: comma-separated directives,
     each `level`, `target=level`, or `target` alone -- which means TRACE for
     that target. A target's `[span...]` suffix and `::module` tail are
     dropped before the match; a directive with no target (`[span]=debug`)
     applies to every target and counts as global.

     MATCHED BY PREFIX, BOTH WAYS. EnvFilter applies a directive to every event
     whose target STARTS WITH the directive's target, so `ping=debug` reaches
     pingora, `cf_gears=debug` cf_gears_oagw and `api=trace` api_gateway. A
     directive is refused when its target is a prefix of a protected one, or a
     protected one is a prefix of it (`pingora_core` sits inside pingora). Levels are
     case-insensitive and may be the digits 0-5. check_log_level_redaction.py
     renders each shape both ways. */}}
{{- define "qa-platform.rustLog" -}}
{{- $raw := toString .Values.gears.logLevel -}}
{{- if not .Values.gears.allowVerboseProxyLogs -}}
{{- $levels := list "off" "error" "warn" "info" "debug" "trace" "0" "1" "2" "3" "4" "5" -}}
{{- $verbose := list "debug" "trace" "4" "5" -}}
{{- $protected := list "pingora" "pingora_core" "pingora_proxy" "oagw" "cf_gears_oagw" "api_gateway" "api-gateway" -}}
{{- range $d := splitList "," $raw -}}
{{- $d = trim $d -}}
{{- if $d -}}
{{- $target := "" -}}
{{- $level := "" -}}
{{- if has (lower $d) $levels -}}
{{- $level = lower $d -}}
{{- else -}}
{{- $candidate := lower (regexReplaceAll "^.*=" $d "") -}}
{{- if and (contains "=" $d) (has $candidate $levels) -}}
{{- $target = regexReplaceAll "=[^=]*$" $d "" -}}
{{- $level = $candidate -}}
{{- else -}}
{{- $target = $d -}}
{{- $level = "trace" -}}
{{- end -}}
{{- end -}}
{{- $base := regexReplaceAll "::.*$" (regexReplaceAll "\\[.*$" $target "") "" -}}
{{- $covers := eq $base "" -}}
{{- range $p := $protected -}}
{{- if or (hasPrefix $base $p) (hasPrefix $p $base) -}}
{{- $covers = true -}}
{{- end -}}
{{- end -}}
{{- if and (has $level $verbose) $covers -}}
{{- fail (printf "gears.logLevel %q: the directive %q makes %s log at %s. pingora (which oagw proxies through) prints whole request lines at debug and trace and the api-gateway's debug lines name every path it routes, and the Slack webhook path in those lines is a credential. Keep the global level, and any target that is a prefix of or inside pingora, oagw, cf_gears_oagw or api_gateway, at info or less (e.g. info,qa_runs=debug), or set gears.allowVerboseProxyLogs=true to accept webhook credentials in the logs." $raw $d (ternary "every target" (printf "target %q" $base) (eq $base "")) $level) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $raw -}}
{{- end -}}
