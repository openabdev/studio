// Pure translation of the deploy wizard's collected fields into the single
// `deploy_provision_agent` argument object (`deploy.ts`'s submit handler calls
// exactly one, so the panel stays a single step — studio#111).
//
// Why this is a module and not an inline object literal: everything the
// sidecar needs to run its create-or-redeploy branch travels in that one
// object, and on the *first* deploy there is nothing else to read it from.
// `fleets.toml` — the only other carrier of the operator's answers — is
// written strictly *after* a confirmed successful provision (ADR-83 §7.5), so
// at create time the region/profile the identity step collected exist nowhere
// but here. Drop them and `oab-mcp` falls back to the ambient `[default]`
// credential chain: it resolves a fleet's credential by the fleet a call names
// (`fleet:`), else by a binding's `cluster` key, and this wizard names neither
// (it passes no `fleet`, and its writer emits no `cluster` key) — so the first
// manifest's
// `default_networking` VPC/subnet/security-group discovery (studio#111's
// create-from-scratch defaults, ported from `oabctl create`'s wizard) and the
// apply itself would both run against whichever account the ambient chain
// happens to resolve, while `fleets.toml` goes on to record the region the
// operator actually picked.
//
// Pure and side-effect-free, like `fleetToml.ts` — the DOM reads stay in
// `deploy.ts`, this only shapes what they collected.

/** The k8s placement fields the submit sends alongside `provider: "k8s"`. */
export interface K8sPlacement {
  /** Kubeconfig context; omitted leaves the kubeconfig current-context in play. */
  context?: string;
  /** `system:serviceaccount:<ns>:<name>`, or omitted for the namespace default. */
  expectedPrincipal?: string;
}

/** Everything the wizard collected for one agent, as read off the form. */
export interface ProvisionAgentInput {
  image: string;
  name: string;
  namespace: string;
  apiKey?: string;
  chatPlatform?: string;
  chatBotToken?: string;
  chatChannelSecret?: string;
  acpEnabled: boolean;
  acpToken?: string;
  localConfigFolder?: string;
  /** ECS only — the identity step's Region field. */
  region?: string;
  /** ECS only — the identity step's Credential profile field. */
  profile?: string;
  /** Present ⇔ this submit targets k8s (studio#104/#153). */
  k8s?: K8sPlacement;
}

/**
 * Where one submit's AWS identity comes from — the same split the panel's
 * `DeployMode` makes for k8s placement (studio#153), for the same reason:
 * `new-fleet` collects the answers in its own identity step, `add-instance`
 * inherits them from the fleet it is adding to.
 */
export type AwsIdentitySource =
  | { kind: "new-fleet"; region: string; profile: string }
  | { kind: "add-instance"; region: string | null; profile: string | null };

/**
 * The AWS identity a submit should deploy under — trimmed, never invented.
 *
 * `add-instance` reads the *fleet's* recorded pair, not an empty form (the
 * panel has no AWS field group in that mode), and a fleet with nothing
 * recorded yields `undefined` for both, i.e. "no override", so the sidecar
 * keeps resolving the credential itself rather than being handed blanks.
 */
export function awsIdentityFor(source: AwsIdentitySource): {
  region?: string;
  profile?: string;
} {
  const [region, profile] =
    source.kind === "new-fleet"
      ? [source.region, source.profile]
      : [source.region ?? "", source.profile ?? ""];
  return { region: orUndefined(region), profile: orUndefined(profile) };
}

/**
 * Trimmed value, or `undefined` for anything blank.
 *
 * Trimming is a behavior change for the four fields that used to cross the wire
 * verbatim — `chat_platform`, `local_config_folder`, and the k8s pair
 * `context`/`expected_principal` — and is intentional in each: a padded platform
 * name or kubeconfig context selects neither, a padded path names a different
 * directory, and a padded `expected_principal` matches no principal. No in-app
 * source produces padding (a `<select>` value, the native directory picker
 * `main.ts`'s Config-folder setting uses, and a `fleets.toml` an operator could
 * hand-edit) — but a hand-edited file can.
 */
function orUndefined(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

/**
 * The `deploy_provision_agent` arguments for one agent deploy.
 *
 * `acp_enabled` is passed verbatim rather than omitted when off — the sidecar's
 * default-when-absent rule (studio#119/128) is "on", so an ACP-off agent has
 * to say so explicitly.
 *
 * Every other optional field goes over the wire omitted rather than as `""`:
 * the sidecar's `context` fallback to the kubeconfig current-context keys off
 * absence specifically, and its `region`/`profile` handling treats a blank as
 * unset (`with_identity` / `has_identity_override`) — so `""` is at best
 * redundant and at worst a literal blank region. Omitting is also what keeps
 * `region`/`profile` from being interpreted as "the operator asked for this".
 */
export function provisionAgentArgs(input: ProvisionAgentInput): Record<string, unknown> {
  const args: Record<string, unknown> = {
    image: input.image,
    name: input.name,
    namespace: input.namespace,
    acp_enabled: input.acpEnabled,
  };

  const apiKey = orUndefined(input.apiKey);
  if (apiKey) args.api_key = apiKey;
  const chatPlatform = orUndefined(input.chatPlatform);
  if (chatPlatform) args.chat_platform = chatPlatform;
  const chatBotToken = orUndefined(input.chatBotToken);
  if (chatBotToken) args.chat_bot_token = chatBotToken;
  const chatChannelSecret = orUndefined(input.chatChannelSecret);
  if (chatChannelSecret) args.chat_channel_secret = chatChannelSecret;
  // ACP-off: the sidecar provisions no ACP secret at all, so an operator-typed
  // token has nowhere to land — never carry it.
  const acpToken = input.acpEnabled ? orUndefined(input.acpToken) : undefined;
  if (acpToken) args.acp_token = acpToken;
  const localConfigFolder = orUndefined(input.localConfigFolder);
  if (localConfigFolder) args.local_config_folder = localConfigFolder;

  if (input.k8s) {
    args.provider = "k8s";
    const context = orUndefined(input.k8s.context);
    if (context) args.context = context;
    const expectedPrincipal = orUndefined(input.k8s.expectedPrincipal);
    if (expectedPrincipal) args.expected_principal = expectedPrincipal;
    // AWS credentials are deliberately not sent on a k8s submit: a k8s pod has
    // no AWS credential chain (studio#104/#128), so a region/profile left over
    // from the identity step's AWS field group could only mislead the sidecar.
    return args;
  }

  const region = orUndefined(input.region);
  if (region) args.region = region;
  const profile = orUndefined(input.profile);
  if (profile) args.profile = profile;
  return args;
}
