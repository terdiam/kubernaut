//! Starting points for creating a resource.
//!
//! Minimal but complete: each applies as written once its name is changed, so
//! the first thing someone does is edit values rather than hunt for a missing
//! required field. Anything not listed still works, because the editor takes
//! any manifest and the plan is checked against the cluster's own API.

pub struct Template {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub yaml: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "web",
        label: "Web app (Deployment + Service + Ingress)",
        group: "Bundles",
        yaml: r##"apiVersion: apps/v1
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
---
apiVersion: v1
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
---
apiVersion: networking.k8s.io/v1
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
"##,
    },
    Template {
        id: "deployment",
        label: "Deployment",
        group: "Workloads",
        yaml: r##"apiVersion: apps/v1
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
"##,
    },
    Template {
        id: "statefulset",
        label: "StatefulSet",
        group: "Workloads",
        yaml: r##"apiVersion: apps/v1
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
"##,
    },
    Template {
        id: "daemonset",
        label: "DaemonSet",
        group: "Workloads",
        yaml: r##"apiVersion: apps/v1
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
"##,
    },
    Template {
        id: "job",
        label: "Job",
        group: "Workloads",
        yaml: r##"apiVersion: batch/v1
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
"##,
    },
    Template {
        id: "cronjob",
        label: "CronJob",
        group: "Workloads",
        yaml: r##"apiVersion: batch/v1
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
"##,
    },
    Template {
        id: "service",
        label: "Service",
        group: "Networking",
        yaml: r##"apiVersion: v1
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
"##,
    },
    Template {
        id: "ingress",
        label: "Ingress",
        group: "Networking",
        yaml: r##"apiVersion: networking.k8s.io/v1
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
"##,
    },
    Template {
        id: "networkpolicy",
        label: "NetworkPolicy (default deny ingress)",
        group: "Networking",
        yaml: r##"apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: default-deny-ingress
spec:
  # An empty podSelector selects every pod in the namespace.
  podSelector: {}
  policyTypes: [Ingress]
"##,
    },
    Template {
        id: "configmap",
        label: "ConfigMap",
        group: "Config",
        yaml: r##"apiVersion: v1
kind: ConfigMap
metadata:
  name: example
data:
  SETTING: value
"##,
    },
    Template {
        id: "secret",
        label: "Secret",
        group: "Config",
        yaml: r##"apiVersion: v1
kind: Secret
metadata:
  name: example
type: Opaque
# stringData takes plain text; the apiserver base64-encodes it into data.
stringData:
  PASSWORD: change-me
"##,
    },
    Template {
        id: "pvc",
        label: "PersistentVolumeClaim",
        group: "Config",
        yaml: r##"apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: example
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 10Gi
"##,
    },
    Template {
        id: "serviceaccount",
        label: "ServiceAccount",
        group: "Access",
        yaml: r##"apiVersion: v1
kind: ServiceAccount
metadata:
  name: example
"##,
    },
    Template {
        id: "rolebinding",
        label: "Role + RoleBinding",
        group: "Access",
        yaml: r##"apiVersion: rbac.authorization.k8s.io/v1
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
"##,
    },
    Template {
        id: "hpa",
        label: "HorizontalPodAutoscaler",
        group: "Scaling",
        yaml: r##"apiVersion: autoscaling/v2
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
"##,
    },
    Template {
        id: "namespace",
        label: "Namespace",
        group: "Cluster",
        yaml: r##"apiVersion: v1
kind: Namespace
metadata:
  name: example
"##,
    },
];

/// A manifest to start from for a kind, or the smallest document the apiserver
/// accepts for it, which is what makes this work for custom resources no list
/// could cover.
pub fn for_kind(api_version: &str, kind: &str) -> String {
    let heading = format!("kind: {kind}");
    TEMPLATES
        .iter()
        // A bundle holds several kinds; only a single-document template stands
        // for the kind itself.
        .find(|template| {
            !template.yaml.contains("\n---\n") && template.yaml.lines().nth(1) == Some(&heading)
        })
        .map_or_else(
            || format!("apiVersion: {api_version}\nkind: {kind}\nmetadata:\n  name: example\n"),
            |template| template.yaml.to_string(),
        )
}

#[cfg(test)]
mod tests {
    use super::{TEMPLATES, for_kind};

    #[test]
    fn every_template_names_what_it_creates() {
        for template in TEMPLATES {
            for document in template.yaml.split("\n---\n") {
                assert!(document.contains("apiVersion: "), "{}", template.id);
                assert!(document.contains("kind: "), "{}", template.id);
                assert!(document.contains("name: "), "{}", template.id);
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = TEMPLATES.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TEMPLATES.len());
    }

    #[test]
    fn a_known_kind_gets_its_template_and_an_unknown_one_a_minimal_document() {
        assert!(for_kind("apps/v1", "Deployment").contains("replicas"));
        assert!(!for_kind("apps/v1", "Deployment").contains("kind: Service"));
        assert_eq!(
            for_kind("example.io/v1", "Widget"),
            "apiVersion: example.io/v1\nkind: Widget\nmetadata:\n  name: example\n"
        );
    }
}
