// Crate dependency boundaries from the resolved cargo graph (architecture.md,
// netcode.md, rendering.md). The graph comes from `cargo metadata`, so a
// boundary breaks only when cargo would actually link the crate — not when a
// name appears in a manifest.

export type ResolvedMetadata = {
  packages: Array<{ id: string; name: string; version: string }>;
  workspace_members: string[];
  resolve: {
    nodes: Array<{
      id: string;
      deps: Array<{ pkg: string; dep_kinds: Array<{ kind: string | null }> }>;
    }>;
  } | null;
};

/// Names of every package reachable from `root` through normal (non-dev,
/// non-build) dependency edges, excluding `root` itself.
export function normalDependencies(metadata: ResolvedMetadata, root: string): Set<string> {
  if (!metadata.resolve) throw new Error("cargo metadata did not return a resolved graph");
  const names = new Map(metadata.packages.map((p) => [p.id, p.name]));
  const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
  const rootId = metadata.workspace_members.find((id) => names.get(id) === root);
  if (!rootId) throw new Error(`${root} is not a workspace member`);
  const seen = new Set<string>();
  const stack = [rootId];
  while (stack.length > 0) {
    const node = nodes.get(stack.pop()!);
    if (!node) continue;
    for (const dep of node.deps) {
      if (!dep.dep_kinds.some((k) => k.kind === null)) continue;
      if (seen.has(dep.pkg)) continue;
      seen.add(dep.pkg);
      stack.push(dep.pkg);
    }
  }
  return new Set([...seen].map((id) => names.get(id) ?? id));
}

/// Packages that resolve to more than one version anywhere in the graph.
export function duplicateVersions(metadata: ResolvedMetadata): Map<string, string[]> {
  const versions = new Map<string, Set<string>>();
  for (const p of metadata.packages) {
    versions.set(p.name, (versions.get(p.name) ?? new Set()).add(p.version));
  }
  return new Map(
    [...versions].filter(([, v]) => v.size > 1).map(([name, v]) => [name, [...v].sort()]),
  );
}

export async function cargoMetadata(platform: string): Promise<ResolvedMetadata> {
  const proc = Bun.spawn(
    ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", platform],
    { stdout: "pipe", stderr: "pipe" },
  );
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  if (exitCode !== 0) throw new Error(`cargo metadata failed: ${stderr.trim()}`);
  return JSON.parse(stdout) as ResolvedMetadata;
}

export async function hostTriple(): Promise<string> {
  const proc = Bun.spawn(["rustc", "-vV"], { stdout: "pipe" });
  const text = await new Response(proc.stdout).text();
  const host = text.match(/^host: (\S+)$/m)?.[1];
  if (!host) throw new Error("rustc -vV did not report a host triple");
  return host;
}
