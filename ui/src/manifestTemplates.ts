/**
 * Starting points for creating a resource.
 *
 * Deliberately minimal and complete rather than exhaustive: every template
 * here applies as written once the name is changed, so the first thing a
 * reader does is edit values, not hunt for the required field they are
 * missing. Anything not covered still works — the editor takes any manifest,
 * and the plan is checked against the cluster's own API.
 */
export interface ManifestTemplate {
  id: string;
  label: string;
  /** Grouping in the picker. */
  group: string;
  /** Kind this template creates, when it creates exactly one. */
  kind?: string;
  yaml: string;
}

const DEPLOYMENT = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: example
  labels:
    app: example
spec:
  replicas: 2
  selector:
    matchLabels:
      app: example
  template:
    metadata:
      labels:
        app: example
    spec:
      containers:
        - name: app
          image: nginx:1.27
          ports:
            - containerPort: 8080
          resources:
            requests:
              cpu: 100m
              memory: 128Mi
            limits:
              memory: 256Mi
          readinessProbe:
            httpGet:
              path: /
              port: 8080
            initialDelaySeconds: 5
`;

const SERVICE = `apiVersion: v1
kind: Service
metadata:
  name: example
spec:
  type: ClusterIP
  selector:
    app: example
  ports:
    - name: http
      port: 80
      targetPort: 8080
`;

const INGRESS = `apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: example
spec:
  rules:
    - host: example.local
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: example
                port:
                  number: 80
`;

export const TEMPLATES: ManifestTemplate[] = [
  {
    id: "web",
    label: "Web app (Deployment + Service + Ingress)",
    group: "Bundles",
    // The three together, because deploying one of them alone is almost never
    // what someone means by "expose an app".
    yaml: `${DEPLOYMENT}---\n${SERVICE}---\n${INGRESS}`,
  },
  { id: "deployment", label: "Deployment", group: "Workloads", kind: "Deployment", yaml: DEPLOYMENT },
  {
    id: "statefulset",
    kind: "StatefulSet",
    label: "StatefulSet",
    group: "Workloads",
    yaml: `apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: example
spec:
  serviceName: example
  replicas: 1
  selector:
    matchLabels:
      app: example
  template:
    metadata:
      labels:
        app: example
    spec:
      containers:
        - name: app
          image: postgres:16
          volumeMounts:
            - name: data
              mountPath: /var/lib/postgresql/data
  volumeClaimTemplates:
    - metadata:
        name: data
      spec:
        accessModes: [ReadWriteOnce]
        resources:
          requests:
            storage: 10Gi
`,
  },
  {
    id: "daemonset",
    kind: "DaemonSet",
    label: "DaemonSet",
    group: "Workloads",
    yaml: `apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: example
spec:
  selector:
    matchLabels:
      app: example
  template:
    metadata:
      labels:
        app: example
    spec:
      containers:
        - name: agent
          image: busybox:1.36
          command: ["sh", "-c", "sleep infinity"]
`,
  },
  {
    id: "job",
    kind: "Job",
    label: "Job",
    group: "Workloads",
    yaml: `apiVersion: batch/v1
kind: Job
metadata:
  name: example
spec:
  # Without this the finished pod outlives its logs, and reading them later
  # fails with a message about a container the node has already collected.
  ttlSecondsAfterFinished: 86400
  backoffLimit: 3
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: task
          image: busybox:1.36
          command: ["sh", "-c", "echo done"]
`,
  },
  {
    id: "cronjob",
    kind: "CronJob",
    label: "CronJob",
    group: "Workloads",
    yaml: `apiVersion: batch/v1
kind: CronJob
metadata:
  name: example
spec:
  schedule: "0 2 * * *"
  successfulJobsHistoryLimit: 1
  failedJobsHistoryLimit: 1
  concurrencyPolicy: Forbid
  jobTemplate:
    spec:
      ttlSecondsAfterFinished: 86400
      template:
        spec:
          restartPolicy: Never
          containers:
            - name: task
              image: busybox:1.36
              command: ["sh", "-c", "echo done"]
`,
  },
  { id: "service", label: "Service", group: "Networking", kind: "Service", yaml: SERVICE },
  { id: "ingress", label: "Ingress", group: "Networking", kind: "Ingress", yaml: INGRESS },
  {
    id: "networkpolicy",
    kind: "NetworkPolicy",
    label: "NetworkPolicy (default deny ingress)",
    group: "Networking",
    yaml: `apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: default-deny-ingress
spec:
  # An empty podSelector selects every pod in the namespace.
  podSelector: {}
  policyTypes: [Ingress]
`,
  },
  {
    id: "configmap",
    kind: "ConfigMap",
    label: "ConfigMap",
    group: "Config",
    yaml: `apiVersion: v1
kind: ConfigMap
metadata:
  name: example
data:
  SETTING: value
`,
  },
  {
    id: "secret",
    kind: "Secret",
    label: "Secret",
    group: "Config",
    yaml: `apiVersion: v1
kind: Secret
metadata:
  name: example
type: Opaque
# stringData takes plain text; the apiserver base64-encodes it into data.
stringData:
  PASSWORD: change-me
`,
  },
  {
    id: "pvc",
    kind: "PersistentVolumeClaim",
    label: "PersistentVolumeClaim",
    group: "Config",
    yaml: `apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: example
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 10Gi
`,
  },
  {
    id: "serviceaccount",
    kind: "ServiceAccount",
    label: "ServiceAccount",
    group: "Access",
    yaml: `apiVersion: v1
kind: ServiceAccount
metadata:
  name: example
`,
  },
  {
    id: "rolebinding",
    label: "Role + RoleBinding",
    group: "Access",
    yaml: `apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: example-read
rules:
  - apiGroups: [""]
    resources: [pods, pods/log]
    verbs: [get, list, watch]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: example-read
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: example-read
subjects:
  - kind: ServiceAccount
    name: example
`,
  },
  {
    id: "hpa",
    kind: "HorizontalPodAutoscaler",
    label: "HorizontalPodAutoscaler",
    group: "Scaling",
    yaml: `apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: example
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: example
  minReplicas: 2
  maxReplicas: 10
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 70
`,
  },
  {
    id: "namespace",
    kind: "Namespace",
    label: "Namespace",
    group: "Cluster",
    yaml: `apiVersion: v1
kind: Namespace
metadata:
  name: example
`,
  },
];

/** Templates grouped for the picker, in declaration order. */
export function templateGroups(): { group: string; templates: ManifestTemplate[] }[] {
  const groups: { group: string; templates: ManifestTemplate[] }[] = [];
  for (const template of TEMPLATES) {
    const existing = groups.find((entry) => entry.group === template.group);
    if (existing) existing.templates.push(template);
    else groups.push({ group: template.group, templates: [template] });
  }
  return groups;
}

/**
 * Where to start when creating one of `kind`.
 *
 * A curated template when there is one; otherwise the smallest document the
 * apiserver will accept an apply for. The fallback is what makes this work for
 * CRDs, which no hand-written list can cover.
 */
export function templateForKind(apiVersion: string, kind: string): string {
  const template = TEMPLATES.find((entry) => entry.kind === kind);
  if (template) return template.yaml;
  return `apiVersion: ${apiVersion}\nkind: ${kind}\nmetadata:\n  name: example\n`;
}
