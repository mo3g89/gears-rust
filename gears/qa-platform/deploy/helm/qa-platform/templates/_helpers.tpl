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
