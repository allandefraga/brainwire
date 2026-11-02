# Describe graph types for applications and agents

Ontology mappings accept an optional `description` on each node and relationship
type. Use it to explain business meaning and help an application choose the right
records or relationships.

```json
{
  "version": "1",
  "nodes": [
    {"label": "Ticket", "description": "A support request, identified by its source ticket ID"}
  ],
  "edges": [
    {"type": "RELATED", "from": {"label": "Ticket"}, "to": {"label": "Ticket"},
     "description": "A documented connection between two support requests"}
  ]
}
```

Apply the ontology using the existing ontology apply command. Descriptions are
stored in the same versioned catalog transaction and survive restart. Inspect
node and relationship maps with:

```cypher
CALL db.schema() YIELD nodes, relationships RETURN nodes, relationships
```

Each map includes `description` when one is declared. Existing ontology and
catalog files without descriptions remain readable, and existing result maps
without descriptions retain their shape. Reapplying an ontology without a
description preserves an existing description; an explicit empty string replaces
it with empty text. Multiple declarations for one label or relationship type
must agree when they supply descriptions.

Each description may contain at most 4,096 UTF-8 bytes and cannot contain NUL.
Invalid declarations reject the ontology update. Normal schema inspection
permissions, output-size limits and query memory budgets apply.

Descriptions are application-supplied, untrusted text. They explain types; they
do not infer relationships, enforce business semantics, prove provenance or grant
tools and permissions to an agent. Catalog versions are not historical node or
relationship revisions.

Mixed-version quorum groups and downgrade after writing description metadata
have not been qualified. Upgrade all replicas with the same tested version;
older software may ignore and later remove metadata it does not understand.
