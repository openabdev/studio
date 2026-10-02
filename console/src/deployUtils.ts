export type AwsProfileResult = {
  fallbackToFreeText: boolean;
  statusClass: "warn" | "err" | null;
  statusMessage: string | null;
  options?: { value: string; label: string }[];
};

export type K8sContextResult = {
  fallbackToFreeText: boolean;
  statusClass: "warn" | "err" | null;
  statusMessage: string | null;
  options?: { value: string; label: string }[];
};

export type AwsProfileInput = {
  profiles: { name: string; region: string | null }[];
  exists: boolean;
  error: string | null;
  source_path: string;
};

export type K8sContextInput = {
  contexts: { name: string; cluster: string; namespace: string; user: string }[];
  current_context: string | null;
  exists: boolean;
  error: string | null;
};

export function processAwsProfiles(_input: AwsProfileInput): AwsProfileResult {
  return {
    fallbackToFreeText: true,
    statusClass: "warn",
    statusMessage: "STUB: AWS config not implemented",
    options: [],
  };
}

export function processK8sContexts(_input: K8sContextInput): K8sContextResult {
  return {
    fallbackToFreeText: true,
    statusClass: "warn",
    statusMessage: "STUB: kubeconfig not implemented",
    options: [],
  };
}