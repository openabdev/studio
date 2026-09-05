// Pure text-level edits to fleets.toml's `[fleet.<name>]` blocks — the
// client-side half of ADR #83 §7.5's deploy flows. `fleet_config_write` has no
// partial/append primitive ("overwrites the operator's fleets.toml" per
// oab-mcp's tool description), so both `[+ New fleet]` (7.5.1) and `[+ Add
// instance]` (7.5.2) compute the new/edited TOML client-side and call
// `fleet_config_write` with the full updated text. Kept side-effect-free and
// regex-based (not a full TOML parser) so it's unit-testable and only ever
// touches the one array/block it means to.

// Exported so fleetsK8sToml.ts (fleets-k8s.toml's client-side edits, same
// `[fleet.<name>]` shape) can reuse it instead of duplicating a one-line
// helper.
export function quote(s: string): string {
  return JSON.stringify(s);
}

// Locate a `[fleet.<name>]` table's span within `text`. `headerEnd` is where
// the block's body starts (right after the header line); `end` is either the
// next top-level `[...]` table header or the end of the file. `null` if the
// fleet isn't present.
function findFleetBlock(
  text: string,
  name: string,
): { start: number; end: number; headerEnd: number } | null {
  const header = `[fleet.${name}]`;
  const start = text.indexOf(header);
  if (start === -1) return null;
  const headerEnd = start + header.length;
  const rest = text.slice(headerEnd);
  const next = rest.match(/\n\[/);
  const end = next && next.index !== undefined ? headerEnd + next.index + 1 : text.length;
  return { start, end, headerEnd };
}

// Whether a `[fleet.<name>]` block already exists — used by the "New fleet"
// wizard (deploy.ts) to reject a colliding name *before* provisioning an
// instance, rather than discovering the collision only when appendFleetBlock/
// appendK8sFleetBlock's blind append produces a second `[fleet.<name>]`
// header and the resulting TOML fails to parse (studio: duplicate-key crash
// after the instance was already deployed).
export function fleetBlockExists(text: string, name: string): boolean {
  return findFleetBlock(text, name) !== null;
}

// Append `member` to an existing fleet's `members = [...]` array (single-line
// TOML array — the only form fleets.toml is written in today, per the ADR's
// mockups). A no-op if the member is already listed or the fleet isn't found.
// If the block has no `members` line yet, one is inserted right after the
// header. `region`/`profile`/`expected_principal` are untouched (7.5.2:
// inherited from the fleet the operator already drilled into).
export function appendMember(text: string, fleetName: string, member: string): string {
  const block = findFleetBlock(text, fleetName);
  if (!block) return text;
  const body = text.slice(block.headerEnd, block.end);
  const arrayLine = body.match(/^([ \t]*members\s*=\s*)\[([^\]]*)\]/m);
  if (!arrayLine || arrayLine.index === undefined) {
    return `${text.slice(0, block.headerEnd)}\nmembers = [${quote(member)}]${body}${text.slice(block.end)}`;
  }
  const items = arrayLine[2]
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  const existing = items.map((s) => s.replace(/^["']|["']$/g, ""));
  if (existing.includes(member)) return text;
  items.push(quote(member));
  const newLine = `${arrayLine[1]}[${items.join(", ")}]`;
  const newBody = body.slice(0, arrayLine.index) + newLine + body.slice(arrayLine.index + arrayLine[0].length);
  return text.slice(0, block.headerEnd) + newBody + text.slice(block.end);
}

export interface NewFleetEntry {
  name: string;
  member: string;
  region: string | null;
  profile: string | null;
  expectedPrincipal: string | null;
}

// Append a brand-new `[fleet.<name>]` block to the end of the file (7.5.1 step
// 2), with the one member — the first instance just deployed. Optional fields
// are omitted rather than written as empty strings.
export function appendFleetBlock(text: string, entry: NewFleetEntry): string {
  const lines = [`[fleet.${entry.name}]`, `members = [${quote(entry.member)}]`];
  if (entry.region) lines.push(`region = ${quote(entry.region)}`);
  if (entry.profile) lines.push(`profile = ${quote(entry.profile)}`);
  if (entry.expectedPrincipal) lines.push(`expected_principal = ${quote(entry.expectedPrincipal)}`);
  const block = `${lines.join("\n")}\n`;
  const trimmed = text.replace(/\s*$/, "");
  return trimmed.length ? `${trimmed}\n\n${block}` : block;
}
