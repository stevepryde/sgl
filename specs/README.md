# SGL specifications

These documents define the library's required behavior. For usage, start with
[Building games with SGL](../docs/README.md). For repository changes, read
[AGENTS.md](../AGENTS.md) and the contract for the boundary being changed.

| Boundary | Contract |
| --- | --- |
| Game/library ownership and crate dependencies | [Architecture](architecture.md) |
| Languages, runtime, and platform choices | [Stack](stack.md) |
| Deterministic helpers, time, animation | [Core](core.md) |
| Kinematic physics: shapes, worlds, queries, the mover | [Physics](physics.md) |
| 2D assets, text, UI, and tool composition | [Client building blocks](client.md) |
| 2D sprite and canvas rendering | [Rendering](rendering.md) |
| 3D behavior, rendering development, and roadmap | [SGL3D](sgl3d.md) |
| 3D layers, stages, contracts, and ownership | [SGL3D architecture](sgl3d-architecture.md) |
| Every rule that drops, weakens or withholds temporal history or motion | [SGL3D temporal history](sgl3d-history.md) |
| Controller input and lifecycle | [Input](input.md) |
| Payload transports and delivery guarantees | [Netcode](netcode.md) |
| Game-owned schemas and shared loaders | [Content](content.md) |
| Test oracles, target coverage, and required checks | [Testing](testing.md) |

[The audit log](audits.md) records what each audit and architecture pass
found, what was done and what now guards it; the
[architect skill](../.claude/skills/sgl-architect/SKILL.md) plans changes
against these contracts and runs those passes.

[Decisions](decisions.md) records why the design changed. It includes retired
crate names and superseded approaches; use the current contracts above for
implementation. Keep requirement and decision identifiers stable when editing,
so existing references remain meaningful.

When changing a contract, update its package documentation and affected
examples together. A roadmap item is planned work, not an available API;
consult the package guide and implementation before using it.
