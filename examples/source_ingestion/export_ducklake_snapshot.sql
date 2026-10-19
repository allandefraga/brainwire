-- Synthetic unified Ticket/Comment source table; parents precede comments.
SELECT * FROM source_lake.main.source
ORDER BY CASE record_type WHEN 'ticket' THEN 0 ELSE 1 END,
         tenant_id, connector_id, external_id;
