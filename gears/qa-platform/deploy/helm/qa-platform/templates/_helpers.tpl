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
