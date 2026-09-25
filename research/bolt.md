# Bolt: o que o servidor do Brainwire precisa falar

Pesquisa de 24/09/2026 para o servidor Bolt do Brainwire (Rust). Alvo: o driver
oficial Python da Neo4j, pacote `neo4j` 5.x (hoje 5.28.6), com `bolt://`
(instância única) e `neo4j://` (roteamento em cluster).

**Como foi feita.** A especificação vem de https://neo4j.com/docs/bolt/current/,
lida nos arquivos AsciiDoc do repositório `neo4j-drivers/docs-bolt`, que é o que
o site publica (páginas soltas do site foram conferidas e batem). O comportamento
do driver vem do código das tags `5.28.6` e `6.3.1` de
`neo4j/neo4j-python-driver`, lido pela API do GitHub, sem clone e sem download.
Tudo que diz "o driver faz X" foi lido no código da 5.28.6, salvo menção
contrária. Onde a doc e o código divergem, o texto diz. Onde não achei algo, está
escrito "não achei". Recomendações para o Brainwire estão marcadas como
**recomendação** e são inferência minha, não fato documentado.

**Versões do driver.** O PyPI lista 6.3.1 como a versão mais recente (upload em
15/09/2026, exige Python 3.10+). A linha 5.x continua mantida: 5.28.6 saiu no
mesmo dia. A plataforma trava `neo4j>=5.0,<6` em `client-core/pyproject.toml`
(medido na 5.28.6), então o alvo real é o 5.28.x. O comentário da trava diz que
o 6.x removeu `Node.id` e `Relationship.id`. No código do 6.3.1, `Entity.id`
ainda existe, deprecado, e a docstring avisa que ele pode vir vazio conforme a
versão do servidor. A causa exata da quebra não foi confirmada aqui. O 6.x
aparece neste texto só onde muda algo para o servidor.

---

## Resumo

### Mínimo para `bolt://` com RUN/PULL (driver 5.28.6)

1. TCP na 7687. Ler `60 60 B0 17` e mais 16 bytes (quatro propostas). O driver
   manda `00 00 01 FF | 00 08 08 05 | 00 02 04 04 | 00 00 00 03`: manifest v1,
   faixa 5.8 a 5.0, faixa 4.4 a 4.2, e 3.0. Um servidor sem manifest ignora a
   primeira proposta e responde 4 bytes com uma versão exata, por exemplo
   `00 00 04 05` para 5.4. Nunca oferecer 5.5.
2. Chunking nos dois sentidos (cabeçalho u16 big-endian, fim de mensagem
   `00 00`) e PackStream v1 completo para ler (todos os marcadores) e escrever.
3. HELLO devolve `SUCCESS {"server": "Neo4j/<versão>", "connection_id": ...}`.
   **O driver 5.x recusa qualquer `server` que não comece com `Neo4j/`**
   (`UnsupportedServerProduct`). Em 5.1+ vem LOGON logo atrás, em pipeline:
   aceitar `{"scheme": "basic", ...}` e também `{}` (é o que o driver manda com
   `auth=None`).
4. RUN devolve `SUCCESS {"fields": [...]}` (`fields` é obrigatório na prática: o
   driver monta os registros com ele). O driver manda PULL `{"n": 1000}` junto;
   o servidor responde RECORDs e `SUCCESS {"has_more": true}` ou o sumário final
   (`bookmark`, `type` em `r`/`w`/`rw`/`s`, `t_last`, `db`, `stats`). DISCARD
   `{"n": -1}` também é usado.
5. BEGIN, RUN em transação (com `qid`), COMMIT (`{"bookmark": ...}`) e ROLLBACK.
   Não são opcionais: `driver.execute_query`, que o adaptador da plataforma usa,
   abre transação gerenciada com BEGIN+RUN+PULL em pipeline e depois COMMIT.
6. FAILURE seguido de IGNORED para tudo o que estiver na fila até chegar RESET.
   RESET sempre responde `SUCCESS {}`, aborta a transação aberta e volta a
   READY. O driver manda RESET depois de toda FAILURE e ao devolver conexão
   "suja" ao pool. GOODBYE fecha o socket sem resposta.
7. Node, Relationship, UnboundRelationship e Path no formato do Bolt 5, com
   `element_id` (o adaptador da plataforma usa `element_id` como `.id`).
8. Formato de erro certo para a versão: até 5.6, `{"code", "message"}`; de 5.7
   em diante, `neo4j_code`, `gql_status`, `description` e `message` juntos, ou o
   driver perde o código e a mensagem.

### Mínimo extra para `neo4j://`

1. Aceitar `routing` no HELLO (o driver manda `{"address": "<host:porta da URI>",
   ...parâmetros da query string}`).
2. ROUTE (tag `66`, Bolt 4.3+; formato 4.4+ com `extra {db, imp_user}`),
   respondendo `SUCCESS {"rt": {"ttl": <segundos>, "db": <database resolvido>,
   "servers": [{"addresses": [...], "role": "ROUTE"}, {..."READ"}, {..."WRITE"}]}}`.
   O driver descarta a tabela se não houver pelo menos um ROUTE e um READ.
   WRITE vazio é tolerado (eleição de líder).
3. Quando ROUTE vem sem `db`, resolver o home database do usuário (ou o padrão
   do servidor) e devolvê-lo em `rt.db`: o driver fixa esse nome e passa a
   mandar `db` em BEGIN/RUN.
4. Endereços anunciados que o cliente alcança, e `mode: "r"` respeitado.
5. Em cluster: seguidor recusa escrita com `Neo.ClientError.Cluster.NotALeader`
   (o driver tira o endereço dos escritores e tenta de novo); leitura com
   bookmark espera o estado chegar no bookmark ou falha com
   `Neo.TransientError.Transaction.BookmarkTimeout`.

---

## 1. Handshake e negociação de versão

### 1.1 Transporte

- TCP, porta padrão 7687. A doc cita WebSocket como transporte possível; o
  driver Python usa socket TCP.
- Tudo que depende de ordem de bytes é big-endian.
- Não existe encerramento formal: qualquer lado pode fechar o TCP a qualquer
  momento.
- TLS fica fora do Bolt: com `bolt+s`/`neo4j+s` (certificado de CA) ou
  `bolt+ssc`/`neo4j+ssc` (aceita autoassinado), o driver abre TLS e faz o
  handshake Bolt por dentro.

### 1.2 Identificação e propostas

1. O cliente manda `60 60 B0 17` logo após conectar. Não há resposta.
2. Em seguida manda exatamente quatro propostas de versão, cada uma um u32
   big-endian (16 bytes no total), em ordem de preferência. `00 00 00 00` é
   "nenhuma versão" e serve de enchimento.
3. O servidor responde 4 bytes com a versão escolhida. Se nenhuma servir,
   responde `00 00 00 00` e fecha a conexão.
4. Se mais de uma proposta servir, vale a primeira na ordem do cliente.

Formatos da proposta:

| Formato | Layout dos 4 bytes | Exemplo |
|---|---|---|
| Bolt 1 a 3 | o inteiro é a versão | `00 00 00 03` = 3 |
| Bolt 4.0+ | 2 bytes reservados, minor, major | `00 00 01 04` = 4.1 |
| Bolt 4.3+ (faixa) | 1 byte reservado, quantas minors abaixo também valem, minor, major | `00 02 03 04` = 4.3, 4.2 e 4.1 |

- A faixa não atravessa major.
- A resposta do servidor é sempre uma versão exata no formato 4.0, sem faixa.
- A doc recomenda que o cliente liste explicitamente as versões anteriores à
  4.3, porque servidor antigo não entende faixa.

### 1.3 Handshake manifest v1 (novo no Bolt 5.7)

- O cliente troca uma das quatro propostas por `00 00 01 FF`: dois bytes
  reservados, o byte da versão do manifest (1) e `FF`, que marca "manifest".
- O servidor que aceita responde, nesta ordem:
  1. `00 00 01 FF`;
  2. um VarInt `N`;
  3. `N` versões de 4 bytes no formato 4.3 (com faixa);
  4. um VarInt `CAPABILITIES`, bitmask reservada para emendas de fornecedor ao
     protocolo.
- O cliente responde a versão escolhida (4 bytes, formato 4.0, sem faixa) e um
  VarInt com o subconjunto de capabilities que aceitou. O servidor não responde
  a isso, então o cliente pode mandar a primeira mensagem Bolt no mesmo envio.
- VarInt: grupos de 7 bits, o grupo menos significativo primeiro, bit alto de
  cada byte como continuação; até u64. `01` = 1, `7F` = 127,
  `FF 82 71` = 1851775.
- Exemplo da doc: cliente `00 00 01 FF 00 00 04 04 00 00 00 03 00 00 00 02`;
  servidor `00 00 01 FF`, `02`, `00 02 08 05` (5.8 a 5.6), `00 04 04 04`
  (4.4 a 4.0), `09` (bits 0 e 3); cliente `00 00 07 05` (5.7) e `08`.
- Com manifest, o SUCCESS do HELLO ganha `protocol_version` (ex.: `"5.7"`). O
  driver 5.28.6 não lê esse campo.

### 1.4 O que o driver Python faz no handshake

Driver 5.28.6 (`_async/io/_bolt.py` e `_async/io/_bolt_socket.py`):

- Manda sempre `60 60 B0 17` e depois
  `00 00 01 FF  00 08 08 05  00 02 04 04  00 00 00 03`: manifest v1, 5.8 a 5.0,
  4.4 a 4.2, 3.0.
- Versões com implementação registrada: 3.0, 4.2, 4.3, 4.4 e 5.0 a 5.8. As
  classes 4.0 e 4.1 existem mas não se registram (`SKIP_REGISTRATION`).
- Resposta clássica: o driver lê só os dois últimos bytes (minor, major) e
  ignora os dois primeiros.
- Resposta com manifest: lê `N`, as faixas e as capabilities; escolhe a **maior**
  versão que conhece dentro de qualquer faixa oferecida; manda essa versão e
  capabilities `00`.
- Se a resposta for `HTTP`, acusa porta errada (`ServiceUnavailable`). Se for
  manifest de versão diferente de 1, `BoltProtocolError`. Se a versão acordada
  não tiver implementação (inclusive `00 00 00 00`), fecha e levanta
  `BoltHandshakeError`.

Driver 6.3.1: manda os mesmos 16 bytes. Sabe falar 6.0 e 6.1, mas essas versões
só aparecem por manifest, porque não estão nas propostas clássicas. Sem versão
em comum, levanta `UnsupportedServerProduct`, que no 6.x virou subclasse de
`ConfigurationError`.

### 1.5 Neo4j x Bolt (tabela da doc)

| Neo4j | Bolt 5.x negociado |
|---|---|
| 5.0 a 5.4 | 5.0 |
| 5.5 a 5.6 | 5.0 e 5.1 |
| 5.7 a 5.8 | 5.0 a 5.2 |
| 5.9 a 5.12 | 5.0 a 5.3 |
| 5.13 a 5.22 | 5.0 a 5.4 |
| 5.23 a 5.25 | 5.0 a 5.4 e 5.6 |
| 5.26 em diante | 5.0 a 5.4 e 5.6 a 5.8 |
| 2025.10 em diante | também Bolt 6.0 |

- **Bolt 5.5 existiu em alguns drivers, tem desenho falho e nenhum servidor
  Neo4j o negocia.** O driver Python tem a classe 5.5, então um servidor que
  ofereça 5.5 pode acabar nela.
- Neo4j 4.4 em diante fala 4.0 a 4.4 (as anteriores marcadas como removíveis).

### 1.6 O que mudou em cada versão 5.x

| Versão | Mudança |
|---|---|
| 5.0 | `element_id` em Node, Relationship e UnboundRelationship; DateTime (`49`) e DateTimeZoneId (`69`) novos, com segundos em UTC, substituem os legados (`46`/`66`). Mensagens iguais às da 4.4. |
| 5.1 | HELLO deixa de carregar autenticação; entram LOGON e LOGOFF e o estado AUTHENTICATION. O estado CONNECTED vira NEGOTIATION. |
| 5.2 | HELLO, BEGIN e RUN aceitam `notifications_minimum_severity` e `notifications_disabled_categories`. |
| 5.3 | `bolt_agent` no HELLO. |
| 5.4 | Mensagem TELEMETRY e hint `telemetry.enabled`. |
| 5.5 | Não usada. |
| 5.6 | `statuses` substitui `notifications` no SUCCESS de PULL/DISCARD; `notifications_disabled_categories` vira `notifications_disabled_classifications`. |
| 5.7 | Handshake manifest v1; `protocol_version` no SUCCESS do HELLO; FAILURE ganha `gql_status`, `description`, `diagnostic_record` e `cause`, e `code` passa a se chamar `neo4j_code`. |
| 5.8 | Hint `ssr.enabled`; `advertised_address` no SUCCESS do LOGON; SUCCESS de BEGIN e de RUN autocommit traz em `db` o home database resolvido. |
| 6.0 | Estruturas Vector (`56`) e UnsupportedType (`3F`); o texto de `message` na FAILURE deixa de ter formato estável. |

Achado fora da doc: no driver 6.3.1, a classe Bolt 6.1 troca o codec para
PackStream v2, que acrescenta um tipo UUID com marcador `E0`. Não achei 6.1 nem
PackStream v2 na doc oficial.

### 1.7 Versão mínima para o driver 5.x conectar

- Tecnicamente, 3.0 conecta: o 5.28.6 ainda oferece 3.0 e 4.2 a 4.4.
- Com Bolt 3 não há `db` (o driver recusa `database=` com `ConfigurationError`)
  e o roteamento usa a procedure `dbms.cluster.routing.getRoutingTable`.
- De 4.0 a 4.2 o roteamento é pela procedure
  `dbms.routing.getRoutingTable($context, $database)`; ROUTE só existe a partir
  da 4.3, e `rt.db` (resolução do home database) a partir da 4.4.
- `element_id` nativo exige 5.0. Com Bolt 4.x o driver 5.x fabrica
  `element_id = str(id)`.
- **Recomendação:** implementar primeiro a faixa 5.1 a 5.4 com handshake
  clássico. É onde entram LOGON (previsto no plano) e TELEMETRY, e os erros
  continuam no formato simples. Aceitar também 5.0 se sair barato (autenticação
  dentro do HELLO). 5.6 a 5.8 e o manifest ficam para quando o formato GQL de
  erro existir. Como o driver 6.x manda as mesmas propostas clássicas, um
  servidor 5.x sem manifest atende também o 6.x.

---

## 2. PackStream (versão 1)

### 2.1 Tipos e marcadores

| Tipo | Marcadores | Observação |
|---|---|---|
| Null | `C0` | |
| Boolean | `C2` false, `C3` true | |
| Integer | `00`..`7F` (0 a 127), `F0`..`FF` (-16 a -1), `C8`+i8, `C9`+i16, `CA`+i32, `CB`+i64 | sempre com sinal, 64 bits no máximo; a doc recomenda a forma mais curta |
| Float | `C1` + 8 bytes IEEE 754 double | não há float de 32 bits nem inteiro sem sinal |
| Bytes | `CC`+u8, `CD`+u16, `CE`+u32 | sem forma curta; máximo 2^31-1 |
| String | `80`..`8F` (0 a 15 bytes), `D0`+u8, `D1`+u16, `D2`+u32 | tamanho em bytes do UTF-8, não em caracteres |
| List | `90`..`9F` (0 a 15 itens), `D4`+u8, `D5`+u16, `D6`+u32 | heterogênea; tamanho em itens |
| Dictionary | `A0`..`AF` (0 a 15 pares), `D8`+u8, `D9`+u16, `DA`+u32 | chave sempre String; chave repetida: vale a última |
| Structure | `B0`..`BF` (0 a 15 campos) + tag byte (0 a 127) | a semântica vem da versão do Bolt |

- Os tamanhos são u8, u16 ou u32 sem sinal, mas o teto vale 2^31-1 em bytes,
  itens ou pares.
- A doc diz que marcadores reservados não devem ser usados e que recebê-los é
  erro. Pela tabela sobram `C4`..`C7`, `CF`, `D3`, `D7`, `DB`..`DF` e
  `E0`..`EF` (lista derivada por mim; a doc não enumera). O `E0` é o UUID do
  PackStream v2 (Bolt 6.1, só no driver 6.x).
- Exemplos da doc: 42 = `2A`; 1.23 = `C1 3F F3 AE 14 7A E1 47 AE`;
  `"A"` = `81 41`; `{}` = `A0`; `[1, 2, 3]` = `93 01 02 03`.
- Tag de mensagem e tag de valor são espaços separados: a estrutura de topo do
  chunk é mensagem, as aninhadas são valores. TELEMETRY (`54`) e Time (`54`),
  ROUTE (`66`) e Legacy DateTimeZoneId (`66`), PULL (`3F`) e UnsupportedType
  (`3F`) têm o mesmo byte.

### 2.2 Chunking (camada do Bolt, logo abaixo das mensagens)

- Cada mensagem vai em um ou mais chunks: cabeçalho u16 big-endian com o
  tamanho (até 65535), depois os dados.
- A mensagem termina com `00 00`. Um chunk nunca carrega duas mensagens.
- Desde a 4.1, um chunk vazio (NOOP) serve de keep-alive; desde a 4.3 pode ir
  em qualquer estado enquanto o servidor processa.
- O driver 5.28.6 corta em chunks de 16 KiB e pula NOOPs.
- Exemplos calculados: `SUCCESS {}` = `00 03 B1 70 A0 00 00`;
  `RECORD [1]` = `00 04 B1 71 91 01 00 00`;
  `SUCCESS {"server": "Neo4j/5.26.0"}` =
  `00 17 B1 70 A1 86 73 65 72 76 65 72 8C 4E 65 6F 34 6A 2F 35 2E 32 36 2E 30 00 00`.

### 2.3 Estruturas de valor (Bolt 5.x)

| Estrutura | Tag | Campos | Observação |
|---|---|---|---|
| Node | `4E` (`B4 4E`) | `id` Integer, `labels` List<String>, `properties` Dict, `element_id` String | 3 campos antes da 5.0 |
| Relationship | `52` (`B8 52`) | `id`, `startNodeId`, `endNodeId` (Integer), `type` String, `properties` Dict, `element_id`, `start_node_element_id`, `end_node_element_id` (String) | 5 campos antes da 5.0 |
| UnboundRelationship | `72` (`B4 72`) | `id`, `type`, `properties`, `element_id` | sem pontas; usada dentro de Path; 3 campos antes da 5.0 |
| Path | `50` (`B3 50`) | `nodes` List<Node>, `rels` List<UnboundRelationship>, `indices` List<Integer> | regra dos índices abaixo |
| Date | `44` (`B1 44`) | `days` | dias desde 1970-01-01 |
| Time | `54` (`B2 54`) | `nanoseconds`, `tz_offset_seconds` | nanossegundos desde a meia-noite local |
| LocalTime | `74` (`B1 74`) | `nanoseconds` | |
| DateTime | `49` (`B3 49`) | `seconds`, `nanoseconds`, `tz_offset_seconds` | `seconds` = epoch em UTC |
| DateTimeZoneId | `69` (`B3 69`) | `seconds`, `nanoseconds`, `tz_id` | `seconds` = epoch em UTC; `tz_id` do tz database |
| LocalDateTime | `64` (`B2 64`) | `seconds`, `nanoseconds` | relógio local contado desde a epoch |
| Duration | `45` (`B4 45`) | `months`, `days`, `seconds`, `nanoseconds` | pode ser negativa |
| Point2D | `58` (`B3 58`) | `srid`, `x`, `y` (Float) | |
| Point3D | `59` (`B4 59`) | `srid`, `x`, `y`, `z` | |
| Legacy DateTime | `46` | `seconds` (UTC mais o offset), `nanoseconds`, `tz_offset_seconds` | só até 4.4; proibida em 5.x |
| Legacy DateTimeZoneId | `66` | `seconds` (hora local), `nanoseconds`, `tz_id` | só até 4.4 |
| Vector | `56` (`B2 56`) | `type_marker` Bytes, `data` Bytes | 6.0 |
| UnsupportedType | `3F` (`B4 3F`) | `name`, `minimum_protocol_major`, `minimum_protocol_minor`, `extra {message}` | 6.0 |

- `nanoseconds` vai de 0 a 999.999.999.
- Exemplo da diferença do DateTime: `1970-01-01T02:15:00.000000042+01:00` é
  `seconds=4500, nanoseconds=42, tz_offset_seconds=3600` no Bolt 5. Na
  estrutura legada seria `seconds=8100`, com o offset somado.
- Path: o primeiro nó de `nodes` abre o caminho e não aparece em `indices`.
  `indices` tem tamanho par. As posições 1ª, 3ª, 5ª... apontam para `rels`
  começando em 1; valor negativo quer dizer relação percorrida ao contrário;
  zero nunca aparece. As posições 2ª, 4ª... apontam para `nodes` começando em 0.

### 2.4 `element_id` no Bolt 5

- O que mudou: Node, Relationship e UnboundRelationship ganharam `element_id`
  String, e Relationship ganhou `start_node_element_id` e
  `end_node_element_id`. O campo `id` Integer continua na estrutura.
- Semântica pelo Cypher Manual: `elementId()` devolve String única entre nós e
  relações de todos os databases do DBMS, mas só dentro de uma transação; fora
  dela não há garantia de mapeamento. `id()` está deprecado e é único só por
  tipo e database. A Neo4j reaproveita IDs de elementos apagados.
- O driver 5.28.6 guarda nós e relações do resultado pela chave `element_id`.
  Se ele vier ausente (Bolt 4.x), usa `str(id)`. O `id` legado também é guardado.
- O adaptador da plataforma (`client-core/.../repository/neo4j.py`) expõe
  `element_id` como `.id` de `_Node` e `_Relationship`, e o memory-api deduplica
  por esse valor. Então `element_id` precisa ser estável pelo menos dentro de um
  resultado.
- **Recomendação:** mandar também um `id` inteiro de verdade, porque a spec
  define o campo como Integer.

### 2.5 Parâmetros que o driver 5.28.6 manda

- `date` vira Date. `time` sem fuso vira LocalTime; com fuso, Time.
- `datetime` sem fuso vira LocalDateTime (`64`); com fuso nomeado (pytz ou
  zoneinfo), DateTimeZoneId (`69`); com offset fixo, DateTime (`49`).
- `timedelta` e `Duration` viram Duration.
- Pontos viram Point2D/Point3D com SRID 7203/9157 (cartesiano) ou 4326/4979
  (WGS-84).
- `bytes` viram Bytes; `list`/`tuple`, List; `dict`, Dictionary.

---

## 3. Máquina de estados e mensagens

### 3.1 Regras gerais

- Cada requisição recebe zero ou mais mensagens de detalhe (só RECORD) e
  exatamente uma de sumário (SUCCESS, IGNORED ou FAILURE).
- O cliente pode mandar várias requisições em pipeline. Depois de uma FAILURE,
  o servidor responde IGNORED a tudo até o cliente reconhecer a falha com RESET.
- Mensagem inesperada para o estado é erro de protocolo, fatal: o servidor vai
  para DEFUNCT e fecha a conexão.
- Sinais: `<INTERRUPT>` fura a fila (o RESET dispara), e `<DISCONNECT>` vem do
  GOODBYE.
- **Recomendação:** sempre mandar SUCCESS com dicionário, mesmo vazio
  (`B1 70 A0`). A doc mostra `SUCCESS` sem metadata no ROLLBACK, mas a mensagem
  é definida com um campo Dictionary. O driver aceita as duas formas.

### 3.2 Estados (5.1+)

| Estado | Significado |
|---|---|
| NEGOTIATION | handshake concluído; só aceita HELLO (chamava CONNECTED até 5.0) |
| AUTHENTICATION | depois de HELLO ou LOGOFF; só aceita LOGON |
| READY | aceita RUN, BEGIN, ROUTE, TELEMETRY, LOGOFF, RESET, GOODBYE |
| STREAMING | autocommit com resultado aberto |
| TX_READY | transação explícita aberta, sem resultado pendente |
| TX_STREAMING | transação explícita com um ou mais resultados abertos |
| FAILED | falha pendente de RESET |
| INTERRUPTED | entre a chegada do `<INTERRUPT>` e o processamento do RESET |
| DEFUNCT | conexão morta (terminal) |

Até 5.0, o HELLO leva a autenticação e vai direto de CONNECTED para READY.

### 3.3 Transições (apêndice "version 5.1+")

| Estado | Mensagem | Resposta | Novo estado |
|---|---|---|---|
| NEGOTIATION | HELLO | SUCCESS / FAILURE | AUTHENTICATION / DEFUNCT |
| AUTHENTICATION | LOGON | SUCCESS / FAILURE | READY / DEFUNCT |
| READY | RUN | SUCCESS / FAILURE | STREAMING / FAILED |
| READY | BEGIN | SUCCESS / FAILURE | TX_READY / FAILED |
| READY | ROUTE | SUCCESS / FAILURE | READY / FAILED |
| READY | TELEMETRY (5.4+) | SUCCESS / FAILURE | READY / FAILED |
| READY | LOGOFF | SUCCESS / FAILURE | AUTHENTICATION / FAILED (ver nota) |
| STREAMING | PULL ou DISCARD | SUCCESS `has_more: true` | STREAMING |
| STREAMING | PULL ou DISCARD | SUCCESS `has_more: false` ou `{}` | READY |
| STREAMING | PULL ou DISCARD | FAILURE | FAILED |
| TX_READY | RUN | SUCCESS `{qid}` / FAILURE | TX_STREAMING / FAILED |
| TX_READY | COMMIT ou ROLLBACK | SUCCESS / FAILURE | READY / FAILED |
| TX_STREAMING | RUN | SUCCESS `{qid}` / FAILURE | TX_STREAMING / FAILED |
| TX_STREAMING | PULL ou DISCARD | `has_more: true` | TX_STREAMING |
| TX_STREAMING | PULL ou DISCARD | `has_more: false` | TX_READY, ou TX_STREAMING se ainda houver resultado aberto |
| FAILED | RUN, PULL, DISCARD, BEGIN, COMMIT, ROLLBACK, ROUTE, LOGOFF | IGNORED | FAILED |
| INTERRUPTED | qualquer uma menos RESET | IGNORED | INTERRUPTED |
| READY, STREAMING, TX_READY, TX_STREAMING, FAILED ou INTERRUPTED | RESET | (`<INTERRUPT>`) | INTERRUPTED; ao processar o RESET, SUCCESS e READY (ou FAILURE e DEFUNCT) |
| qualquer | GOODBYE | nenhuma | DEFUNCT |

- COMMIT não é aceito em TX_STREAMING. O driver descarta os resultados
  pendentes (DISCARD) antes de COMMIT e ROLLBACK.
- Nota: sobre FAILURE no LOGOFF a doc diverge. A tabela do apêndice leva a
  FAILED; o texto da mensagem manda FAILURE e fechar a conexão, e o cliente
  deve tratá-la como DEFUNCT.

### 3.4 Tags

| Mensagem | Sentido | Tag | Cabeçalho | Campos | Desde |
|---|---|---|---|---|---|
| HELLO | C para S | `01` | `B1 01` | `extra` Dict | 3 |
| LOGON | C para S | `6A` | `B1 6A` | `auth` Dict | 5.1 |
| LOGOFF | C para S | `6B` | `B0 6B` | nenhum | 5.1 |
| TELEMETRY | C para S | `54` | `B1 54` | `api` Integer | 5.4 |
| GOODBYE | C para S | `02` | `B0 02` | nenhum | 3 |
| RESET | C para S | `0F` | `B0 0F` | nenhum | 1 |
| RUN | C para S | `10` | `B3 10` | `query` String, `parameters` Dict, `extra` Dict | 1 (`extra` desde 3) |
| DISCARD | C para S | `2F` | `B1 2F` | `extra` Dict (`n`, `qid`) | campos desde 4.0 |
| PULL | C para S | `3F` | `B1 3F` | `extra` Dict (`n`, `qid`) | campos desde 4.0 |
| BEGIN | C para S | `11` | `B1 11` | `extra` Dict | 3 |
| COMMIT | C para S | `12` | `B0 12` | nenhum | 3 |
| ROLLBACK | C para S | `13` | `B0 13` | nenhum | 3 |
| ROUTE | C para S | `66` | `B3 66` | `routing` Dict, `bookmarks` List<String>, `extra` Dict | 4.3 (`extra` desde 4.4) |
| SUCCESS | S para C | `70` | `B1 70` | `metadata` Dict | |
| RECORD | S para C | `71` | `B1 71` | `data` List | |
| IGNORED | S para C | `7E` | `B0 7E` | nenhum | |
| FAILURE | S para C | `7F` | `B1 7F` | `metadata` Dict | |

INIT, ACK_FAILURE, PULL_ALL e DISCARD_ALL são de versões antigas e não aparecem
em 5.x.

### 3.5 Mensagem por mensagem

Legenda: "spec" é o que a doc define; "driver" é o que o 5.28.6 manda ou lê.

#### HELLO (`01`)

- **Estado:** só em NEGOTIATION. Em outro estado é violação e fecha a conexão.
- **Campos de `extra` (spec):**
  - `user_agent` String, no formato "Nome/Versão".
  - Até 5.0, também `scheme`, `principal`, `credentials` e demais campos de
    autenticação.
  - `routing` Dict (4.1+). Ausente ou `null`: sem roteamento. `{}`: rotear.
    Com conteúdo: rotear com esse contexto (as chaves da query string da URI
    mais `address`).
  - `patch_bolt` List (só 4.3 e 4.4, patch `"utc"`).
  - `notifications_minimum_severity` String (5.2+; `"OFF"` desliga tudo).
  - `notifications_disabled_categories` List (5.2 a 5.4).
  - `notifications_disabled_classifications` List (5.6+).
  - `bolt_agent` Dict (5.3+, obrigatório pela spec): `product` obrigatório;
    `platform`, `language` e `language_details` opcionais.
- **Driver:** sempre manda `user_agent`, no formato
  `neo4j-python/5.28.6 Python/<versão> (<plataforma>)`. Manda `routing` só com
  `neo4j://`, com `address` e os parâmetros da URI. `bolt_agent` vai a partir de
  5.3, com `product` `neo4j-python/5.28.6`. Os campos de notificação só vão se o
  usuário configurar. Em 5.1+ manda LOGON logo atrás, no mesmo envio, e só então
  lê as duas respostas.
- **SUCCESS (spec):**
  - `server` String, ex.: `"Neo4j/5.26.0"`.
  - `connection_id` String, ex.: `"bolt-61"`.
  - `hints` Dict (4.3+).
  - `protocol_version` String (5.7+, só com manifest).
  - `patch_bolt` (4.3 e 4.4).
- **Hints definidos:**
  - `connection.recv_timeout_seconds` Integer (4.3+): o driver passa a usá-lo
    como timeout de leitura do socket. Quem anunciar precisa mandar NOOP em
    query longa, ou o driver derruba a conexão.
  - `telemetry.enabled` Boolean (5.4+): sem ele, o driver não manda TELEMETRY.
  - `ssr.enabled` Boolean (5.8+).
- **Driver lê:** `hints`; guarda o resto em `server_info`; exige `server`
  começando com `Neo4j/` (seção 6.5). `connection_id` é opcional (vira
  `"<unknown id>"` nos logs).
- **FAILURE:** o servidor pode mandar metadata e deve fechar a conexão em
  seguida. O driver mata a conexão.

#### LOGON (`6A`, 5.1+)

- **Estado:** AUTHENTICATION.
- **Campos (spec):** `auth` Dict com `scheme` (`none`, `basic`, `bearer`,
  `kerberos`, conforme o servidor) e os campos do esquema: `basic` usa
  `principal` e `credentials`; `bearer`, só `credentials`.
- **Driver:** manda o dicionário do objeto `Auth`: `scheme`, `principal`,
  `credentials` e, se existirem, `realm` e `parameters`. Com `auth=None` manda
  `{}`, sem `scheme`.
- **SUCCESS (spec):** `{}`; `advertised_address` (5.8+); `credentials_expired`
  Boolean (ausente conta como false). O driver 5.28.6 não lê nenhum dos dois.
- **FAILURE:** o servidor fecha a conexão. O driver mata a conexão e levanta o
  erro, por exemplo `AuthError` para `Neo.ClientError.Security.Unauthorized`.

#### LOGOFF (`6B`, 5.1+)

- **Estado:** só em READY. Sem campos.
- **Resposta:** SUCCESS leva a AUTHENTICATION. Fora de READY, FAILURE e fecha
  (ver nota em 3.3).
- **Driver:** usa quando precisa trocar de credencial na mesma conexão (auth
  por sessão, rotação de token). Manda LOGOFF e LOGON em pipeline. Com
  `bolt://` e credencial fixa, não aparece.

#### TELEMETRY (`54`, 5.4+)

- **Estado:** só em READY.
- **Campo:** `api` Integer: 0 transação gerenciada, 1 explícita, 2 implícita,
  3 `execute_query()`. A informação vai para as métricas do servidor.
- **Resposta:** SUCCESS e o estado não muda. Valor inválido ou estado errado dá
  FAILURE e FAILED.
- **Driver:** só manda se o hint `telemetry.enabled` for true e o usuário não
  tiver desligado. Vai em pipeline antes de BEGIN ou RUN. A spec diz que o
  servidor deve aceitar TELEMETRY mesmo se o cliente ignorar o hint.

#### RUN (`10`)

- **Estado:** READY (autocommit) ou TX_READY/TX_STREAMING (transação). Em FAILED
  ou INTERRUPTED, IGNORED.
- **Campos:** `query` String e `parameters` Dict.
- **`extra` no autocommit** (em transação explícita vem vazio; quem carrega é o
  BEGIN):

  | Campo | Tipo | Padrão (spec) | Desde |
  |---|---|---|---|
  | `bookmarks` | List<String> | `[]` | 3 |
  | `tx_timeout` | Integer (ms) | timeout configurado no servidor | 3 |
  | `tx_metadata` | Dict | null | 3 |
  | `mode` | `"r"` ou `"w"` | `"w"` | 3 |
  | `db` | String | null; null e `""` significam o database padrão configurado no servidor | 4.0 |
  | `imp_user` | String | null (sem impersonação) | 4.4 |
  | `notifications_minimum_severity` | String | null (usa o do HELLO) | 5.2 |
  | `notifications_disabled_classifications` | List<String> | null | 5.6 |
  | `notifications_disabled_categories` | List<String> | null | 5.2 a 5.4 |

- **Driver:** manda `mode: "r"` só em leitura (ausente é escrita). Manda `db`
  só se o database estiver definido, `bookmarks` só se a lista não for vazia, e
  `tx_metadata`, `tx_timeout` (convertido para ms) e `imp_user` só se
  configurados. Manda PULL `{"n": <fetch_size>}` (padrão 1000) no mesmo envio.
- **SUCCESS (spec):**
  - `fields` List<String>.
  - `t_first` Integer (ms até o primeiro registro).
  - `qid` Integer (4.0+, só em transação explícita).
  - `db` String (5.8+, só quando o cliente não mandou `db`: é o home database
    resolvido do usuário).
- **Driver lê:** `fields`, que é obrigatório na prática (as chaves do
  `Record`); `qid`; `db`, que informa o database resolvido.

#### PULL (`3F`) e DISCARD (`2F`)

- **Campos:** `extra` com `n` Integer (obrigatório, sem padrão; `-1` = todos) e
  `qid` Integer (só em transação; `-1` = o último resultado aberto; padrão -1).
- **Detalhe:** PULL devolve zero ou mais `RECORD [valores]`; DISCARD não
  devolve nada.
- **SUCCESS com mais registros:** `{"has_more": true}`. Ausente conta como
  false.
- **SUCCESS no fim do resultado:**

  | Campo | Tipo | Nota |
  |---|---|---|
  | `bookmark` | String | só em autocommit |
  | `db` | String | database onde rodou (4.0+) |
  | `type` | String | `"r"`, `"w"`, `"rw"` ou `"s"` (schema) |
  | `t_last` | Integer | ms até consumir o último registro |
  | `t_first` | Integer | listado também aqui pela doc |
  | `stats` | Dict | contadores |
  | `plan`, `profile` | Dict | EXPLAIN/PROFILE |
  | `notifications` | List<Dict> | até 5.4 |
  | `statuses` | List<Dict> | 5.6+, objetos de status GQL |

- **Driver:** manda PULL `{"n": 1000}` e repete enquanto vier
  `has_more: true`. No `consume()`, manda DISCARD `{"n": -1}`. Só manda `qid`
  quando o resultado não é o último aberto na transação.
- **Driver lê:** se `type` vier com valor fora de `r`/`w`/`rw`/`s`, levanta
  `BoltProtocolError`. Lê `stats` pelas chaves `nodes-created`,
  `nodes-deleted`, `relationships-created`, `relationships-deleted`,
  `properties-set`, `labels-added`, `labels-removed`, `indexes-added`,
  `indexes-removed`, `constraints-added`, `constraints-removed`,
  `system-updates`, `contains-updates` e `contains-system-updates`. Sem
  `statuses` nem `notifications`, o driver sintetiza o status.
- **Transação com vários resultados:** a partir do Bolt 4.0 o driver não
  esgota o resultado anterior antes do próximo RUN. O servidor precisa então
  manter vários resultados abertos na mesma transação, cada um com seu `qid`, e
  atender PULL/DISCARD por `qid`. Sem `qid` no SUCCESS do RUN, o driver usa -1
  para todos e puxa do resultado errado.

#### BEGIN (`11`)

- **Estado:** READY.
- **Campos de `extra`:** os mesmos do RUN autocommit (`bookmarks`, `tx_timeout`,
  `tx_metadata`, `mode`, `db`, `imp_user` e os de notificação), com os mesmos
  padrões.
- **SUCCESS:** `{}`; a partir de 5.8, `db` quando o cliente não mandou `db`.
- **Driver:** mesma política de campos do RUN. Em `execute_query` manda BEGIN,
  RUN e PULL no mesmo envio (BEGIN em pipeline). Se o BEGIN falhar, o servidor
  precisa responder IGNORED ao RUN e ao PULL.

#### COMMIT (`12`) e ROLLBACK (`13`)

- **Estado:** TX_READY. Sem campos.
- **SUCCESS do COMMIT:** `bookmark` String.
- **SUCCESS do ROLLBACK:** `{}`.
- **Driver:** lê `bookmark` e `db` do SUCCESS do COMMIT. Não manda ROLLBACK se a
  conexão já foi resetada, porque o RESET já desfez a transação.

#### RESET (`0F`)

- **Semântica:** fura a fila, interrompe o trabalho em curso, faz o que estava
  na fila à frente dele receber IGNORED, e deixa a conexão como logo após
  HELLO+LOGON bem-sucedidos. Na prática, aborta a transação aberta.
- **Resposta:** `SUCCESS {}` e READY. FAILURE só se a conexão ainda não chegou a
  READY, e então o servidor fecha.
- **Driver:** manda RESET dentro do tratamento de toda FAILURE comum, antes de
  levantar a exceção, e ao devolver ao pool uma conexão que não esteja limpa.
  Se o RESET receber FAILURE, IGNORED ou RECORD, o driver descarta a conexão.

#### GOODBYE (`02`)

- Sem campos e sem resposta. O servidor fecha o socket e interrompe o que
  estiver rodando. O driver manda ao fechar conexões.

#### ROUTE (`66`, 4.3+)

- Detalhes na seção 4. Só depois da autenticação e fora de transação (READY).
  O estado não muda.

#### FAILURE (`7F`) e IGNORED (`7E`)

- Formato e códigos na seção 6. IGNORED não tem campos.

### 3.6 Sequências que o driver 5.28.6 gera

```
# conexão (bolt://, Bolt 5.4, servidor sem manifest)
C: 60 60 B0 17  00 00 01 FF  00 08 08 05  00 02 04 04  00 00 00 03
S: 00 00 04 05
C: HELLO {"user_agent": "neo4j-python/5.28.6 ...", "bolt_agent": {...}}
C: LOGON {"scheme": "basic", "principal": "neo4j", "credentials": "..."}
S: SUCCESS {"server": "Neo4j/5.26.0", "connection_id": "bolt-1"}
S: SUCCESS {}

# session.run (autocommit, sessão aberta com database="neo4j")
C: RUN "MATCH (n) RETURN n" {} {"db": "neo4j"}
C: PULL {"n": 1000}
S: SUCCESS {"fields": ["n"], "t_first": 1}
S: RECORD [...]  (até 1000)
S: SUCCESS {"has_more": true}
C: PULL {"n": 1000}
S: RECORD [...]
S: SUCCESS {"bookmark": "...", "type": "r", "t_last": 3, "db": "neo4j"}

# execute_query (transação gerenciada)
C: BEGIN {"db": "neo4j", "bookmarks": ["..."]}
C: RUN "CREATE (n:X) RETURN n" {} {}
C: PULL {"n": 1000}
S: SUCCESS {}
S: SUCCESS {"fields": ["n"], "qid": 0}
S: RECORD [...]
S: SUCCESS {"type": "w", "t_last": 2, "db": "neo4j"}
C: COMMIT
S: SUCCESS {"bookmark": "..."}

# falha (Bolt 5.4, formato de erro anterior à 5.7)
C: RUN "RETRUN 1" {} {}
C: PULL {"n": 1000}
S: FAILURE {"code": "Neo.ClientError.Statement.SyntaxError", "message": "..."}
S: IGNORED
C: RESET
S: SUCCESS {}

# encerramento
C: GOODBYE
```

Com Bolt 5.4 e o hint `telemetry.enabled`, entra `TELEMETRY <api>` antes de RUN
ou BEGIN.

---

## 4. Roteamento

### 4.1 `bolt://` e `neo4j://` no driver

- **`bolt://`, `bolt+s://`, `bolt+ssc://`:** driver direto, com pool de um
  endereço só. O HELLO vai sem `routing` e não existe ROUTE. Parâmetros na
  query string da URI geram aviso de depreciação e são ignorados. Sem
  `database=`, nenhum `db` é enviado e o servidor usa o padrão.
- **`neo4j://`, `neo4j+s://`, `neo4j+ssc://`:** driver de roteamento. O
  contexto de roteamento é a query string da URI (cada chave uma vez só, valor
  não vazio) mais `address` com o host:porta da URI. A chave `address` é
  reservada. Esse contexto vai no HELLO (`routing`) e no ROUTE.
- A doc do driver 5 diz que com `neo4j://` o driver recebe tabela de roteamento
  mesmo sem cluster; quem quer uma máquina específica usa `bolt://`.

### 4.2 Quando o driver pede a tabela

- Há uma tabela por database. A primeira nasce com o endereço da URI como único
  roteador.
- Ao adquirir conexão para um modo (leitura ou escrita), a tabela precisa estar
  "fresca": não expirada (atualizada há menos de `ttl` segundos), com pelo menos
  um roteador e com pelo menos um servidor no papel pedido (READ para leitura,
  WRITE para escrita). Senão o driver atualiza.
- Ordem de tentativa: se a última tabela veio sem escritor (ou não existe),
  primeiro o endereço inicial da URI, depois os roteadores conhecidos. Caso
  contrário, os roteadores conhecidos primeiro e o endereço inicial por último.
  Cada roteador é resolvido por DNS e todos os IPs são tentados.
- Se nenhum roteador der tabela válida, levanta `ServiceUnavailable("Unable to
  retrieve routing information")`.
- Tabela sem uso é apagada depois de `ttl` + 30 s.
- Sessão sem `database=`: antes da primeira transação o driver manda ROUTE sem
  `db`. O `rt.db` da resposta vira o database fixado da sessão, que passa a ir
  em `db` nos BEGIN/RUN. Sem `rt.db`, pelo código a tabela fica guardada sob a
  chave nula e as transações seguem sem `db`, sem resolução de nome.
- Bolt 5.8: se todas as conexões do pool anunciarem `ssr.enabled: true`, o
  driver pode pular esse ROUTE e usar o home database guardado em cache,
  confiando no `db` do SUCCESS de BEGIN/RUN e no roteamento do lado do servidor.
  **Recomendação:** não anunciar `ssr.enabled` sem ter roteamento do lado do
  servidor.

### 4.3 ROUTE: pedido e resposta

- **Campos (4.4+):**
  - `routing` Dict: o contexto, com `address`.
  - `bookmarks` List<String>.
  - `extra` Dict: `db` (null = database padrão configurado no servidor) e
    `imp_user` (usuário impersonado, para resolver o home database dele).
- **Na 4.3** o terceiro campo era `db` String em vez de `extra`.
- **Driver:** manda o contexto de roteamento (ou `{}`), a lista de bookmarks da
  sessão e um `extra` com `db`/`imp_user` só quando definidos.
- **SUCCESS:**
  `{"rt": {"ttl": Integer, "db": String, "servers": [{"addresses": [String], "role": "ROUTE" | "READ" | "WRITE"}, ...]}}`
  - `ttl` em segundos.
  - `db` (4.4+) é o database a que a tabela se refere.
  - A doc diz que `servers` tem três entradas, uma por papel.
- **Driver lê:** `rt.servers`, `rt.ttl` e `rt.db`.
  - `rt` ou `servers`/`ttl` ausentes quebram o parsing.
  - Endereço é `host:porta` (porta padrão 7687).
  - O mesmo endereço pode aparecer em mais de um papel.
  - Tabela com zero ROUTE ou zero READ é descartada como falha daquele
    roteador. Zero WRITE é aceito, e a escrita espera o próximo refresh.
- **Exemplo da doc:** ttl 1000, db `"foo"`, um ROUTE, dois READ, dois WRITE.

### 4.4 Escolha do servidor e reação a falhas

- **Leitura** (`execute_read`, `routing_=READ`) vai para um READ. **Escrita**
  (padrão de `session.run` e de `execute_query`) vai para um WRITE.
- Entre os candidatos, o driver pega o de menos conexões em uso; empate é
  sorteado.
- **Falha de conexão** (ServiceUnavailable ou SessionExpired) desativa o
  endereço: sai de todas as tabelas e as conexões ociosas para ele fecham.
  Depois de atualizar a tabela, o driver fecha conexões de servidores que não
  aparecem mais em nenhuma tabela.
- **`Neo.ClientError.Cluster.NotALeader` e
  `Neo.ClientError.General.ForbiddenOnReadOnlyDatabase`:** o endereço sai só da
  lista WRITE daquele database, e o erro é retentável.
- **`Neo.TransientError.General.DatabaseUnavailable`:** desativa o endereço
  inteiro, e o erro é retentável.

### 4.5 Operação na Neo4j (referência)

- `dbms.routing_ttl` define quanto tempo o chamador guarda a tabela (padrão
  5 min, mínimo 1 s).
- Normalmente há um escritor por database. Os demais servidores são leitores, e
  todos os que hospedam o database aparecem como roteadores.
- Roteamento do lado do servidor (`dbms.routing.enabled`, padrão true)
  redireciona internamente a escrita que caiu num membro que não é líder. Só
  funciona com `neo4j://`.
- O endereço que vai na tabela é o anunciado (`server.bolt.advertised_address`).

**Recomendação para o Brainwire:**

- **Instância única com `neo4j://`:** o próprio endereço anunciado nos três
  papéis, com `ttl` de 300 s e o home database do usuário em `rt.db`.
- **Cluster Raft:** o líder em WRITE, os seguidores (e opcionalmente o líder) em
  READ, todos em ROUTE.
- **Seguidor que recebe escrita:** FAILURE `Neo.ClientError.Cluster.NotALeader`.

---

## 5. Bookmarks

### 5.1 O que a doc diz

- Bookmark é um token que representa um estado do banco. Com bookmarks no
  pedido, o servidor não executa a transação antes de esses estados existirem.
- Dentro de uma sessão o encadeamento é automático.
- Para encadear sessões diferentes: juntar os `last_bookmarks()` num objeto
  `Bookmarks` e passar na criação da nova sessão, ou compartilhar um bookmark
  manager.
- `execute_query` usa por padrão o `driver.execute_query_bookmark_manager`, e
  queries seguidas enxergam as anteriores.
- Custo: a query espera o estado se propagar no cluster, então a doc recomenda
  bookmark só quando consistência causal for necessária.
- Na Neo4j, só servidor que já processou a transação marcada roda a próxima
  transação do cliente.

### 5.2 Como o driver 5.28.6 propaga

- **Formato:** bookmark é String opaca. O driver nunca interpreta.
- **Onde vai:**
  - em `bookmarks` do BEGIN (transação explícita ou gerenciada);
  - no RUN de autocommit;
  - no ROUTE.
  Nos três casos, só se a lista não for vazia.
- **De onde vem:**
  - `bookmark` do SUCCESS do COMMIT;
  - `bookmark` do último SUCCESS (`has_more` false) de PULL/DISCARD no
    autocommit.
- **Na sessão:** o bookmark novo substitui os anteriores, e a sessão passa a
  mandar só ele. Se o servidor não devolver bookmark, a sessão mantém o que
  tinha.
- **No bookmark manager:** a cada atualização ele remove os bookmarks que a
  transação usou e acrescenta os novos. Desde o driver 5.3 o manager não separa
  bookmarks por database, então uma lista pode misturar bookmarks de databases
  diferentes.
- **No ROUTE:** um comentário no código do driver explica que os bookmarks
  garantem que um database recém-criado já exista antes de pedir a tabela dele.

### 5.3 O que o servidor deve fazer

- **Antes de começar a transação**, garantir que o estado local inclua tudo que
  cada bookmark representa. Se não chegar lá a tempo, responder
  `Neo.TransientError.Transaction.BookmarkTimeout`: é transiente, então a
  transação gerenciada do driver tenta de novo.
- **Bookmark que não se consegue interpretar:**
  `Neo.ClientError.Transaction.InvalidBookmark`.
- **Mistura de databases:** a descrição de
  `Neo.ClientError.Transaction.InvalidBookmarkMixture` diz que misturar
  bookmarks gerados por databases diferentes é proibido. Como o bookmark
  manager do driver mistura (seção 5.2), **não achei** doc oficial explicando como a
  Neo4j 5 trata essa mistura na prática.
- **Resposta:** devolver bookmark novo em todo COMMIT e no fim de todo
  autocommit.
- **Timeout:** a Neo4j tem a configuração dinâmica
  `db.transaction.bookmark_ready_timeout`. **Não achei** a descrição nem o
  padrão nas páginas consultadas.

**Recomendação para o Brainwire (coerente com o PLANO.md):**

- **Conteúdo do bookmark:** identifica o database e o índice do log Raft
  aplicado no commit.
- **Instância única:** todo bookmark válido já está satisfeito ao chegar.
- **Seguidor:** espera o índice aplicado alcançar o do bookmark, com timeout.
- **Lista com vários databases:** aceitar e esperar por cada um. Recusar
  quebraria `execute_query` entre databases.

---

## 6. Erros

### 6.1 Formato da FAILURE

- **Bolt 1 a 5.6:** `{"code": "Neo.<Classificação>.<Categoria>.<Título>",
  "message": "..."}`.
- **Bolt 5.7+:**
  - `neo4j_code` String (o antigo `code`);
  - `message` String;
  - `gql_status` String (o marcador estável, preferido ao `neo4j_code`);
  - `description` String (descreve o `gql_status`);
  - `diagnostic_record` Dict, com `OPERATION` (padrão `""`), `OPERATION_CODE`
    (padrão `"0"`), `CURRENT_SCHEMA` (padrão `"/"`) e `_classification` (ex.:
    `"CLIENT_ERROR"`), omitido quando é o padrão e podendo ter entradas extras;
  - `cause` Dict opcional, com a mesma forma e aninhável.
- **Bolt 6.0:** o texto de `message` não é mais estável; o marcador estável é
  `gql_status`.
- **Classificações do driver:** `CLIENT_ERROR`, `DATABASE_ERROR`,
  `TRANSIENT_ERROR` e `UNKNOWN`.

### 6.2 Como o driver 5.28.6 lê a FAILURE

- **Até 5.6:** lê `code` e `message`. Sem `code`, usa
  `Neo.DatabaseError.General.UnknownError`.
- **5.7 em diante:**
  - lê `neo4j_code`, não `code`;
  - exige `gql_status`, `description` e `message` juntos, e se faltar um,
    troca os três por valores genéricos (status `50N42` e a mensagem "An
    unknown error occurred");
  - sem `neo4j_code`, o erro vira `Neo.DatabaseError.General.UnknownError`.
  Um servidor que negocie 5.7+ e mande o formato antigo faz todo erro virar
  DatabaseError desconhecido, não retentável e sem mensagem.
- **A classe vem do código:** o driver quebra o código em quatro partes por
  ponto. A segunda parte (`ClientError`, `TransientError`, `DatabaseError`)
  escolhe a classe; código com outro formato vira DatabaseError.
- **Mesmo em 5.7+, a classificação e a retentativa saem do `neo4j_code`**, não
  do `gql_status` nem do `_classification`.
- **Efeito na plataforma:** o adaptador da Neo4j decide o fallback de
  `CALL { ... } IN TRANSACTIONS` procurando marcadores como `syntaxerror` no
  código e na mensagem. Código e mensagem fiéis aos da Neo4j não são
  cosméticos.

### 6.3 Retentativa e roteamento

A transação gerenciada (`execute_read`, `execute_write`, `execute_query`) tenta
de novo quando `is_retryable()` é verdadeiro, com backoff exponencial, até
`max_transaction_retry_time` (padrão 30 s). Autocommit (`session.run`) nunca é
retentado.

| Código ou situação | Classe no driver | Retenta? | Efeito no roteamento |
|---|---|---|---|
| `Neo.TransientError.*` (regra geral) | TransientError | sim | nenhum |
| `Neo.TransientError.General.DatabaseUnavailable` | DatabaseUnavailable | sim | desativa o endereço |
| `Neo.ClientError.Cluster.NotALeader` | NotALeader (subclasse de TransientError) | sim | tira o endereço dos WRITE daquele database |
| `Neo.ClientError.General.ForbiddenOnReadOnlyDatabase` | ForbiddenOnReadOnlyDatabase (subclasse de TransientError) | sim | tira o endereço dos WRITE |
| `Neo.ClientError.Security.AuthorizationExpired` | reclassificado como TransientError | sim | marca todas as conexões daquele endereço para reautenticar |
| `Neo.TransientError.Transaction.Terminated` e `...LockClientStopped` | reescritos como `Neo.ClientError.Transaction.*` | não | nenhum |
| `Neo.ClientError.Security.*` tratado pelo auth manager (`AuthManagers.basic`: `Unauthorized`; `AuthManagers.bearer`: `TokenExpired` e `Unauthorized`; credencial fixa não trata nada) | AuthError/TokenExpired | sim, se o manager tratar | nenhum |
| demais `Neo.ClientError.*` (sintaxe, constraint, tipo...) | ClientError e subclasses | não | nenhum |
| `Neo.DatabaseError.*` | DatabaseError | não | nenhum |
| queda de conexão, sem servidor para o modo | ServiceUnavailable, SessionExpired | sim | desativa o endereço |
| queda durante o COMMIT | IncompleteCommit | não | nenhum |

Subclasses de ClientError mapeadas por código:

- `CypherSyntaxError`: `Statement.SyntaxError`, `Statement.InvalidSyntax`.
- `CypherTypeError`: `Statement.TypeError`, `Statement.InvalidType`,
  `Procedure.TypeError`.
- `ConstraintError`: `Schema.ConstraintValidationFailed`,
  `Schema.ConstraintViolation` e as equivalentes de `Statement`.
- `Forbidden`: `Security.Forbidden`, `General.ReadOnly` e outras.
- `AuthError`: `Security.Unauthorized`, `Security.AuthorizationFailed`.
- `TokenExpired`: `Security.TokenExpired`.

### 6.4 Erros durante a descoberta (ROUTE)

- **Falham na hora,** sem tentar outro roteador:
  - `Neo.ClientError.Database.DatabaseNotFound`;
  - `Neo.ClientError.Transaction.InvalidBookmark`;
  - `Neo.ClientError.Transaction.InvalidBookmarkMixture`;
  - `Neo.ClientError.Statement.TypeError`;
  - `Neo.ClientError.Statement.ArgumentError`;
  - `Neo.ClientError.Request.Invalid`;
  - qualquer `Neo.ClientError.Security.*`, menos `AuthorizationExpired`.
- **Qualquer outro Neo4jError,** ServiceUnavailable ou SessionExpired no ROUTE:
  o driver passa ao próximo roteador.
- A lista de códigos da Neo4j também tem `Neo.ClientError.Cluster.Routing`
  (não conseguiu rotear o pedido). O driver 5.28.6 não trata esse código de
  forma especial.

### 6.5 `server` que não começa com "Neo4j/"

- **Driver 5.x:**
  - Ao fim do `hello()`, depois das respostas de HELLO e LOGON, o driver chama
    `check_supported_server_product`, e o teste é
    `if not agent.startswith("Neo4j/")`.
  - Se falhar, levanta `neo4j.exceptions.UnsupportedServerProduct(agent)` e
    mata a conexão.
  - Na 5.28.6 a exceção herda direto de `Exception`, então a retentativa não a
    pega, e `verify_connectivity`, sessões e `execute_query` falham todos.
  - Sem o campo `server`, o driver compara a string `"None"` e falha igual.
- **Driver 6.x:** a checagem saiu no PR #1208, que tira as restrições à string
  do agente (merge em 12/06/2025, linha 6.0). No 6.x, `UnsupportedServerProduct`
  passou a significar "nenhuma versão Bolt em comum".
- **Prática de mercado:** o Memgraph manda por padrão
  `Neo4j/v5.11.0 compatible graph database server - Memgraph` (seção 7).
- **Recomendação para o Brainwire:** algo como
  `Neo4j/5.26.0 compatible (Brainwire/0.1.0)`. O prefixo `Neo4j/` é o que
  importa para o driver 5.x.

### 6.6 Outras falhas que derrubam conexão

- **FAILURE no HELLO ou no LOGON:** o servidor fecha e o driver mata a conexão.
- **FAILURE no RESET:** o driver descarta a conexão.
- **Mensagem inesperada:** erro de protocolo e DEFUNCT.

---

## 7. Memgraph e Amazon Neptune falando Bolt

### 7.1 Memgraph (documentação pública)

- **Versões do Bolt:** 1 (desde o Memgraph 0.8.0), 4.0 e 4.1 (desde 1.2.0), 4.3
  (desde 2.1.0) e 5.2 (desde 2.9.0).
- **Nome do servidor:** vem da flag `--bolt-server-name-for-init`. O padrão é
  `Neo4j/v5.11.0 compatible graph database server - Memgraph`. A doc do cliente
  Python manda, no Memgraph anterior a 2.11, trocar para `Neo4j/5.2.0` para o
  driver da Neo4j funcionar.
- **Roteamento (`neo4j://`):** só em alta disponibilidade (Enterprise), falando
  com os coordenadores.
  - O ROUTE vai a qualquer coordenador. A tabela traz writers = o MAIN atual,
    readers = as réplicas (e opcionalmente o MAIN), routers = os coordenadores.
  - O TTL é fixo em 5 minutos, e algumas mudanças de topologia ficam invisíveis
    até ele vencer.
  - Configurar o cluster exige `bolt://`, não `neo4j://`.
  - Escrita numa réplica falha.
- **Vários databases:** multi-tenant é Enterprise desde a 2.10. A doc recomenda
  passar `database_="memgraph"` nas queries. Impersonação também é Enterprise.
- **`element_id`:** devolve o ID interno do nó, que não é o ID definido pelo
  usuário.
- **Bookmarks:** **não achei** nada na doc do Memgraph.

### 7.2 Amazon Neptune (documentação pública)

- **Esquema e endpoints:** a doc manda conectar com o esquema `bolt` e não
  menciona `neo4j://`. Com várias instâncias, a AWS recomenda dois drivers: um
  para o escritor e um para as réplicas de leitura (ou um por endpoint
  customizado).
- **Transporte:** só TCP, sem WebSocket. Não dá para pôr Application Load
  Balancer na frente. A porta é a do cluster (8182 nos exemplos), e os exemplos
  usam criptografia (`encrypted=True`, `bolt+s`).
- **Versão:** o servidor escolhe a maior versão Bolt adequada entre 1, 2, 3 e
  4.0. **Inferência minha:** diante das propostas do driver Python 5.28.6, isso
  resulta em Bolt 3.
- **Conexões:**
  - até 1000 conexões Bolt simultâneas (512 em t3.medium e t4g.medium);
  - conexão ociosa por 20 minutos é fechada;
  - com IAM, a conexão cai pouco mais de 10 dias depois de aberta.
- **Autenticação:**
  - sem IAM, as credenciais são ignoradas (dá para usar `AuthTokens.none()`);
  - com IAM, o token é uma assinatura SigV4 posta em `credentials` do esquema
    `basic`, com principal fictício e realm `realm`;
  - a assinatura vence em cerca de 5 minutos, o que quebrava drivers sem
    renovação de token.
- **Transações e erros (desvios da spec do Bolt):**
  - uma transação por conexão;
  - query nova com resultado anterior não consumido é descartada, e para
    descartar o anterior o cliente precisa mandar RESET;
  - exceção dentro de transação faz o servidor desfazer a transação e fechar a
    conexão.
- **Bookmarks:** aparecem na lista de recursos da Neo4j ausentes no Neptune.
  Para ler a própria escrita, a AWS manda ler pelo endpoint do cluster (réplicas
  são eventualmente consistentes, em geral abaixo de 100 ms).
- **Vários databases:** um grafo por cluster.
- **IDs:** IDs de nó e relação são strings, e `id()` devolve string.
- **Procedures:** não há `CALL`. **Inferência minha:** isso exclui o roteamento
  por procedure dos Bolt 3 e 4.x.

---

## Lacunas (o que não achei)

- Documentação oficial do Bolt 6.1 e do PackStream v2. Só existem no código do
  driver 6.3.1.
- Descrição e valor padrão de `db.transaction.bookmark_ready_timeout`.
- Como a Neo4j 5 trata bookmarks de databases diferentes na mesma lista, que é
  o que o bookmark manager do driver produz.
- Formato dos bookmarks da Neo4j (a doc os trata como opacos).
- O que Memgraph documenta sobre bookmarks.
- A string `server` que o Neptune anuncia.

---

## 8. Fontes

Especificação do Bolt (site e fonte):

- https://neo4j.com/docs/bolt/current/
- https://neo4j.com/docs/bolt/current/bolt/
- https://neo4j.com/docs/bolt/current/bolt/handshake/
- https://neo4j.com/docs/bolt/current/bolt/message/
- https://neo4j.com/docs/bolt/current/bolt/server-state/
- https://neo4j.com/docs/bolt/current/bolt/structure-semantics/
- https://neo4j.com/docs/bolt/current/packstream/
- https://neo4j.com/docs/bolt/current/bolt-compatibility/
- https://neo4j.com/docs/bolt/current/appendix/version-5/
- https://neo4j.com/docs/bolt/current/appendix/connection-hints/
- https://neo4j.com/docs/bolt/current/appendix/message-exchange-examples/
- https://github.com/neo4j-drivers/docs-bolt (fonte AsciiDoc em `modules/ROOT/pages/`)

Driver Python (código lido na tag 5.28.6, salvo indicação):

- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt_socket.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt3.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt4.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt5.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_common.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_pool.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_routing.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/exceptions.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/api.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/driver.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/workspace.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/session.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/transaction.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/result.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/bookmark_manager.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/auth_management.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_auth_management.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_codec/hydration/v1/hydration_handler.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_codec/hydration/v2/hydration_handler.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_work/summary.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_conf.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_meta.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.3.1/src/neo4j/_async/io/_bolt.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.3.1/src/neo4j/_async/io/_bolt6.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.3.1/src/neo4j/_codec/packstream/v2/__init__.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.x/CHANGELOG.md
- https://github.com/neo4j/neo4j-python-driver/pull/1208
- https://github.com/neo4j/neo4j-python-driver/pull/1164
- https://pypi.org/project/neo4j/

Manuais da Neo4j:

- https://neo4j.com/docs/python-manual/5/bookmarks/
- https://neo4j.com/docs/python-manual/5/connect-advanced/
- https://neo4j.com/docs/python-manual/current/bookmarks/
- https://neo4j.com/docs/status-codes/current/errors/all-errors/
- https://neo4j.com/docs/operations-manual/current/clustering/introduction/
- https://neo4j.com/docs/operations-manual/current/clustering/setup/routing/
- https://neo4j.com/docs/operations-manual/current/configuration/configuration-settings/
- https://neo4j.com/docs/operations-manual/current/configuration/dynamic-settings/
- https://neo4j.com/docs/cypher-manual/current/functions/scalar/#functions-elementid

Memgraph:

- https://memgraph.com/docs/client-libraries
- https://memgraph.com/docs/client-libraries/python
- https://memgraph.com/docs/database-management/configuration
- https://memgraph.com/docs/clustering/high-availability/querying-the-cluster-in-high-availability

Amazon Neptune:

- https://docs.aws.amazon.com/neptune/latest/userguide/access-graph-opencypher-bolt.html
- https://docs.aws.amazon.com/neptune/latest/userguide/migration-compatibility.html
- https://docs.aws.amazon.com/neptune/latest/userguide/feature-opencypher-compliance.html

Neste repositório:

- `client-core/pyproject.toml` (extra `neo4j`: `neo4j>=5.0,<6`)
- `client-core/src/client_core/repository/neo4j.py` (uso de `element_id`,
  `execute_query` e do código de erro)
- `client-brainwire/PLANO.md` (bookmark = índice do log Raft)
