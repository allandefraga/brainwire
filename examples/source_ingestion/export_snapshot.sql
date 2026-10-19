-- Candidate synthetic source contract. Adapt the source table/view explicitly;
-- this is not an approved live external application or DuckLake schema.
-- All ticket rows must precede comment rows, including across fetched batches.
SELECT record_type, tenant_id, connector_id, external_id,
       subject, status, priority, tags, ticket_id, body, is_public
FROM source
ORDER BY CASE record_type WHEN 'ticket' THEN 0 ELSE 1 END,
         tenant_id, connector_id, external_id;
