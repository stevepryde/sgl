import { readFile, readdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

// Run from the game directory. Cargo-about owns graph and SPDX resolution;
// this script adds notices that package-level SPDX expressions cannot represent.
const [output, ...args] = Bun.argv.slice(2);
if (!output || output.startsWith("-")) {
  throw new Error("usage: bun scripts/distribution-notices.ts OUTPUT [cargo-about generate options]");
}
const root = resolve(import.meta.dir, "..");
const customConfig = args.some((arg) => arg === "--config" || arg === "-c" || arg.startsWith("--config="));
const process_ = Bun.spawn([
  "cargo", "about", "generate", "--format", "json", "--locked", "--fail",
  ...(customConfig ? [] : ["--config", resolve(root, "licenses/about.toml")]),
  ...args,
], { stdout: "pipe", stderr: "inherit" });
const [status, json] = await Promise.all([process_.exited, new Response(process_.stdout).text()]);
if (status !== 0) process.exit(status);

type Package = { name: string; version: string; manifest_path: string; authors: string[] };
type License = { id: string; text: string; source_path: string | null; used_by: { crate: Package }[] };
type Supplement = { packages: string[]; source: string; text: string; note?: string };
const data: { licenses: License[]; crates: { package: Package; license: string }[] } = JSON.parse(json);
const supplements: Supplement[] = JSON.parse(await readFile(resolve(root, "licenses/supplemental-notices.json"), "utf8"));
const key = (p: Package) => `${p.name}@${p.version}`;
const packages = new Map(data.crates.filter((entry) => entry.license !== "Ignore")
  .map(({ package: p }) => [key(p), p]));
const texts = new Map<string, Set<string>>();
function add(text: string, applies: string[]) {
  const normalized = text.replace(/\r\n?/g, "\n").split("\n").map((line) => line.trimEnd()).join("\n").trim();
  const owners = texts.get(normalized) ?? new Set<string>();
  for (const name of applies) owners.add(name);
  texts.set(normalized, owners);
}
const canonicalTerms = new Set(["Apache-2.0", "CC0-1.0", "Unlicense", "0BSD"]);
for (const license of data.licenses) {
  if (license.source_path || canonicalTerms.has(license.id)) {
    add(license.text, license.used_by.map((use) => key(use.crate)));
  }
}

// Keep NOTICE/COPYRIGHT and bundled-port notices in addition to SPDX licences.
const originals = new Set<string>();
for (const [name, p] of packages) {
  const directory = dirname(p.manifest_path);
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (!entry.isFile() || !/^(copyright|notices?|third[_-]party[_-]notices|licen[cs]e[_-]third[_-]party)([._-]|$)/i.test(entry.name)) continue;
    add(await readFile(resolve(directory, entry.name), "utf8"), [name]);
  }
}
for (const supplement of supplements) {
  const applies = supplement.packages.filter((name) => packages.has(name));
  if (applies.length) {
    add(`${supplement.note ? `${supplement.note}\n` : ""}Source: ${supplement.source}\n\n${supplement.text}`, applies);
    for (const name of applies) originals.add(name);
  }
}
// A generic MIT/BSD/etc. template can lose the required copyright attribution.
// Apache and CC0 standard bodies do not require a per-package copyright line;
// package NOTICE files above are retained separately.
const unresolved = data.licenses.filter((license) => !license.source_path
  && !canonicalTerms.has(license.id))
  .flatMap((license) => license.used_by.map((use) => key(use.crate)))
  .filter((name) => !originals.has(name));
if (unresolved.length) {
  throw new Error(`Missing original attribution for ${[...new Set(unresolved)].join(", ")}. Resolve with cargo-about --config clarifications or pinned supplemental notices; output was not written.`);
}
const sections = [
  "DISTRIBUTION NOTICES",
  "Collected for the resolved Cargo packages listed below. Identical texts are shared.\nAdditional game assets and non-Cargo components need their own applicable notices.\nAlternative licence texts are retained as supplied; this does not require choosing both.",
  [...packages].sort(([a], [b]) => a.localeCompare(b)).map(([name, p]) =>
    `${name}${p.authors.length ? ` — authors: ${p.authors.join(", ")}` : ""}`).join("\n"),
];
for (const [text, owners] of texts) sections.push(`--- Applies to: ${[...owners].sort().join(", ")}\n\n${text}`);
// Use the copyright holder's preferred name, including in older published dependencies.
const notices = `${sections.join("\n\n")}\n`.replace(/\bSteve(?= Pryde\b)/g, "Stephen");
await writeFile(output, notices);
console.log(`Wrote ${output} (${packages.size} packages). Include it in the distributed game.`);
