import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";

const INVENTORY_PATH = resolve("licenses/cargo-license-inventory.json");
const NOTICES_PATH = resolve("THIRD_PARTY_NOTICES.txt");

export const APPROVED_LICENSES = [
  "MIT",
  "Apache-2.0",
  "CC0-1.0",
  "ISC",
  "Zlib",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "0BSD",
  "Unlicense",
  "Unicode-3.0",
  "Apache-2.0 WITH LLVM-exception",
  "CDLA-Permissive-2.0",
  "BSL-1.0",
  "MPL-2.0",
] as const;

const STANDARD_LICENSES = new Set<string>(["MIT", "Apache-2.0"]);
// Ported DiligentFX code retains its original component license inside the workspace.
const IMPORTED_WORKSPACE_LICENSES: Record<string, string> = {
  "sgl-post-fx": "Apache-2.0",
};
// Licences of code ported from other projects, kept beside it, by workspace package.
const BUNDLED_RENDERING_NOTICES: Record<string, string[]> = {
  "sgl-3d": [
    "src/stages/post/smaa/LICENSE-smaa.txt",
    "src/stages/post/smaa/LICENSE-three.txt",
    "src/stages/opaque/ambient_occlusion/reference/LICENSE",
    "src/LICENSE-amd-fidelityfx.txt",
    "src/LICENSE-bevy.txt",
    "src/LICENSE-filament.txt",
    "src/LICENSE-godot.txt",
    "src/LICENSE-ltc-code.txt",
    "src/LICENSE-wicked.txt",
  ],
  "sgl-post-fx": ["LICENSE-bevy.txt", "LICENSE-godot.txt"],
};
// These terms remain scoped to the dependency/component. MPL-2.0 additionally requires
// preserving its notices and source availability for MPL-covered files and modifications.
// Any unlisted term fails closed: in particular strong/network copyleft, restricted
// source-available, noncommercial/no-derivatives, proprietary, and unknown terms must never
// propagate obligations into unrelated SGL, game, or generated-output files.
const COMPATIBLE_COMPONENT_LICENSES = new Set<string>(APPROVED_LICENSES);

const CANONICAL_LICENSE_SOURCES: Record<string, { package: string; version: string; file: string }> = {
  "Apache-2.0": { package: "ahash", version: "0.8.12", file: "LICENSE-APACHE" },
  "BSD-2-Clause": { package: "arrayref", version: "0.3.9", file: "LICENSE" },
  "BSD-3-Clause": { package: "subtle", version: "2.6.1", file: "LICENSE" },
  "CC0-1.0": { package: "blake3", version: "1.8.6", file: "LICENSE_CC0" },
  ISC: { package: "libloading", version: "0.8.9", file: "LICENSE" },
  MIT: { package: "ahash", version: "0.8.12", file: "LICENSE-MIT" },
  "Apache-2.0 WITH LLVM-exception": {
    package: "winx",
    version: "0.36.4",
    file: "LICENSE",
  },
  "Unicode-3.0": { package: "unicode-ident", version: "1.0.24", file: "LICENSE-UNICODE" },
  Zlib: { package: "slotmap", version: "1.1.1", file: "LICENSE" },
};

type MetadataPackage = {
  id: string;
  name: string;
  version: string;
  source: string | null;
  license: string | null;
  license_file: string | null;
  manifest_path: string;
};

export type CargoMetadata = {
  packages: MetadataPackage[];
  workspace_members: string[];
  resolve: { nodes: Array<{ id: string }> } | null;
};

export type LicenseInventoryEntry = {
  name: string;
  version: string;
  source: string;
  declaredLicense: string;
  selectedLicenses: string[];
  policy: "standard" | "compatible-component" | "workspace-dual";
};

export type LicenseInventory = {
  version: 1;
  generatedFrom: "Cargo.lock via cargo metadata --locked --all-features --format-version 1";
  approvedLicenses: string[];
  packages: LicenseInventoryEntry[];
};

type Expression =
  | { kind: "license"; id: string }
  | { kind: "and" | "or"; left: Expression; right: Expression };

class LicenseExpressionParser {
  private index = 0;

  constructor(private readonly tokens: string[]) {}

  parse(): Expression {
    const expression = this.parseOr();
    if (this.index !== this.tokens.length) {
      throw new Error(`unexpected license token ${this.tokens[this.index]}`);
    }
    return expression;
  }

  private parseOr(): Expression {
    let expression = this.parseAnd();
    while (this.peek() === "OR" || this.peek() === "/") {
      this.index += 1;
      expression = { kind: "or", left: expression, right: this.parseAnd() };
    }
    return expression;
  }

  private parseAnd(): Expression {
    let expression = this.parsePrimary();
    while (this.peek() === "AND") {
      this.index += 1;
      expression = { kind: "and", left: expression, right: this.parsePrimary() };
    }
    return expression;
  }

  private parsePrimary(): Expression {
    if (this.peek() === "(") {
      this.index += 1;
      const expression = this.parseOr();
      if (this.peek() !== ")") {
        throw new Error("unclosed parenthesis in license expression");
      }
      this.index += 1;
      return expression;
    }
    const id = this.tokens[this.index];
    if (!id || [")", "AND", "OR", "/", "WITH"].includes(id)) {
      throw new Error(`expected a license identifier, found ${id ?? "end of expression"}`);
    }
    this.index += 1;
    if (this.peek() === "WITH") {
      this.index += 1;
      const exception = this.tokens[this.index];
      if (!exception || ["(", ")", "AND", "OR", "/", "WITH"].includes(exception)) {
        throw new Error("expected an SPDX exception after WITH");
      }
      this.index += 1;
      return { kind: "license", id: `${id} WITH ${exception}` };
    }
    return { kind: "license", id };
  }

  private peek(): string | undefined {
    return this.tokens[this.index];
  }
}

function parseLicenseExpression(value: string): Expression {
  const tokens = value.match(/\(|\)|\/|[A-Za-z0-9.+-]+/g) ?? [];
  if (tokens.length === 0) {
    throw new Error("empty license expression");
  }
  return new LicenseExpressionParser(tokens).parse();
}

function selectionScore(licenses: string[]): string {
  const priority = (license: string): number => {
    if (license === "MIT") return 0;
    if (license === "Apache-2.0") return 1;
    return 2 + APPROVED_LICENSES.indexOf(license as (typeof APPROVED_LICENSES)[number]);
  };
  return licenses
    .map((license) => `${String(priority(license)).padStart(2, "0")}:${license}`)
    .sort()
    .join("|");
}

function selectLicenses(expression: Expression, allowed: ReadonlySet<string>): string[] | null {
  if (expression.kind === "license") {
    return allowed.has(expression.id) ? [expression.id] : null;
  }
  const left = selectLicenses(expression.left, allowed);
  const right = selectLicenses(expression.right, allowed);
  if (expression.kind === "and") {
    if (!left || !right) return null;
    return [...new Set([...left, ...right])].sort();
  }
  if (!left) return right;
  if (!right) return left;
  return selectionScore(left) <= selectionScore(right) ? left : right;
}

function packageKey(package_: Pick<MetadataPackage, "name" | "version">): string {
  return `${package_.name}@${package_.version}`;
}

export function buildLicenseInventory(metadata: CargoMetadata): LicenseInventory {
  if (!metadata.resolve) {
    throw new Error("cargo metadata did not return a resolved locked graph");
  }
  const resolved = new Set(metadata.resolve.nodes.map((node) => node.id));
  const workspace = new Set(metadata.workspace_members);
  const failures: string[] = [];
  const packages: LicenseInventoryEntry[] = [];

  for (const package_ of metadata.packages.filter((item) => resolved.has(item.id))) {
    const key = packageKey(package_);
    if (workspace.has(package_.id)) {
      const importedLicense = IMPORTED_WORKSPACE_LICENSES[package_.name];
      if (importedLicense) {
        if (package_.source !== null || package_.license !== importedLicense) {
          failures.push(`${key}: imported workspace component must retain ${importedLicense}`);
          continue;
        }
        packages.push({
          name: package_.name,
          version: package_.version,
          source: "workspace",
          declaredLicense: importedLicense,
          selectedLicenses: [importedLicense],
          policy: "standard",
        });
        continue;
      }
      if (package_.source !== null || package_.license !== "MIT OR Apache-2.0") {
        failures.push(
          `${key}: workspace crates must be source=null and dual licensed MIT OR Apache-2.0`,
        );
        continue;
      }
      packages.push({
        name: package_.name,
        version: package_.version,
        source: "workspace",
        declaredLicense: "MIT OR Apache-2.0",
        selectedLicenses: ["MIT", "Apache-2.0"],
        policy: "workspace-dual",
      });
      continue;
    }
    if (package_.source === null) {
      failures.push(`${key}: non-workspace path dependency has no approved third-party policy`);
      continue;
    }
    const declaredLicense = package_.license;
    if (!declaredLicense) {
      failures.push(`${key}: missing declared license`);
      continue;
    }
    let selected: string[] | null = null;
    try {
      selected = selectLicenses(
        parseLicenseExpression(declaredLicense),
        COMPATIBLE_COMPONENT_LICENSES,
      );
    } catch (error) {
      failures.push(`${key}: could not parse ${JSON.stringify(declaredLicense)}: ${error}`);
      continue;
    }
    if (!selected) {
      failures.push(`${key}: unapproved license expression ${JSON.stringify(declaredLicense)}`);
      continue;
    }
    packages.push({
      name: package_.name,
      version: package_.version,
      source: package_.source,
      declaredLicense,
      selectedLicenses: selected,
      policy: selected.some((license) => !STANDARD_LICENSES.has(license))
        ? "compatible-component"
        : "standard",
    });
  }

  if (failures.length > 0) {
    throw new Error(`license policy rejected the locked graph:\n${failures.sort().join("\n")}`);
  }
  packages.sort(
    (left, right) =>
      left.name.localeCompare(right.name) ||
      left.version.localeCompare(right.version) ||
      left.source.localeCompare(right.source),
  );
  return {
    version: 1,
    generatedFrom: "Cargo.lock via cargo metadata --locked --all-features --format-version 1",
    approvedLicenses: [...APPROVED_LICENSES],
    packages,
  };
}

type NoticeText = {
  packages: string[];
  source: string;
  text: string;
};

function looksLikeNoticeFile(name: string): boolean {
  return /^(NOTICE|LICENSE-THIRD-PARTY)([-_.].*)?$/i.test(name);
}

function looksLikeLicenseFile(name: string): boolean {
  return /^(LICENSE|LICENCE|COPYING|UNLICENSE)([-_.].*)?$/i.test(name);
}

function detectedLicenseIds(name: string, text: string): Set<string> {
  const upperName = name.toUpperCase();
  const ids = new Set<string>();
  if (/APACHE/.test(upperName) || /Apache License\s+Version 2\.0/i.test(text)) ids.add("Apache-2.0");
  if (/LLVM[-_]EXCEPTION/.test(upperName) || /LLVM Exceptions to the Apache 2\.0 License/i.test(text)) {
    ids.add("Apache-2.0 WITH LLVM-exception");
  }
  if ((/(^|[-_.])MIT($|[-_.])/.test(upperName) && !/MIT0/.test(upperName)) || /Permission is hereby granted, free of charge/i.test(text)) ids.add("MIT");
  if (/CC0/.test(upperName) || /Creative Commons Zero v?1\.0/i.test(text)) ids.add("CC0-1.0");
  if (/ISC/.test(upperName) || /Permission to use, copy, modify, and\/or distribute this software/i.test(text)) ids.add("ISC");
  if (/ZLIB/.test(upperName) || /This software is provided ['‘]as-is['’]/i.test(text)) ids.add("Zlib");
  if (
    /BSD[-_.]?3/.test(upperName) ||
    (/Redistribution and use in source and binary forms/i.test(text) &&
      /Neither the name of the copyright holder nor the names of its contributors/i.test(text))
  ) {
    ids.add("BSD-3-Clause");
  } else if (/BSD/.test(upperName) || /Redistribution and use in source and binary forms/i.test(text)) {
    ids.add("BSD-2-Clause");
  }
  if (/0BSD/.test(upperName) || /BSD Zero Clause License/i.test(text)) ids.add("0BSD");
  if (/UNLICENSE/.test(upperName) || /This is free and unencumbered software released into the public domain/i.test(text)) ids.add("Unlicense");
  if (/UNICODE/.test(upperName) || /UNICODE LICENSE V3/i.test(text)) ids.add("Unicode-3.0");
  if (/CDLA/.test(upperName) || /Community Data License Agreement.*Permissive/i.test(text)) ids.add("CDLA-Permissive-2.0");
  if (/BSL/.test(upperName) || /Boost Software License - Version 1\.0/i.test(text)) ids.add("BSL-1.0");
  if (
    /(^|[-_.])MPL(?:[-_.]|$)/.test(upperName)
    || /Mozilla Public License Version 2\.0/i.test(text)
  ) {
    ids.add("MPL-2.0");
  }
  return ids;
}

export async function collectPackageLicenseTexts(
  package_: MetadataPackage,
  selectedLicenses: string[],
  metadataPackages: MetadataPackage[],
): Promise<Array<{ source: string; text: string }>> {
  const root = dirname(package_.manifest_path);
  const entries = (await readdir(root, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && looksLikeLicenseFile(entry.name))
    .map((entry) => entry.name)
    .sort();
  const selectedFiles = new Map<string, { source: string; text: string }>();
  const notices: Array<{ source: string; text: string }> = [];

  for (const name of entries) {
    const text = await readFile(resolve(root, name), "utf8");
    const detected = detectedLicenseIds(name, text);
    for (const license of selectedLicenses) {
      if (detected.has(license) && !selectedFiles.has(license)) {
        selectedFiles.set(license, { source: `${packageKey(package_)}/${name}`, text });
      }
    }
    if (looksLikeNoticeFile(name)) {
      notices.push({ source: `${packageKey(package_)}/${name}`, text });
    }
  }

  const noticeNames = (await readdir(root, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && looksLikeNoticeFile(entry.name) && !entries.includes(entry.name))
    .map((entry) => entry.name)
    .sort();
  for (const name of noticeNames) {
    notices.push({ source: `${packageKey(package_)}/${name}`, text: await readFile(resolve(root, name), "utf8") });
  }

  for (const license of selectedLicenses) {
    if (selectedFiles.has(license)) continue;
    const canonical = CANONICAL_LICENSE_SOURCES[license];
    if (!canonical) {
      throw new Error(`${packageKey(package_)}: no registry license-text source configured for ${license}`);
    }
    const sourcePackage = metadataPackages.find(
      (candidate) => candidate.name === canonical.package && candidate.version === canonical.version,
    );
    if (!sourcePackage || sourcePackage.source === null) {
      throw new Error(
        `${packageKey(package_)}: locked graph lacks registry text source ${canonical.package}@${canonical.version}`,
      );
    }
    const path = resolve(dirname(sourcePackage.manifest_path), canonical.file);
    let text: string;
    try {
      text = await readFile(path, "utf8");
    } catch {
      throw new Error(`${packageKey(package_)}: applicable ${license} text not found at ${path}`);
    }
    selectedFiles.set(license, {
      source: `${canonical.package}@${canonical.version}/${canonical.file}`,
      text,
    });
  }
  return [...selectedFiles.values(), ...notices];
}

function digestText(text: string): string {
  return new Bun.CryptoHasher("sha256").update(text).digest("hex");
}

export function normalizeNoticeText(text: string): string {
  return text
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")
    .trimEnd();
}

export function renderThirdPartyNotices(inventory: LicenseInventory, texts: NoticeText[]): string {
  const coveredPackages = new Set(texts.flatMap((item) => item.packages));
  const missingText = inventory.packages
    .filter((entry) => entry.policy !== "workspace-dual")
    .map((entry) => `${entry.name}@${entry.version}`)
    .filter((key) => !coveredPackages.has(key));
  if (missingText.length > 0) {
    throw new Error(`applicable license text not found for: ${missingText.join(", ")}`);
  }
  const lines = [
    "THIRD-PARTY NOTICES FOR STEVE'S GAME LIBRARY",
    "",
    "This file covers third-party Rust packages and bundled rendering sources in the SGL SDK.",
    "Game code, scripts, artwork, audio, fonts, maps, and other game-owned assets remain",
    "independently licensed by their owners and are not relicensed by this notice file.",
    "Game distributors must add any notices required by their own code and assets.",
    "",
    "Locked package inventory",
    "========================",
    "",
  ];
  for (const package_ of inventory.packages.filter((entry) => entry.policy !== "workspace-dual")) {
    lines.push(
      `${package_.name} ${package_.version} — ${package_.selectedLicenses.join(" AND ")} (declared: ${package_.declaredLicense})`,
    );
  }
  lines.push("", "Exact license and notice texts", "==============================", "");
  for (const item of [...texts].sort((left, right) => left.source.localeCompare(right.source))) {
    lines.push(
      `--- ${item.source}`,
      `Applies to: ${[...item.packages].sort().join(", ")}`,
      "",
      normalizeNoticeText(item.text),
      "",
    );
  }
  return `${lines.join("\n").trimEnd()}\n`;
}

async function generateArtifacts(metadata: CargoMetadata): Promise<{ inventory: string; notices: string }> {
  const inventory = buildLicenseInventory(metadata);
  const metadataByKey = new Map(metadata.packages.map((package_) => [packageKey(package_), package_]));
  const grouped = new Map<string, NoticeText>();
  for (const entry of inventory.packages.filter((package_) => package_.policy !== "workspace-dual")) {
    const package_ = metadataByKey.get(`${entry.name}@${entry.version}`);
    if (!package_) throw new Error(`metadata package disappeared: ${entry.name}@${entry.version}`);
    const files = await collectPackageLicenseTexts(package_, entry.selectedLicenses, metadata.packages);
    if (files.length === 0) throw new Error(`${entry.name}@${entry.version}: no applicable license text found`);
    for (const file of files) {
      const normalizedText = normalizeNoticeText(file.text);
      const key = `${digestText(normalizedText)}:${file.source.split("/").at(-1)}`;
      const existing = grouped.get(key);
      if (existing) {
        existing.packages.push(`${entry.name}@${entry.version}`);
      } else {
        grouped.set(key, {
          packages: [`${entry.name}@${entry.version}`],
          source: file.source,
          text: normalizedText,
        });
      }
    }
  }
  for (const [name, paths] of Object.entries(BUNDLED_RENDERING_NOTICES)) {
    const renderer = metadata.packages.find((package_) => package_.name === name);
    if (!renderer) continue;
    for (const path of paths) {
      const text = await readFile(resolve(dirname(renderer.manifest_path), path), "utf8");
      grouped.set(`${name}/${path}`, {
        packages: [packageKey(renderer)],
        source: `${packageKey(renderer)}/${path}`,
        text,
      });
    }
  }
  return {
    inventory: `${JSON.stringify(inventory, null, 2)}\n`,
    notices: renderThirdPartyNotices(inventory, [...grouped.values()]),
  };
}

async function cargoMetadata(): Promise<CargoMetadata> {
  const process_ = Bun.spawn(
    ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"],
    { stdout: "pipe", stderr: "pipe" },
  );
  const [exitCode, stdout, stderr] = await Promise.all([
    process_.exited,
    new Response(process_.stdout).text(),
    new Response(process_.stderr).text(),
  ]);
  if (exitCode !== 0) throw new Error(`cargo metadata failed: ${stderr.trim()}`);
  return JSON.parse(stdout) as CargoMetadata;
}

async function main(): Promise<void> {
  const [task] = Bun.argv.slice(2);
  if (task !== "generate" && task !== "check") {
    throw new Error("usage: bun scripts/license-notices.ts <generate|check>");
  }
  const artifacts = await generateArtifacts(await cargoMetadata());
  if (task === "generate") {
    await mkdir(dirname(INVENTORY_PATH), { recursive: true });
    await writeFile(INVENTORY_PATH, artifacts.inventory, "utf8");
    await writeFile(NOTICES_PATH, artifacts.notices, "utf8");
    return;
  }
  const stale: string[] = [];
  for (const [path, expected] of [
    [INVENTORY_PATH, artifacts.inventory],
    [NOTICES_PATH, artifacts.notices],
  ] as const) {
    let actual: string | undefined;
    try {
      actual = await readFile(path, "utf8");
    } catch {
      // Report a stable repository-relative artifact name, not a machine path.
    }
    if (actual !== expected) stale.push(basename(path));
  }
  if (stale.length > 0) {
    throw new Error(
      `generated license artifacts are missing or stale: ${stale.join(", ")}; run bun scripts/license-notices.ts generate`,
    );
  }
}

if (import.meta.main) await main();
