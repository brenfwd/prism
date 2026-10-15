# REST API

The API lives under `/api/v1`. The OpenAPI 3.1 description is served by every
node at `GET /api/v1/openapi.json`; it is built from the operation table in
`crates/prism/src/api/openapi.rs`, which must be updated when routes change.
Errors are RFC 9457 problem details (`application/problem+json`).

## Rolling upgrades

Each node advertises `{version, feature_level}`; `GET /api/v1/cluster` reports
`supported_feature_level` beside the cluster's level. Join and accept are
refused when levels differ by more than one (`version_skew`). New `Command`
variants are gated with `feature_enabled(cluster_level, required)` and appear
only after every node supports them and the level is raised. Upgrade learners
and spokes first, then voters, then the hub.
