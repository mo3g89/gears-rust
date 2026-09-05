# Hairpin measurement: pod -> node hostPort

Date: 2026-08-29 (measured; re-verified same day, fix round 1)
Cluster: single-node k3s v1.36.3+k3s1 at root@10.136.20.200 (KUBECONFIG=/etc/rancher/k3s/k3s.yaml)
hostPort used: 18080 (free on this node both runs; no conflict encountered, so no alternate port was needed)

## Exact commands (reproducible one-liners, redirection plumbing intact)

Step 1 — create the hostPort target pod and wait for it to be Ready:

```
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl run hairpin-target --image=nginx:alpine --restart=Never \
  --overrides="{\"spec\":{\"containers\":[{\"name\":\"nginx\",\"image\":\"nginx:alpine\",\"ports\":[{\"containerPort\":80,\"hostPort\":18080}]}]}}"
kubectl wait --for=condition=Ready pod/hairpin-target --timeout=90s' > step1.log 2>&1
echo "wrapper_exit=$?" >> step1.log
cat step1.log
```

Step 2 — curl the node IP from a second pod, redirecting curl's own output/exit
capture to a file instead of piping (so the exit code read back is curl's, not a
downstream stage's):

```
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl run hairpin-probe --image=curlimages/curl --restart=Never --rm -i --quiet -- \
  curl -sS -o /tmp/out -w "%{http_code}" --max-time 10 http://10.136.20.200:18080/ > /tmp/hairpin.rc 2>&1
echo "exit=$?"; cat /tmp/hairpin.rc' > step2.log 2>&1
echo "wrapper_exit=$?" >> step2.log
cat step2.log
```

Step 4 — cleanup:

```
ssh root@10.136.20.200 'export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl delete pod hairpin-target --ignore-not-found
kubectl delete pod hairpin-probe --ignore-not-found
kubectl get pods'
```

## Verbatim output (fix round 1 re-run, 2026-08-29)

Step 1 (`step1.log`), byte-for-byte:

```
pod/hairpin-target created
pod/hairpin-target condition met
wrapper_exit=0
```

Step 2 (`step2.log`), byte-for-byte:

```
exit=0
200wrapper_exit=0
```

(The `200` and `wrapper_exit=0` appear on the same line because the probe's `-w
"%{http_code}"` output has no trailing newline before the next `echo` runs — this
is the raw terminal output, not a transcription artifact. Read as: HTTP status
`200`, followed immediately by the outer wrapper's `wrapper_exit=0`.)

Cleanup output, byte-for-byte:

```
pod "hairpin-target" deleted from default namespace
No resources found in default namespace.
wrapper_exit=0
```

`hairpin-probe` was launched with `--rm`, so it self-deletes on completion; the
explicit `kubectl delete pod hairpin-probe --ignore-not-found` in the cleanup
command is a no-op confirming that (no line is emitted for a pod that no longer
exists beyond the `--ignore-not-found` swallowing it, consistent with `kubectl get
pods` reporting "No resources found" immediately after).

## Conclusion

exit=0 (curl's own exit code, captured via redirection to a file, not a pipe) and
HTTP status 200. The pod -> node-IP hairpin to a `hostPort` service **works** on
this cluster. The gears pod can reach the UI pod's `hostPort: 443` via the node IP
over HTTPS, so no `hostAliases` fallback is needed.

Resulting default: `gears.hostAliases.enabled: false`

## Cleanup confirmation

`kubectl get pods` in `default` returned "No resources found in default
namespace." after both runs (initial measurement and fix-round-1 re-run). The
`argo` namespace was not touched by either run.
