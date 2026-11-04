# Restrict a new data volume to one Pod

This optional profile starts one authenticated Brainwire instance and requests
`ReadWriteOncePod` for its data PVC. It uses the public release image, the
`brainwire-auth` Secret and retained volume policy.

Use this profile only for a **new deployment with a new PVC**, after checking
that the target StorageClass uses a compatible CSI driver. Kubernetes 1.29 or
newer makes this access mode stable; the CSI driver and its sidecars must support
it. The default `ReadWriteOnce` mode can allow several Pods on the same node to
mount a volume. `ReadWriteOncePod` restricts that volume to one Pod across the
cluster when the storage stack enforces it.

1. Prepare the `brainwire-auth` Secret as described in
   [protected Kubernetes setup](../../docs/protected-containers.md).
2. Choose an empty test namespace and a StorageClass that supports
   `ReadWriteOncePod`. The template uses the default StorageClass; inspect the
   rendered claim and set a suitable `storageClassName` in your deployment if
   its default is unsuitable.
3. Render, inspect and apply:

   ```sh
   kubectl kustomize deploy/kubernetes-single-writer
   kubectl apply -n YOUR_NEW_TEST_NAMESPACE -k deploy/kubernetes-single-writer
   kubectl get -n YOUR_NEW_TEST_NAMESPACE pvc data-brainwire-0 \
     -o jsonpath='{.spec.accessModes}'
   kubectl rollout status -n YOUR_NEW_TEST_NAMESPACE statefulset/brainwire
   ```

Verify the PVC is bound, the Pod becomes ready, and a second disposable Pod
requesting that same claim cannot obtain read-write access in the target
cluster before relying on the restriction. Keep the database stopped while testing restore or performing
offline operations against its volume. Never force-delete a database Pod to
bypass a storage attachment problem; fence the original writer first.

Applying this overlay does not migrate an existing StatefulSet or PVC from
`ReadWriteOnce`; those fields are not a safe in-place migration. Do not delete a
PVC to make the apply succeed. An existing deployment needs a planned migration
with a verified backup, fresh compatible storage and a tested restore.

This profile does not add replicas, quorum, backups, encryption, a disruption
budget or a NetworkPolicy. One Pod still means downtime during its restart.
Retaining a PVC is not protection against loss of its disk or machine. The
network and disruption overlays are separate choices; this profile should not
be layered onto an existing live installation by applying multiple overlays.

The static render check covers this manifest. CSI enforcement and recovery on
an actual cluster remain unverified by that check. See the official
[Kubernetes ReadWriteOncePod guide](https://kubernetes.io/docs/tasks/administer-cluster/change-pv-access-mode-readwriteoncepod/)
for supported storage components and migration requirements.
