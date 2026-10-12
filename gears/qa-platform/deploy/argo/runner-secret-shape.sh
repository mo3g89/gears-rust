# Sourced, not executed. The name shape of the Secrets qa-environments' retired
# D4 writer created: "qa-platform-", the tenant UUID truncated to 34 characters,
# "-", and a 16-hex FNV-1a digest. Shared by argo/cleanup-runner-secrets.sh and
# remote/verify-k8s.sh so the two cannot drift.
writer_shape='^qa-platform-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{10}-[0-9a-f]{16}$'
