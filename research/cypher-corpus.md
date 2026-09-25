# Corpus de Cypher da plataforma

> Inventário feito em 2026-09-24 por um agente de busca, varrendo o monorepo.
> Serve para priorizar o parser e o executor do Brainwire. As contagens são
> templates distintos no código, não execuções: o repo não tem telemetria de
> tráfego. Caminhos com `arquivo:linha` valem para o commit `5dc9ef10b`.

## Conclusões principais

1. **Há dois caminhos, com dialetos diferentes.**
   - O interno (`execute`, `execute_batch`, `execute_pipeline`) não passa por
     gramática nenhuma e leva tudo o que o memory-worker e o memory-api escrevem
     à mão.
   - O governado (`run_read_cypher`) só aceita o subconjunto da gramática `lark`
     e injeta o predicado de ACL em todo nó casado. Por ele passam o
     `run_cypher` do MCP, o `/v1/memory/cypher` do console e parte das leituras
     do memory-api.
2. **Quase toda a escrita é `UNWIND $rows AS row` sobre três templates** (nó,
   aresta, evento). Os lotes têm 10.000 linhas no código e 250.000 no starter.
3. **O predicado de ACL depende de `any()`/`all()`** sobre lista com
   `coalesce(prop, [])`, `size()` e `IN $lista`. HydraDB e PuppyGraph ficaram
   fora da leitura governada por não avaliarem isso.
4. **As variáveis de iteração do predicado (`p`, `c`) têm o mesmo nome das
   variáveis que os modelos mais usam**, `(p:Person)` e `(c:Company)`. O motor
   precisa acertar o escopo: a lista do quantificador é avaliada fora dele.
   Nenhum teste do repo prova essa semântica.
5. **O `get_entity_context` busca `(e {entity_id: $entity_id})` sem label.** Nos
   backends atuais o índice é por label.
6. **Não aparecem em produção:** caminho de comprimento variável, shortestPath,
   `EXISTS {}`, `CALL` subquery (fora do wipe), `FOREACH`, `REMOVE`,
   `ON CREATE/ON MATCH SET`, `+=`, comprehensions, map projection, regex e
   funções de data. `CONTAINS`/`STARTS WITH`/`ENDS WITH` só aparecem no
   construtor de views do console, que a gramática recusa.

## Arquivos citados

| Apelido | Caminho |
|---|---|
| `common` | `client-core/src/client_core/repository/_cypher_common.py` |
| `neo4j` | `client-core/src/client_core/repository/neo4j.py` |
| `falkordb` | `client-core/src/client_core/repository/falkordb.py` |
| `hydradb` | `client-core/src/client_core/repository/hydradb.py` |
| `visibility` | `client-core/src/client_core/auth/visibility.py` |
| `lark` | `client-core/src/client_core/cypher/cypher_acl.lark` |
| `cypher_acl` | `client-core/src/client_core/cypher/cypher_acl.py` |
| `diagnose` | `client-core/src/client_core/cypher/diagnose.py` |
| `pii` | `client-core/src/client_core/pii.py` |
| `catalog` | `client-core/src/client_core/i18n/catalog.py` |
| `generator` | `client-ai/services/memory-worker/memory_worker/cypher_generator.py` |
| `graph_client` | `client-ai/services/memory-worker/memory_worker/graph_client.py` |
| `layout` | `client-ai/services/memory-worker/memory_worker/layout.py` |
| `pipeline` | `client-ai/services/memory-worker/memory_worker/pipeline.py` |
| `graph.py` | `client-ai/services/memory-api/src/memory_api/db/graph.py` |
| `memory.py` | `client-ai/services/memory-api/src/memory_api/api/v1/endpoints/memory.py` |
| `phantom.py` | `client-ai/services/memory-api/src/memory_api/api/v1/endpoints/phantom.py` |
| `query.py` | `client-ai/services/skills-api/src/skills_api/routers/query.py` |
| `mcp_server` | `client-ai/services/mcp-server/src/mcp_server/server.py` |
| `mcp_generic` | `client-ai/services/mcp-server/src/mcp_server/tools/generic.py` |
| `view_modal` | `client-console-ui/app/(app)/memory/components/toolbar/create-view-modal.tsx` |
| `fixture` | `client-core/tests/fixtures/cypher_sql_refusals.json` |
| `spec_err` | `docs/superpowers/specs/2026-09-15-mcp-tool-error-contract-design.md` |
| `gb/` | `client-brain/tools/graph-bench/` |
| `bench/` | `client-bench/` |

## 1. Cláusulas

| Cláusula | Templates em produção | Exemplo real |
|---|---|---|
| MATCH | ~33 | `MATCH (a:{from_label} {entity_id: $from_entity_id})` e `MATCH (b:{to_label} {entity_id: $to_entity_id})` generator:361-362 |
| OPTIONAL MATCH | 2 | `OPTIONAL MATCH (e)-[r{rel_pat}]-(neighbor) WHERE NOT neighbor:Event{acl_n}` graph.py:565-566 |
| WHERE | ~20, mais o que o rewriter injeta em todo MATCH governado | `WHERE NOT n:Event AND n.__schema_marker IS NULL` layout:257-258 |
| WITH | 0 | só em LLM e bench: `WITH n.event_id AS id, count(n) AS c WHERE c > 1` |
| UNWIND | 3 | `UNWIND $rows AS row` em todo lote (common:320-322); `UNWIND $items AS item MATCH (n) WHERE id(n) = item.id SET n.x = item.x` layout:485-487 |
| RETURN | ~24 | `RETURN type(r) AS rel, labels(n)[0] AS label, count(*) AS c` graph.py:687 |
| RETURN DISTINCT | 0 | a gramática aceita; 4 das 74 queries de LLM recusadas |
| ORDER BY | 3 | `ORDER BY ev.timestamp DESC` graph.py:456 |
| SKIP | 0 | a gramática aceita |
| LIMIT | ~14 | `LIMIT $limit`; literal interpolado; `LIMIT $_acl_limit` neo4j:675 |
| UNION | 1 | dois braços, cada um com `LIMIT $limit` (graph.py:563-575) |
| CALL {} IN TRANSACTIONS | 1 | `MATCH (n) CALL (n) { DETACH DELETE n } IN TRANSACTIONS OF 10000 ROWS` neo4j:757, em auto-commit |
| CREATE | 1 template, 2 cláusulas | `CREATE (ev:Event {...})` e `CREATE (e)-[:HAD_EVENT]->(ev)` generator:441-442 |
| MERGE | 9 | `MERGE (n:{label} {entity_id: $entity_id})` generator:243; `MERGE (a)-[r:{rel_type}]->(b)` generator:363; `MERGE (e)-[r:MENTIONED_IN {source_id: $source_id}]->(s)` graph.py:373 |
| ON CREATE / ON MATCH SET | 0 | |
| SET de propriedade | 9 | `SET n.p = $p, ..., n.acl_allow = $acl_allow, ..., n.updated_at = $updated_at` generator:243 |
| SET +=, SET de label, REMOVE, DELETE, FOREACH | 0 | propriedade se remove com `SET x = NULL`, de propósito |
| DETACH DELETE | 5 | `MATCH (n{label_frag}){where_frag} DETACH DELETE n` neo4j:794; wipe `MATCH (n) DETACH DELETE n` |
| CALL procedure | 5, sem YIELD | `CALL db.labels()`, `CALL db.relationshipTypes()`, `CALL db.indexes()` (falkordb) |
| YIELD | 1, em SHOW | `SHOW INDEXES YIELD labelsOrTypes, properties WHERE $lbl IN labelsOrTypes RETURN properties` neo4j:815-816 |

## 2. Funções e operadores

| Item | Produção | Exemplo ou onde aparece |
|---|---|---|
| any() | renderer de ACL + 1 | `any(p IN coalesce({var}.acl_allow, []) WHERE p IN $acl_principals)` visibility:255; `any(lbl IN labels(n) WHERE lbl IN $entity_types)` graph.py:794 |
| all() | renderer de ACL | `all(c IN {conn} WHERE c IN $acl_open_connectors)` visibility:283 |
| size(), coalesce() | renderer de ACL | `size(coalesce(n.acl_connector_ids, [])) = 0` |
| labels() | 4 | `labels(n)[0]`; `NOT 'Event' IN labels(n)` |
| type() | 2 | `type(r) AS rel` |
| id() | 3 | `RETURN id(n) AS nid`; `id(a) IN $node_ids`; `WHERE id(n) = item.id` (layout) |
| keys() | 2, uso interno | `RETURN keys(n) AS k LIMIT 1` neo4j:845 |
| count() | 2 | `count(*) AS c` |
| IN | 7 + renderer | `ev.event_type IN $event_types`; `n.entity_id IN $node_ids` |
| IS NULL / IS NOT NULL | 5 e 1 | `n.__schema_marker IS NULL` |
| teste de label no WHERE | 7 | `NOT neighbor:Event` |
| `<>` entre nós | 2 | `neighbor <> e` |
| índice de lista `[0]` | 2 | `labels(n)[0]` |
| elementId, properties, toLower e família, collect, sum/avg/min/max, CASE, STARTS/ENDS WITH, CONTAINS, `=~`, comprehensions, map projection, EXISTS, path functions, shortestPath, datas | 0 | aparecem no Cypher de LLM (seção 6) ou em testes |

`pii_token('email', 'x')` vira um literal hex antes de chegar ao motor (pii:609-661);
o motor nunca vê essa função.

## 3. Padrões de caminho

- Comprimento variável: 0 em produção (a gramática só aceita fixo). O LLM tentou
  `MATCH (cf)-[*1..2]->(node)` uma vez.
- Direção: `->` é a regra; não direcionado `-[r]-` em 5 templates; `<-` só no
  graph-bench e em 3 queries de LLM.
- Vários tipos `[:A|B]`: 1 site (graph.py:560), com tipos vindos do planner LLM.
- Relação sem tipo `-[]-`: graph.py:698.
- Propriedades inline no nó: ~17 templates, com label e **sem label**
  (`(e {entity_id: $entity_id})` em graph.py:371, 453, 564, 570, 578, 685, 698).
- Cadeia de dois saltos: `(e {...})-[{rel_pat}]-(mid)-[r2]-(neighbor)` graph.py:570.
- Padrões separados por vírgula: 2.

## 4. Schema, introspecção e procedures

- Índice: `CREATE INDEX {nome} IF NOT EXISTS FOR (n:{lbl}) ON (n.{p})`
  (neo4j:721-724), nome `client_<label>_<sha1>_<prop>`; erro com "already",
  "exist" ou "equivalent" é engolido como reexecução.
- Quem cria índice: o pipeline, a cada run, para `entity_id` de cada label,
  para `Event`, para os match_field e os indexed_fields (pipeline:1270-1285).
- Listar índices: `SHOW INDEXES YIELD labelsOrTypes, properties WHERE $lbl IN
  labelsOrTypes RETURN properties` (neo4j:814-818).
- `CALL db.labels()` e `CALL db.relationshipTypes()` a cada `get_schema`, sem
  cache, junto com `keys(n)` e SHOW INDEXES por label. O código chama o
  `get_schema` de "the most-called tool there is" (query.py:153-158).
- Amostras de assinatura: `MATCH (a)-[r:{t}]->(b) RETURN labels(a) AS la,
  labels(b) AS lb LIMIT 200` por tipo (common:157-158) e `MATCH (:{a})-[:{t}]->(:{b})
  RETURN 1 AS ok LIMIT 1` (common:182).
- Health: `RETURN 1`.
- Sem uso: CREATE CONSTRAINT, db.schema.*, db.propertyKeys, dbms.*, apoc.

## 5. Parâmetros reservados de ACL

- O predicado sai de `AclPredicate.to_cypher` (visibility:237-284) e os
  parâmetros de `bound_acl_params` (visibility:286-301): `acl_principals`
  sempre que há principal; `acl_connectors` com o eixo de entrada ligado e lista
  não vazia; `acl_open_connectors` com conjunto aberto não vazio. Todos são
  listas de strings em minúscula.
- O token do MCP sempre liga o eixo de entrada.
- `bind_acl_params` recusa parâmetro do chamador com prefixo `acl_` ou `_acl` e
  sobrescreve com os valores do servidor (common:433-455).
- O rewriter injeta o predicado em todo nó de cada MATCH e OPTIONAL MATCH de
  cada braço de UNION; nó anônimo ganha a variável `_acl_anon_N`.
- `get_graph` e `get_neighborhood_schema` injetam o predicado à mão pelo caminho
  cru (graph.py:530-539, 669-678) e o endpoint recheca no Postgres;
  `get_timeline` não leva predicado (a raiz é checada no Postgres).

Exemplo real do sombreamento, saída do rewriter para
`MATCH (p:Person)-[:TRABALHA_EM]->(:Company) RETURN p.name`:

```
MATCH (p:Person)-[:TRABALHA_EM]->(_acl_anon_0:Company) WHERE (any(p IN coalesce(p.acl_allow, []) WHERE p IN $acl_principals) AND NOT any(p IN coalesce(p.acl_deny, []) WHERE p IN $acl_principals)) AND (...) RETURN p.name
```

O pós-filtro (common:330-431) derruba a linha inteira se algum nó alcançável em
qualquer célula for invisível, julgando cada nó por `acl.is_visible` sobre as
propriedades `acl_*` devolvidas; depois remove todas as chaves `acl_*`. Relações
não são julgadas, e as relações de um Path não são visitadas.

**Consequências para o Brainwire:**

- Pelo `neo4j.py` sem mudança, todo nó devolvido a uma sessão de serviço
  precisa trazer as propriedades `acl_*`, senão o pós-filtro descarta toda linha
  de quem não é allow_all. O motor as materializa a partir do conjunto de ACL.
- Escalar e agregado só ficam protegidos pelo predicado no MATCH; o pós-filtro
  não julga escalar. A ACL nativa do Brainwire fecha esse buraco.

## 6. Cypher escrito por LLM

- Só a tool `run_cypher` do MCP recebe Cypher livre (mcp_server:629-648 até
  query.py:386-477 e `run_read_cypher`).
- A instrução ao modelo é a descrição da tool (catalog:426-441 em PT,
  1301-1316 em EN): chamar `get_schema` antes, preferir propriedade indexada, e
  o subconjunto aceito (MATCH/OPTIONAL MATCH/WITH/RETURN de comprimento fixo,
  WHERE, agregados, ORDER BY/SKIP/LIMIT, UNION; sem CONTAINS e família, sem
  UNWIND, CALL, subquery, caminho variável, properties()/keys(), mutação), com
  teto de 200 linhas.
- A descrição do `get_schema` diz que o grafo é "(FalkorDB)", qualquer que seja
  o backend.
- Medido no run g4: 175 chamadas de `run_cypher`, 104 (59%) sem resultado
  aproveitável; das 49 recusas, 12 por função e 37 por sintaxe (19 por
  CONTAINS/STARTS WITH/ENDS WITH, 3 por `=~`, 3 por LIKE, 1 por GROUP BY).

Construtos na amostra de 74 queries de LLM recusadas (fixture):

| Construto | Queries |
|---|---|
| propriedade inline no padrão | 30 |
| CONTAINS | 29 |
| busca por entity_id | 28 |
| properties() | 17 |
| count() | 16 |
| LIMIT | 16 |
| padrão não direcionado | 14 |
| toLower | 12 |
| ORDER BY | 11 |
| `;` final | 9 |
| `\n` literal | 8 |
| coalesce | 8 |
| type() | 7 |
| padrão dentro de WHERE (`NOT (c)--(:Ticket)`) | 5 |
| RETURN DISTINCT, WITH | 4 cada |
| OPTIONAL MATCH, collect(DISTINCT), `<-`, `=~`, LIKE, `x.*` | 3 cada |
| ENDS WITH | 2 |
| STARTS WITH, `size((c)--())`, EXISTS {}, caminho variável, fatia `[0..5]`, GROUP BY, toDate | 1 cada |

## 7. Escrita em lote

- O gerador produz um par (cypher, params) por linha; o pipeline descarrega a
  cada `_batch_size` linhas (10.000 no código, 250.000 no starter).
- Arestas e eventos (começam por MATCH) vão para `execute_pipeline`; MERGE de nó
  para `execute_batch` (graph_client:99-111, 199-214).
- A dobra em UNWIND (common:288-322) agrupa statements consecutivos de texto
  idêntico, troca `$nome` por `row.nome` fora de literais e envia
  `{"rows": [...]}`. Grupo de um statement só não é dobrado, e se a dobra falha
  cai para statement por statement: o motor precisa aceitar as duas formas.
- Um lote de 250.000 linhas passa de 200 MB num parâmetro só (estimativa a
  partir de ~1 KB por nó).

O que chega ao motor pelo `neo4j.py`:

```
UNWIND $rows AS row
MERGE (n:Customer {entity_id: row.entity_id})
SET n.name = row.name, n.source = row.source, n.acl_connector_ids = row.acl_connector_ids, n.acl_source_table = row.acl_source_table, n.acl_allow = row.acl_allow, n.acl_deny = row.acl_deny, n.connector_id = row.connector_id, n.display_name = row.display_name, n.display_tier = row.display_tier, n.display_color = row.display_color, n.updated_at = row.updated_at

UNWIND $rows AS row
MATCH (a:Person {entity_id: row.from_entity_id})
MATCH (b:Company {entity_id: row.to_entity_id})
MERGE (a)-[r:WORKS_AT]->(b)
SET r.role = row.role, r.last_activity_at = row.last_activity_at

UNWIND $rows AS row
MATCH (e:Customer {entity_id: row.entity_id})
CREATE (ev:Event {event_id: row.event_id, type: row.event_type, timestamp: row.ev_timestamp})
CREATE (e)-[:HAD_EVENT]->(ev)
```

Semântica que o lote pressupõe:

- um MERGE dentro do UNWIND enxerga as linhas anteriores do mesmo lote;
- uma aresta só existe se os dois MATCH acharem os nós, senão some em silêncio;
- `SET` com NULL remove a propriedade;
- o CREATE de evento não é idempotente.

Valores de linha: str, int, float, bool, None e listas de str. Datetime, Decimal
e UUID chegam como string ou float.

## 8. O que o `neo4j.py` exige do Bolt

- Driver com `notifications_disabled_classifications=["UNRECOGNIZED"]` e
  `verify_connectivity()`; esquemas bolt, bolt+s, bolt+ssc, neo4j, neo4j+s,
  neo4j+ssc.
- Toda query vai por `execute_query` (transação gerenciada, rota de escrita por
  padrão, leitura também), com `database_` que pode ser None.
- O wipe exige RUN em auto-commit (IN TRANSACTIONS não roda dentro de transação
  explícita).
- Valores: Node com labels, element_id e propriedades; Relationship com type,
  start_node, end_node e element_id; Path. `element_id` estável dentro de um
  resultado; `id()` devolve inteiro.
- Erros: falha de driver e erro transitório param o lote; "sintaxe não
  suportada" é reconhecida pelo texto ("invalidinput", "syntaxerror",
  "unsupportedadministrationcommand", "notsupported"); índice duplicado por
  "already", "exist" ou "equivalent". Códigos no padrão Neo4j mantêm esse
  comportamento.
- O skills-api monta driver novo e roda `verify_connectivity` a cada
  requisição de `get_schema` e `run_cypher`.

## 9. Inconsistências encontradas na plataforma

- O construtor de views do console oferece CONTAINS, STARTS WITH e ENDS WITH,
  que a gramática recusa; a pré-visualização devolve 400.
- A mensagem de erro do `/v1/memory/cypher` diz que UNWIND é aceito
  (graph.py:1009-1010); a gramática recusa.
- A descrição do `get_schema` diz que o grafo é FalkorDB, qualquer que seja o
  backend.
- O evento é gravado com `type` (generator:412), mas a timeline filtra e lê
  `event_type` (graph.py:448, 468).
- O regex de mutação do skills-api não tem o lookbehind nem a máscara de
  literais do core (query.py:94-97 contra common:49-68).

## 10. Os 20 construtos por prioridade

1. `UNWIND $rows AS row` com lista de maps grande e acesso `row.campo`.
2. `MERGE (n:L {entity_id: row.entity_id})` com busca por índice, enxergando as
   linhas anteriores do lote.
3. `SET n.p = expr, ...` com mais de 10 itens, listas e `= NULL` removendo.
4. `MATCH (a:L {entity_id: row.x})` com índice dentro do UNWIND, em dois MATCH
   seguidos.
5. `MERGE (a)-[r:T]->(b)` entre variáveis ligadas, com `SET r.p` e props inline.
6. `CREATE (ev:L {map})` e `CREATE (e)-[:T]->(ev)`.
7. O predicado de ACL (`any()`/`all()` sobre `coalesce(n.lista, [])`, `IN`,
   `size()`, NOT/AND/OR) com o escopo certo das variáveis.
8. Nó com map inline, com ou sem label: `(e {entity_id: $id})` pede busca
   global por `entity_id`.
9. Relação de comprimento fixo: `->`, `-[r]-`, `:A|B`, `-[]-`, cadeia de dois
   saltos.
10. `OPTIONAL MATCH ... WHERE`, com NULL na falta.
11. Predicados: `NOT n:L`, IS NULL, IS NOT NULL, `IN $lista`, comparações sobre
    string e número, `<>` entre nós.
12. RETURN de Node, Relationship e Path completos, com `acl_*` para sessão de
    serviço, aliases e constantes (`RETURN 1`).
13. `count(*)` com agrupamento implícito.
14. `ORDER BY alias [DESC]` e `LIMIT` com parâmetro ou literal.
15. `UNION` com LIMIT por braço.
16. `labels()`, `type()`, `id()`, `keys()` e índice `[0]`.
17. `CREATE INDEX nome IF NOT EXISTS FOR (n:L) ON (n.p)`.
18. `CALL db.labels()`, `CALL db.relationshipTypes()`, `SHOW INDEXES YIELD ...
    WHERE ... RETURN`.
19. `MATCH ... DETACH DELETE n` e `CALL (n) { DETACH DELETE n } IN TRANSACTIONS
    OF 10000 ROWS` em auto-commit.
20. O que os modelos usam e a gramática aceita: `WITH` com WHERE depois de
    agregar, `RETURN DISTINCT`, `collect(DISTINCT x)`, `size()`, `toLower`,
    `coalesce` com default string, `min`/`max`.

Fora da lista, com demanda medida: CONTAINS e família (19 das 37 recusas de
sintaxe no g4) e `properties()` (12 recusas, bloqueada de propósito pelo IR5).
O gate do graph-bench pede ainda `<-`, cadeias de 4 saltos e
`UNWIND ... CREATE ... RETURN count(b)`.
