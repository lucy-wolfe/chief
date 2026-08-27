# Organization specification

The shape `chiefd-core`'s `store::organization_spec` parses and validates. It
moved here out of a skill directory that no person could ever be assigned —
`isForbiddenLauncherResource` refused that directory by name and the resource
catalog excluded it on every build — so the one live reader of this file,
`README.md`, was linking into a tree the product had already retired. The
validator is the authority; keep the fields below in step with it.

```json
{
  "name": "Northstar Capital",
  "purpose": "Operate a disciplined multi-strategy hedge fund.",
  "ceo": {
    "name": "Avery",
    "mandate": "Set risk, capital-allocation, and company direction."
  },
  "departments": [
    {
      "name": "Quant",
      "purpose": "Research and validate systematic strategies.",
      "head": { "name": "Quinn" },
      "staff": [
        { "name": "Signal Researcher", "taskClass": "research" }
      ]
    },
    {
      "name": "Engineering",
      "purpose": "Build reliable software for the organization.",
      "head": { "name": "Morgan" }
    },
    {
      "name": "IT",
      "purpose": "Release and operate internal products.",
      "head": { "name": "Ira" }
    },
    {
      "name": "Launch Audit",
      "purpose": "Perform one bounded production-readiness audit.",
      "kind": "contract",
      "transient": {
        "engagement": "Audit the first production release.",
        "expiresAt": "2026-08-01T00:00:00.000Z"
      },
      "head": { "name": "Alex" }
    }
  ]
}
```

The implicit root unit is kind `company`. Child entries default to durable kind `department`; set `kind` to `contract` and provide `transient.engagement` for bounded contractor work. Either child kind may contain nested `departments`; that field means recursive child units, not that every child must have durable department semantics. Derive each person's required capabilities from their owned outputs before choosing a capable model. A person's model needs no written justification, and neither does an explicitly assigned skill, extension, or package — the latter needs only its exact installed id. Do not add capabilities “just in case” or omit one the mandate requires. There is no approval tier and no exception tier: every model is available to a caller who may act on the person. Ordinary workers begin benched unless `startActive` is true; heads and the CEO begin active.
