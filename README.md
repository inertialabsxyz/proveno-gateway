# proveno-gateway

An MCP gateway for [proveno](https://github.com/inertialabsxyz/proveno). Agents
submit Lua programs through one tool, `execute`. Every tool call a program makes
is policy-checked, has its credential injected by the gateway, is dispatched to
a downstream MCP server, and is recorded in a signed trace that replays
bit-for-bit with the network off.

The gateway makes no model calls: it receives programs, not tasks.

Depends on [proveno-core](https://github.com/inertialabsxyz/proveno-core) by git
tag.

```bash
make check
```

## Status

Skeleton. Nothing described above is built yet; the spec is
`planning/proveno-gateway-spec.md` in the umbrella repository.

## Licence

See [LICENSE](LICENSE).
