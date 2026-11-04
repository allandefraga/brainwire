# Restrict Kubernetes client access

This optional profile runs **one authenticated Brainwire instance**, with its
retained data volume and a disruption budget. It adds an ingress NetworkPolicy.
It does not configure replication, quorum peers or failover.

1. Prepare the `brainwire-auth` Secret as described in
   [the orchestrator guide](../../docs/orchestrators.md).
2. Add this label to the pod template of each trusted application or
   administration Job in the **same namespace**:

   ```yaml
   metadata:
     labels:
       brainwire-client: "true"
   ```

3. Inspect and apply the profile in your test namespace:

   ```sh
   kubectl kustomize deploy/kubernetes-network-isolated
   kubectl apply -n YOUR_TEST_NAMESPACE -k deploy/kubernetes-network-isolated
   ```

The policy selects pods labeled `app: brainwire`. It allows inbound TCP port
7687 from the labeled client pods in that namespace. Other pod ingress is
denied **only when your cluster's CNI enforces NetworkPolicy**. Verify both an
allowed and a denied connection in your cluster before relying on this profile.
NetworkPolicies are additive: another policy allowing more traffic can widen
access. A user who can create or relabel pods can grant this network access;
Brainwire authentication and grants remain necessary.

Startup, readiness and liveness probes execute inside the Brainwire container;
they use its loopback listener and do not need a network-policy exception.
Administrative Bolt connections from other pods need the client label too.
Traffic originating on the node and control-plane access such as port-forward
can have different enforcement behavior; this is not a node-level firewall.

This policy does not restrict outbound traffic, encrypt Bolt traffic or open
peer replication ports. Do not reuse it as a quorum deployment policy. Shared
deployments still need a verified encrypted network boundary.

To remove only this ingress restriction, delete its policy explicitly:

```sh
kubectl delete -n YOUR_TEST_NAMESPACE networkpolicy brainwire-client-access
```

Applying a different overlay alone does not remove an existing NetworkPolicy.
Deleting the policy reopens access unless other policies restrict it. The
remaining single-pod disruption budget still requires a planned maintenance
procedure before voluntary eviction.
