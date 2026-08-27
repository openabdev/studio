// Pure text-level edits to fleets-k8s.toml's `[fleet.<name>]` blocks — the
// k8s counterpart to fleetToml.ts, same rationale (studio#104: k8s_fleet_
// config_write has no partial/append primitive, so the client computes the
// new/edited TOML and calls it with the full updated text).
//
// `[fleet.<name>]` block lookup/append-member is identical between fleets.
// toml and fleets-k8s.toml (same table shape, same `members = [...]` array —
// neither `findFleetBlock` nor `appendMember` reference any AWS-specific
// field), so this module reuses fleetToml.ts's `appendMember` rather than
// duplicating it. Only "create a brand-new fleet block" differs, since the
// two files' required/optional fields differ (context+namespace vs
// region+profile).

import { quote, appendMember } from "./fleetToml";

export { appendMember };

export interface NewK8sFleetEntry {
  name: string;
  member: string;
  context: string | null;
  namespace: string;
  expectedPrincipal: string | null;
}

// Append a brand-new `[fleet.<name>]` block to the end of the file, with the
// one member — the first instance just deployed. `context` and
// `expected_principal` are optional fields, omitted rather than written as
// empty strings (mirrors fleetToml.ts's appendFleetBlock).
export function appendK8sFleetBlock(text: string, entry: NewK8sFleetEntry): string {
  const lines = [`[fleet.${entry.name}]`];
  if (entry.context) lines.push(`context = ${quote(entry.context)}`);
  lines.push(`namespace = ${quote(entry.namespace)}`);
  lines.push(`members = [${quote(entry.member)}]`);
  if (entry.expectedPrincipal) lines.push(`expected_principal = ${quote(entry.expectedPrincipal)}`);
  const block = `${lines.join("\n")}\n`;
  const trimmed = text.replace(/\s*$/, "");
  return trimmed.length ? `${trimmed}\n\n${block}` : block;
}
