import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  APPROVED_LICENSES,
  buildLicenseInventory,
  collectPackageLicenseTexts,
  normalizeNoticeText,
  renderThirdPartyNotices,
  type CargoMetadata,
} from "./license-notices";

function metadata(license: string): CargoMetadata {
  return {
    packages: [
      {
        id: "path+file:///repo#sgl-core@0.1.0",
        name: "sgl-core",
        version: "0.1.0",
        source: null,
        license: "MIT OR Apache-2.0",
        license_file: null,
        manifest_path: "/repo/crates/sgl-core/Cargo.toml",
      },
      {
        id: "registry+mock#fixture@1.2.3",
        name: "fixture",
        version: "1.2.3",
        source: "registry+mock",
        license,
        license_file: null,
        manifest_path: "/registry/fixture-1.2.3/Cargo.toml",
      },
    ],
    workspace_members: ["path+file:///repo#sgl-core@0.1.0"],
    resolve: {
      nodes: [
        { id: "registry+mock#fixture@1.2.3" },
        { id: "path+file:///repo#sgl-core@0.1.0" },
      ],
    },
  };
}

function externalPackage(metadata_: CargoMetadata, name: string, version: string): CargoMetadata {
  const package_ = metadata_.packages.find((item) => item.source !== null)!;
  const oldId = package_.id;
  package_.name = name;
  package_.version = version;
  package_.id = `registry+mock#${name}@${version}`;
  metadata_.resolve!.nodes.find((node) => node.id === oldId)!.id = package_.id;
  return metadata_;
}

test("locked inventory is deterministic and records dual-licensed workspace crates", () => {
  const first = buildLicenseInventory(metadata("Apache-2.0 OR MIT"));
  const secondMetadata = metadata("Apache-2.0 OR MIT");
  secondMetadata.packages.reverse();
  secondMetadata.resolve!.nodes.reverse();
  const second = buildLicenseInventory(secondMetadata);

  expect(JSON.stringify(first)).toBe(JSON.stringify(second));
  expect(first.approvedLicenses).toEqual([...APPROVED_LICENSES]);
  expect(first.packages).toEqual([
    {
      name: "fixture",
      version: "1.2.3",
      source: "registry+mock",
      declaredLicense: "Apache-2.0 OR MIT",
      selectedLicenses: ["MIT"],
      policy: "standard",
    },
    {
      name: "sgl-core",
      version: "0.1.0",
      source: "workspace",
      declaredLicense: "MIT OR Apache-2.0",
      selectedLicenses: ["MIT", "Apache-2.0"],
      policy: "workspace-dual",
    },
  ]);
});

test("strong copyleft, noncommercial, restricted source-available, unknown, and proprietary terms are rejected", () => {
  for (const license of [
    "GPL-3.0-only",
    "AGPL-3.0-only",
    "LGPL-2.1-only",
    "BUSL-1.1",
    "SSPL-1.0",
    "CC-BY-NC-4.0",
    "CC-BY-ND-4.0",
    "LicenseRef-Unknown",
    "Proprietary",
  ]) {
    expect(() => buildLicenseInventory(metadata(license))).toThrow(
      `fixture@1.2.3: unapproved license expression ${JSON.stringify(license)}`,
    );
  }
  expect(() => buildLicenseInventory(metadata("MIT AND GPL-3.0-only"))).toThrow(
    "fixture@1.2.3: unapproved license expression",
  );
});

test("MPL file-scoped copyleft is compatible without admitting strong or unknown terms", () => {
  const inventory = buildLicenseInventory(metadata("MPL-2.0"));
  expect(inventory.packages[0]).toMatchObject({
    selectedLicenses: ["MPL-2.0"],
    policy: "compatible-component",
  });
  expect(buildLicenseInventory(metadata("MIT AND MPL-2.0")).packages[0].selectedLicenses).toEqual([
    "MIT",
    "MPL-2.0",
  ]);
  expect(() => buildLicenseInventory(metadata("MPL-2.0 AND GPL-3.0-only"))).toThrow(
    "fixture@1.2.3: unapproved license expression",
  );
  expect(() => buildLicenseInventory(metadata("LicenseRef-Unknown"))).toThrow(
    "fixture@1.2.3: unapproved license expression",
  );
});

test("MPL text proof requires an exact filename token or canonical license text", async () => {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "sgl-mpl-license-test-"));
  const packageAt = (root: string) => ({
    id: "registry+mock#fixture@1.2.3",
    name: "fixture",
    version: "1.2.3",
    source: "registry+mock",
    license: "MPL-2.0",
    license_file: null,
    manifest_path: join(root, "Cargo.toml"),
  });
  try {
    const unrelatedRoot = join(temporaryRoot, "unrelated");
    await mkdir(unrelatedRoot);
    await writeFile(join(unrelatedRoot, "LICENSE-EXAMPLE"), "unrelated example terms\n");
    await expect(
      collectPackageLicenseTexts(packageAt(unrelatedRoot), ["MPL-2.0"], []),
    ).rejects.toThrow("fixture@1.2.3: no registry license-text source configured for MPL-2.0");

    const filenameRoot = join(temporaryRoot, "filename");
    await mkdir(filenameRoot);
    await writeFile(join(filenameRoot, "LICENSE-MPL-2.0"), "filename-identified terms\n");
    expect(await collectPackageLicenseTexts(packageAt(filenameRoot), ["MPL-2.0"], [])).toEqual([
      {
        source: "fixture@1.2.3/LICENSE-MPL-2.0",
        text: "filename-identified terms\n",
      },
    ]);

    const textRoot = join(temporaryRoot, "text");
    await mkdir(textRoot);
    const canonicalMarker = "Mozilla Public License Version 2.0\n";
    await writeFile(join(textRoot, "LICENSE-EXAMPLE"), canonicalMarker);
    expect(await collectPackageLicenseTexts(packageAt(textRoot), ["MPL-2.0"], [])).toEqual([
      {
        source: "fixture@1.2.3/LICENSE-EXAMPLE",
        text: canonicalMarker,
      },
    ]);
  } finally {
    await rm(temporaryRoot, { recursive: true });
  }
});

test("compatible component terms survive package updates without relicensing outputs", () => {
  const arrayref = buildLicenseInventory(externalPackage(metadata("BSD-2-Clause"), "arrayref", "0.3.9"));
  expect(arrayref.packages[0].selectedLicenses).toEqual(["BSD-2-Clause"]);
  expect(arrayref.packages[0].policy).toBe("compatible-component");
  expect(
    buildLicenseInventory(externalPackage(metadata("BSD-2-Clause"), "arrayref", "0.3.10"))
      .packages[0].selectedLicenses,
  ).toEqual(["BSD-2-Clause"]);

  const unicode = buildLicenseInventory(
    externalPackage(
      metadata("(MIT OR Apache-2.0) AND Unicode-3.0"),
      "unicode-ident",
      "1.0.24",
    ),
  );
  expect(unicode.packages[1].selectedLicenses).toEqual(["MIT", "Unicode-3.0"]);

  const winx = buildLicenseInventory(
    externalPackage(metadata("Apache-2.0 WITH LLVM-exception"), "winx", "0.36.4"),
  );
  expect(winx.packages[1].selectedLicenses).toEqual(["Apache-2.0 WITH LLVM-exception"]);

  const conjunctive = buildLicenseInventory(
    externalPackage(metadata("Apache-2.0 AND ISC"), "ring", "0.17.14"),
  );
  expect(conjunctive.packages[0].selectedLicenses).toEqual(["Apache-2.0", "ISC"]);
});

test("workspace crates cannot silently leave the dual MIT Apache policy", () => {
  const metadata_ = metadata("MIT");
  metadata_.packages.find((item) => item.source === null)!.license = "Proprietary";
  expect(() => buildLicenseInventory(metadata_)).toThrow(
    "workspace crates must be source=null and dual licensed MIT OR Apache-2.0",
  );
});

test("an approved alternative does not import an unapproved OR branch", () => {
  const inventory = buildLicenseInventory(metadata("MIT OR LGPL-2.1-or-later"));
  expect(inventory.packages[0].selectedLicenses).toEqual(["MIT"]);
});

test("third-party notices are deterministic and normalize transport whitespace only", () => {
  const inventory = buildLicenseInventory(metadata("MIT"));
  const texts = [
    {
      packages: ["fixture@1.2.3"],
      source: "fixture@1.2.3/LICENSE",
      text: "exact text  \r\nsecond line\t\r\n",
    },
    { packages: ["fixture@1.2.3"], source: "fixture@1.2.3/NOTICE", text: "exact notice\r" },
  ];
  const first = renderThirdPartyNotices(inventory, texts);
  const second = renderThirdPartyNotices(inventory, [...texts].reverse());
  expect(first).toBe(second);
  expect(first).toContain(
    "--- fixture@1.2.3/LICENSE\nApplies to: fixture@1.2.3\n\nexact text\nsecond line",
  );
  expect(first).toContain("--- fixture@1.2.3/NOTICE\nApplies to: fixture@1.2.3\n\nexact notice");
  expect(first).toContain("Game code, scripts, artwork, audio, fonts, maps");
  expect(first).not.toContain("\r");
  expect(first.split("\n").every((line) => line === line.trimEnd())).toBe(true);
  expect(normalizeNoticeText("a  \r\nb\t\r\n")).toBe("a\nb");
});

test("notice rendering fails when an applicable text is absent", () => {
  expect(() => renderThirdPartyNotices(buildLicenseInventory(metadata("MIT")), [])).toThrow(
    "applicable license text not found for: fixture@1.2.3",
  );
});
