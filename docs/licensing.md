# Shipping licence notices

MIT and Apache-2.0 permit closed-source commercial games, but require preserving
applicable licence notices. This normally means a text file distributed with the
game, not an on-screen credit or a requirement to publish game code. See the
[MIT terms](https://opensource.org/license/mit) and
[Apache redistribution terms](https://www.apache.org/licenses/LICENSE-2.0).
Identical licence texts can be shared; distinct copyright notices must survive.

## One file for a game

[DISTRIBUTION_NOTICES.txt](../DISTRIBUTION_NOTICES.txt) is a collected bundle for
this repository's locked workspace, across all features and platforms. It is a
convenience snapshot, not a universal licence file for every game: a game's
lockfile, selected crates, features and target can resolve different code.

Generate the game's combined file using SGL's helper. Install the development
CLI once (it is not a game dependency):

```sh
cargo install cargo-about --features cli --locked
```

From the game directory, using an SGL checkout matching the game's release:

```sh
bun /path/to/sgl/scripts/distribution-notices.ts THIRD_PARTY_NOTICES.txt \
  --manifest-path Cargo.toml --target x86_64-pc-windows-msvc
```

Use the actual target triple and the same `--features`/`--no-default-features`
selection as the build. Multiple `--target` arguments can cover one bundle's
platforms. Use the game's package manifest rather than `--workspace` to avoid
unrelated workspace tools. Private game packages should have `publish = false`.
The command reads the existing lockfile and does not update dependencies.

The helper uses [cargo-about](https://embarkstudios.github.io/cargo-about/) for
transitive dependency and licence resolution, combines identical texts, adds
package `NOTICE`/`COPYRIGHT` files and SGL's bundled-code notices, and preserves
known upstream attribution omitted from published crates. Development-only
crates are excluded. Build dependencies are retained conservatively because
some contribute generated code or linked native code. Cargo metadata cannot
identify every piece of code surviving compilation and linking.

If a licence cannot be resolved, or an attribution-bearing licence has only a
generic template, resolve it from that package's original sources. Use
cargo-about `--config` clarifications or update SGL's pinned
[supplemental notices](../licenses/supplemental-notices.json) for the exact
package version. Never invent copyright holders, suppress a missing notice, or
reuse a different package's MIT copyright statement. A known upstream omission
is recorded explicitly for `dispatch`; its declared MIT terms and author credit
are retained without inventing an absent copyright statement.

## Instructions for game agents

When creating or maintaining a game, carry these rules into the game's own
`AGENTS.md` and its existing packaging workflow:

- Generate notices from the game's locked dependencies for its release targets
  and features. Refresh them when dependencies, copied code or shipped assets
  change. Preserve the source notices supplied with SGL and other libraries.
- Keep notices for non-Cargo material too: fonts, artwork, music, native SDKs,
  JavaScript packages, and copied/generated code with attribution obligations.
  Keep these in a separate maintained asset-notices file so regeneration cannot
  erase them; append it to the generated bundle or ship it alongside that file.
- Include the resulting text files in the actual game distribution. For native
  games, place them in the installed game/archive; for browsers, publish them
  with the site and provide an accessible licence link.
- Inspect the final archive, installer or deployed browser files to confirm the
  notices are present. A file existing only in the source repository is not
  enough. Do not add branding, splash screens or source-publication requirements
  that the applicable licences do not impose.
- Read licence-specific obligations too. A notice bundle does not replace any
  required source availability (for example, for MPL-covered files), modification
  notices, or restrictions on using reserved font names.

## Maintaining SGL

[THIRD_PARTY_NOTICES.txt](../THIRD_PARTY_NOTICES.txt) covers code and assets copied
into SGL. The 3D and post-effects crates also carry package-local copies so their
notices travel through crates.io. Normal Cargo dependencies belong in the
separate distribution bundle, not the source-notices list.

[upstreams.json](../licenses/upstreams.json) records each original licence file's
upstream revision, destination and SGL provenance prefix. On a port update,
change its pinned URL and provenance alongside the code. Retain notices for
older portions still present; add another entry when multiple licences apply.
A licence change on upstream `main` does not by itself justify replacing the
licence accompanying SGL's existing copy.

```sh
bun scripts/refresh-licenses.ts
```

This explicitly downloads the recorded licence revisions and supplemental
attribution, preserves SGL's provenance prefixes, then regenerates source and
package notices. It finishes all downloads before changing files; fetch or
excerpt errors leave existing files intact. Unchanged vendored files keep their
original bytes. Recorded upstream omissions are not replaced with guessed text.
Review any changed terms before adopting new upstream code; licence refresh is
not permission to port incompatible code.

For Cargo updates, `cargo-about` reads the newly resolved versions. Update
version-specific supplements or clarifications when needed, then regenerate the
convenience bundle:

```sh
bun scripts/distribution-notices.ts DISTRIBUTION_NOTICES.txt --workspace --all-features
```

Keep this work with the dependency/port update and describe any changed consumer
obligations in [CHANGELOG.md](../CHANGELOG.md). No downloads run at game startup,
and these commands are not added as release/deployment gates.
