# RocksDB no Brainwire: memória, recursos compartilhados, carga e purga

Pesquisa de 24/09/2026 para a seção 1 (armazenamento) do spec do Brainwire:
motor de grafo em Rust sobre RocksDB, 5 bilhões de elementos por grafo, processo
limitado a 16 GB de RAM, dados em NVMe e um RocksDB por database na mesma
instância.

**Como foi feita.** Tudo lido pela web, sem clone e sem download: a wiki do
RocksDB, headers e alguns arquivos de código do branch `main` do
`facebook/rocksdb` (pelo raw do GitHub), o `HISTORY.md`, posts do blog oficial,
issues e PRs, a documentação do crate no docs.rs, o código do crate na tag
`v0.25.0` e a API do crates.io. Os headers são os do `main`, que em 24/09 estava
na 11.11.0; o crate empacota a 11.8.1, então algum comentário ou padrão pode
diferir entre as duas. O texto das fontes está parafraseado em português, com a
fonte ao lado. Onde a fonte não confirmou algo, está escrito "não achei". O que
está marcado como **recomendação** é inferência minha. Conta sem fonte ao lado é
minha, com as hipóteses escritas.

**Versões.** O crate `rocksdb` (repositório `rust-rocksdb/rust-rocksdb`) está na
0.25.0, de 16/08/2026, sobre o `librocksdb-sys` 0.19.0+11.8.1, isto é, RocksDB
11.8.1. A versão anterior, 0.24.0, saiu em 10/08/2025. Existe um fork publicado
com outro nome, o crate `rust-rocksdb` (repositório `zaidoon1/rust-rocksdb`), na
0.53.0 de 05/09/2026, também sobre a 11.8.1 (`rust-librocksdb-sys`
0.48.1+11.8.1), com dez versões entre janeiro e setembro de 2026. Neste texto,
"o crate" é o `rocksdb` 0.25.0; o fork aparece como alternativa quando o crate
não expõe algo.

---

## Resumo

Configuração recomendada, tudo a medir no gate de 5 bilhões:

1. Um único `Cache::new_hyper_clock_cache(capacidade, 0)` no processo, passado a
   todas as famílias de todos os databases. Como o custo das memtables é cobrado
   nele, a capacidade inclui as memtables: para ~9 GB de blocos e ~1 GB de
   memtables, o cache é de 10 GB. A tabela 1.5 do spec soma os dois por fora.
2. Um único `WriteBufferManager` de 1 GB, com custo no cache e
   `allow_stall = false`, por causa de travamentos relatados com muitas famílias
   e databases no mesmo gerenciador (seção 8.3).
3. Índice e filtro particionados nas famílias grandes, com o topo fixado e as
   partições no cache, e bloco de dados de 16 KB. O que precisa ficar residente
   cai para dezenas de MB por grafo (seção 1.3).
4. Filtro escolhido por família. Bloom de 10 bits em todas as 10 bilhões de
   chaves custaria 12,5 GB. Com prefixo de 8 bytes em `out` e `in`,
   `optimize_filters_for_hits` onde a busca quase sempre acha e filtro de chave
   inteira só na `pk`, a conta fica entre 2,3 e 5,4 GB, que ficam no cache e não
   precisam ser residentes.
5. LZ4 em `compression_type` e ZSTD em `bottommost_compression`, com
   `max_dict_bytes` de 16 KB e `zstd_max_train_bytes` de 100 vezes isso. Evitar
   `compression_per_level`, cujas posições passam a contar a partir do nível
   base com `level_compaction_dynamic_level_bytes`.
6. Carga de 5 bilhões pelo caminho offline: SST escrito em ordem, faixas
   disjuntas por família, ingerido num database vazio. Os arquivos vão direto
   para o último nível e não pagam a amplificação de escrita da compactação
   (33 vezes no exemplo do guia de tuning).
7. Nada de `prepare_for_bulk_load()` no `bulk()` online: ele reduz o DB a 2
   níveis e desliga a compactação automática.
8. Para o commitador do spec, DB comum com validação própria. O
   `OptimisticTransactionDB` está pronto no crate, mas recusa DeleteRange no
   caminho da transação e segura histórico de memtables em memória (seção 5.6).
9. Purga física com `compact_range_cf_opt` e `BottommostLevelCompaction::Force`
   na faixa afetada, com `exclusive_manual_compaction = false`. O motor só
   declara o dado apagado depois de conferir snapshots, arquivos obsoletos, WAL,
   checkpoints e backups (seção 6.5).
10. `max_open_files = -1` (padrão) e `ulimit -n` do container acima do total de
    SSTs de todos os databases.

O que o crate não expõe e pesa aqui: `WriteBatchWithIndex`; as opções do
`OptimisticTransactionDB` (política de validação e buckets de lock) e a política
de escrita do `TransactionDB`; `SstFileManager`; `strict_capacity_limit` e
`high_pri_pool_ratio` do LRU; `cache_usage_options`;
`fail_if_not_bottommost_level`, `link_files` e `PrepareFileIngestion` na
ingestão; `max_dict_buffer_bytes` e `use_zstd_dict_trainer`; ajustes do
`BackupEngineOptions` além de threads e sync; `two_write_queues`;
`auto_prefix_mode`. O fork cobre boa parte (seção 7). Com a feature `raw-ptr`, o
crate entrega o ponteiro C do DB, do `Options` e do `Env` (trait `AsRawPtr`), e o
`librocksdb-sys` traz o C API, então vários desses itens saem por FFI, com
`unsafe`.

---

## 1. Memória com bilhões de chaves

### 1.1 Índice e filtro particionados

Sem partição, cada SST tem um bloco de índice e um de filtro inteiros. A wiki de
índice particionado dá a ordem de grandeza: num SST de 256 MB, é comum o índice
ter 0,5 MB e o filtro 5 MB, contra blocos de dados de 4 a 32 KB. Com partição, o
índice e o filtro viram blocos pequenos (as partições) com um índice de topo;
só o topo precisa estar em memória, e as partições entram no block cache quando
alguém precisa delas.

| Opção no C++ | No crate | Efeito |
|---|---|---|
| `index_type = kTwoLevelIndexSearch` | `BlockBasedOptions::set_index_type(BlockBasedIndexType::TwoLevelIndexSearch)` | índice em partições; elas usam o block cache mesmo com `cache_index_and_filter_blocks = false` (`table.h`) |
| `partition_filters = true` | `set_partition_filters(true)` | filtro em partições; exige filtro "full", não o antigo por bloco |
| `metadata_block_size = 4096` | `set_metadata_block_size(4096)` | tamanho alvo de cada partição |
| `cache_index_and_filter_blocks = true` | `set_cache_index_and_filter_blocks(true)` | o topo também vai para o cache e conta na capacidade |
| `cache_index_and_filter_blocks_with_high_priority` (padrão `true`) | `set_cache_index_and_filter_blocks_with_high_priority(true)` | índice, filtro e dicionário entram com prioridade alta |
| `pin_top_level_index_and_filter` (padrão `true`) | `set_pin_top_level_index_and_filter(true)` | o table reader segura o topo, que só sai do cache quando o arquivo fecha |
| `metadata_cache_options` | `set_top_level_index_pinning_tier`, `set_partition_pinning_tier`, `set_unpartitioned_pinning_tier`, com `BlockBasedTablePinningTier::{Fallback, None, FlushAndSimilar, All}` | fixação por tipo de bloco e por camada de arquivo |
| `decouple_partitioned_filters` | não exposto | padrão `true` desde a 10.6.0, já marcado como deprecado |

Os dois `pin_*` estão marcados como deprecados no `table.h` atual em favor de
`metadata_cache_options`, cujo padrão (`kFallback`) repete o comportamento
antigo. O crate expõe as duas formas.

Custos, pela wiki: o topo ocupa de 0,1% a 1% do índice e do filtro; se ele não
estiver no cache, a leitura faz uma I/O a mais; leitura aleatória frequente no
mesmo SST carrega uma partição diferente a cada vez. A wiki também avisa que
blocos fixados podem passar da capacidade do cache quando `strict_capacity_limit`
está desligado, que é o padrão. No teste com Linkbench em SSD de 300 GB, reduzir
a memória de 6 para 2 GB derrubou a vazão de 38 mil para 30 mil com partição e
para 23 mil sem.

### 1.2 Bloom, Ribbon e custo por chave

Bloom, pela wiki do filtro:

| Bits por chave | Falso positivo |
|---|---|
| 1,5 | 50% |
| 2,9 | 25% |
| 4,9 | 10% |
| 9,9 | 1% |
| 15,5 | 0,1% |

O tamanho é linear: com 10 bits, o filtro ocupa número de chaves × 10 bits (wiki
de uso de memória). O Bloom do `format_version = 5` em diante usa hash de 64 bits
e, segundo a wiki, escala para muitos bilhões de chaves num filtro só; o formato
antigo não descia de ~0,1% de falso positivo nem com 100 bits por chave.

O Ribbon ocupa cerca de 30% menos que o Bloom para a mesma taxa, ao custo de 3 a
4 vezes mais CPU na construção: `NewRibbonFilterPolicy(9.9)` dá 1% com ~7 bits por
chave. O `filter_policy.h` avisa que construir um filtro muito grande usa memória
temporária que ninguém contabiliza: ~3 GB para 100 milhões de chaves num único
filtro, contra 1 GB no Bloom. Com filtro particionado, cada partição é finalizada
quando é cortada (`CutAFilterBlock` chama `Finish`, em
`partitioned_filter_block.cc`), então o pico fica por partição, não por arquivo.
O modo híbrido (`bloom_before_level`) usa Bloom nos níveis de cima e Ribbon
embaixo; o valor padrão 0 gera Bloom só nos flushes.

Três opções mudam a conta:

- `optimize_filters_for_memory` (padrão `true` desde a 9.2.0) ajusta o tamanho de
  cada filtro para reduzir a fragmentação do alocador, mantendo a taxa de falso
  positivo agregada no alvo.
- `optimize_filters_for_hits` (da família, padrão `false`) não grava filtro no
  último nível, que tem a maior parte dos dados. Para busca que acha, esse filtro
  não ajuda; os dos outros níveis continuam úteis, porque dizem se vale descer
  naquele nível (`advanced_options.h`).
- Com prefix extractor e `whole_key_filtering = false`, só o prefixo entra no
  filtro, e prefixos repetidos em sequência viram uma entrada só: o
  `filter_policy.cc` descarta hash igual ao anterior e comenta que, com prefixo,
  a repetição é comum e sempre adjacente. Como as arestas de um nó são contíguas
  em `out` e `in`, o filtro dessas famílias cresce com o número de nós distintos
  por SST, não com o número de arestas.

### 1.3 A conta para 10 bilhões de entradas

Hipóteses (minhas):

- 5 bilhões de elementos: 1,0 bilhão de nós, 3,5 bilhões de arestas e 0,5 bilhão
  de eventos.
- 10 bilhões de chaves: `node` 1,0; `pk` 1,0; `out` 3,5; `in` 3,5; `event` 0,5;
  `lbl`, `idx` e `ident` juntas 0,5.
- Em disco, de 175 GB (meta do README) a 350 GB (70 bytes por elemento, a régua
  do Neo4j); descomprimido, ~500 GB.
- Com `level_compaction_dynamic_level_bytes` (padrão desde a 8.4), a wiki de
  compactação por nível garante ~90% dos dados no último nível e ~9% no
  penúltimo.

**Filtros.** 10 bits por chave são 1,25 byte por entrada.

| Política | Entradas no filtro | Bloom, 10 bits | Ribbon, ~7 bits |
|---|---|---|---|
| Chave inteira em todas as famílias e níveis | 10 bi | 12,5 GB | 8,75 GB |
| Chave inteira, `optimize_filters_for_hits` em tudo | ~1 bi | 1,25 GB | 0,88 GB |
| Plano P1 (abaixo) | até 4,3 bi | até 5,4 GB | até 3,8 GB |
| Plano P2: P1 com `optimize_filters_for_hits` também em `out`, `in` e `event` | até 1,85 bi | até 2,3 GB | até 1,6 GB |

Plano P1:

| Família | Chaves | Filtro | Entradas | Bloom |
|---|---|---|---|---|
| `node` | 1,0 bi | chave inteira e `optimize_filters_for_hits`: quem busca por id quase sempre acha | ~0,1 bi | ~0,13 GB |
| `pk` | 1,0 bi | chave inteira em todos os níveis: o `MERGE` de entidade nova é busca que não acha | 1,0 bi | 1,25 GB |
| `out` | 3,5 bi | prefixo de 8 bytes, sem chave inteira | até 1,0 bi no último nível (uma por nó com aresta) e até 0,35 bi nos outros | até 1,7 GB |
| `in` | 3,5 bi | idem | até 1,35 bi | até 1,7 GB |
| `event` | 0,5 bi | prefixo com stream e entidade | até 0,5 bi | até 0,63 GB |
| `lbl`, `idx`, `ident` | 0,5 bi | nenhum: são lidas por faixa, e o filtro de chave inteira só serve a point lookup (wiki do filtro) | 0 | 0 |

Os "até" supõem que toda chave dos níveis de cima tem prefixo próprio e que todo
nó tem aresta; na prática o número é menor.

**Índice.** Cada entrada guarda um separador, que é a chave encurtada e sem
número de sequência desde o `format_version = 3`, e a posição do bloco,
codificada em delta desde o 4 (wiki do formato do índice). Supondo de 16 a 32
bytes por entrada e 500 GB descomprimidos:

| Bloco de dados | Blocos | Índice |
|---|---|---|
| 4 KB (padrão) | 122 milhões | 2,0 a 3,9 GB |
| 16 KB | 30,5 milhões | 0,5 a 1,0 GB |
| 32 KB | 15,3 milhões | 0,24 a 0,5 GB |

Com 1 TB descomprimido, os números dobram. Duas medidas públicas batem com a
linha de 16 KB: a proporção da wiki (0,5 MB de índice num SST de 256 MB, 0,2%) dá
0,34 a 0,68 GB para 175 a 350 GB em disco, e o exemplo da wiki "Space Tuning"
mostra 0,97 GB de índice e 1,16 GB de filtro para 624,57 GB de dados. A mesma
página diz que `format_version = 5` com `index_block_restart_interval = 16` deixa
o índice pelo menos 4 vezes menor; a wiki do formato do índice fala em metade só
com o restart interval, pagando mais CPU.

**Topo fixado.** Uma entrada por partição de 4 KB, com ~25 bytes cada:

| Estrutura | Total | Partições | Topo |
|---|---|---|---|
| Índice | 0,5 a 1,0 GB | 0,12 a 0,24 milhão | 3 a 6 MB |
| Filtro P2 | 2,3 GB | 0,57 milhão | ~14 MB |
| Filtro P1 | 5,4 GB | 1,3 milhão | ~33 MB |
| Filtro em tudo | 12,5 GB | 3,05 milhões | ~76 MB |

Fica dentro da faixa de 0,1% a 1% da wiki.

**Leitura fria.** Com o topo fixado e o índice inteiro (0,5 a 1 GB) no cache em
prioridade alta, um get frio em `node` que chega ao último nível lê um bloco de
dados do disco. Se a partição do índice também estiver fria, são duas leituras.
Em 9 GB de blocos, o P2 deixa ~6 GB para dados; o P1, ~3 GB.

### 1.4 O que precisa ficar residente

1. O topo do índice e do filtro de cada SST aberto: dezenas de MB por grafo.
2. As memtables ativas e imutáveis, até o teto do `WriteBufferManager`.
3. Os table readers de todos os SSTs abertos com `max_open_files = -1`. Essa
   memória fica fora do cache por padrão; cobrá-la no cache é
   `cache_usage_options`, que o crate não expõe. Não achei número oficial por
   arquivo; a medida é `properties::ESTIMATE_TABLE_READERS_MEM`.
4. Os blocos que iteradores abertos seguram: pela wiki, cerca de iteradores ×
   tamanho do bloco × (níveis menos 1, mais arquivos no L0). Mil iteradores com
   bloco de 16 KB, 7 níveis e 4 arquivos no L0 dão ~164 MB.
5. Com `OptimisticTransactionDB` ou `TransactionDB`, o histórico de memtables já
   descarregadas que a validação usa (seção 5.2).

Partições de índice e filtro, dicionários e blocos de dados só precisam caber no
cache na parte quente.

Um relato público mostra o desenho oposto: 302 milhões de chaves, 8,34 GiB em
disco, bloco de 4 KB, Ribbon de 10 bits com prefixo e
`cache_index_and_filter_blocks = false`, com 4,68 GB de índice e filtro em
memória e subindo (issue 12942 do RocksDB, sem resposta de mantenedor quando li).
As chaves eram URLs. O índice cresce com o tamanho da chave, e fora do cache ele
não tem teto.

---

## 2. Recursos compartilhados entre databases

A FAQ do RocksDB aceita um DB por shard e diz que pool de threads, block cache e
rate limiter podem ser compartilhados entre as instâncias. A documentação de
memória do Kafka Streams (Confluent) dá a receita para vários RocksDB no mesmo
processo: índice e filtro no block cache, memtables limitadas por um
`WriteBufferManager` compartilhado e cobrado no cache, e o mesmo `Cache` em todas
as instâncias.

### 2.1 Block cache único

- A wiki de ajuste básico manda usar o mesmo `Cache` nas opções de tabela de
  todas as famílias de todos os DBs do processo, e sugere cerca de um terço do
  orçamento de memória para ele.
- Família sem cache configurado ganha um próprio, de 32 MB (`table.h`). Com 10
  famílias por database, esquecer uma é criar um cache escondido.
- LRU: dividido em até 64 shards de no mínimo 512 KB (wiki do block cache), com
  `high_pri_pool_ratio = 0.5` e `strict_capacity_limit = false` por padrão
  (`cache.h`). Com índice e filtro em prioridade alta, a lista do LRU se divide
  em duas.
- HyperClockCache (HCC): desde a 10.7.0, o HCC sem `estimated_entry_charge` é
  dado como pronto para produção e preferido ao LRU, e virou a implementação
  padrão quando nenhum cache é passado (`HISTORY.md` e notas da v10.7.5).
  Limites, pelo `cache.h`: serve só como block cache (não como row cache nem blob
  cache), a prioridade é aplicada com menos rigor que no LRU, e o modo automático
  depende de mmap anônimo. O `estimated_entry_charge` fica difícil de prever
  quando o `WriteBufferManager` cobra memtables no cache, então 0 é o valor certo
  aqui. O post de 2025 do blog sobre rastreamento unificado de memória configura
  justamente um HCC com o custo das memtables cobrado nele.

No crate:

- `Cache::new_hyper_clock_cache(capacity, estimated_entry_charge)`. A doc do
  crate também dá o HCC como recomendado e indica 0.
- `Cache::new_lru_cache(capacity)` e `Cache::new_lru_cache_opts(&LruCacheOptions)`.
  O `LruCacheOptions` tem só `set_capacity` e `set_num_shard_bits`: não há
  `high_pri_pool_ratio`, `low_pri_pool_ratio` nem `strict_capacity_limit`. O
  `librocksdb-sys` tem `rocksdb_cache_create_lru_with_strict_capacity_limit`.
- `Cache` é `Clone` sobre um `Arc` do ponteiro C, e
  `BlockBasedOptions::set_block_cache(&cache)` guarda um clone
  (`src/db_options.rs`). Uso: `get_usage()`, `get_pinned_usage()`; ajuste:
  `set_capacity()`.

### 2.2 WriteBufferManager compartilhado e cobrado no cache

Pela wiki e pelo `write_buffer_manager.h`:

- um objeto só, passado às opções de todos os DBs, limita a soma das memtables
  de todas as famílias;
- o flush dispara numa família quando a memtable mutável passa de ~90% do limite,
  ou quando o total passa do limite e a mutável já passou de 50%;
- com cache, o WBM reserva no cache, em entradas vazias, o tamanho da memória que
  as memtables alocaram, e o cache despeja blocos para abrir espaço;
- com `allow_stall = true`, quando o uso passa do limite, todas as threads que
  escrevem, em todos os DBs, param até o uso cair.

Consequência para a tabela 1.5 do spec: com custo no cache, as memtables ficam
dentro da capacidade do cache. Um cache de 9 GB com 1 GB de memtables deixa 8 GB
para blocos.

No crate: `WriteBufferManager::new_write_buffer_manager(buffer_size, allow_stall)`
e `new_write_buffer_manager_with_cache(buffer_size, allow_stall, cache)`, ligados
por `Options::set_write_buffer_manager(&wbm)`. O tipo é `Clone`, `Send` e `Sync`,
com `get_usage()`, `get_buffer_size()`, `set_buffer_size()`, `enabled()` e
`set_allow_stall()`. Qual memtable descarregar não é configurável, nem no C++
(seção 8.3).

### 2.3 Threads de flush e compactação

- O pool de threads pertence ao `Env` e é dividido por todos os DBs que usam o
  mesmo `Env`. Flush vai para o pool HIGH e compactação para o LOW, e a wiki pede
  pools separados e não vazios justamente quando vários DBs dividem o mesmo
  `Env` (wiki do pool de threads).
- `max_background_jobs` (padrão 2) é por DB. Com os limites antigos em -1, um
  quarto dos jobs vai para flush e o resto para compactação (análise do blog
  Small Datum, que mediu 8 contra 16 jobs).
- Ao abrir, cada DB só aumenta o pool: chama `IncBackgroundThreadsIfNeeded` com o
  seu limite de compactações no LOW e de flushes no HIGH (`db_impl_open.cc`). O
  pool vai ao maior limite entre os DBs, não à soma, e dez DBs com 4 jobs cada
  disputam as mesmas threads.
- `IncreaseParallelism(n)` põe `max_background_jobs = n`, o LOW em `n` e o HIGH
  em 1, direto no `Env` (`options.cc`). Chamado nas opções de cada DB, mexe no
  pool de todos.
- Pool BOTTOM: a nota da 5.8 diz que compactações universais que incluem o último
  nível podem rodar num pool próprio, o que reduz o bloqueio da fila de
  compactação, em especial com várias instâncias. Não achei na documentação se a
  compactação por nível usa esse pool na versão atual.

No crate: `Env::new()`, cuja doc diz que devolve o `Env` padrão (ele chama
`rocksdb_create_default_env`, que é o `Env::Default()` do C++);
`set_background_threads` (LOW), `set_high_priority_background_threads`,
`set_low_priority_background_threads` e `set_bottom_priority_background_threads`;
`Options::set_env(&env)`, `set_max_background_jobs`, `set_max_subcompactions` e
`increase_parallelism`. Não achei no crate um `Env` com pools próprios (só
`Env::new()` e `Env::mem_env()`), então todos os DBs do processo dividem os
mesmos pools.

### 2.4 Rate limiter, SstFileManager e buckets de lock

- Rate limiter: `Options::set_ratelimiter` e `set_auto_tuned_ratelimiter` recebem
  números, não um objeto, e criam um limitador a cada chamada. Como
  `Options::clone()` chama `rocksdb_options_create_copy`, que copia o
  `shared_ptr` do C++, derivar as opções de todos os DBs de um mesmo `Options`
  base deve compartilhar o limitador. É inferência da semântica de cópia; não
  testei.
- `SstFileManager` (teto de espaço, taxa de remoção de arquivos, soma dos SSTs de
  vários DBs): o crate não tem. O fork tem (módulo `sst_file_manager` e
  `Options::set_sst_file_manager`, desde a 0.44.0).
- Cada `OptimisticTransactionDB` cria `occ_lock_buckets = 1 << 20` buckets de lock
  por padrão (`optimistic_transaction_db.h`). O C++ permite dividir um conjunto
  entre DBs (`shared_lock_buckets`). O crate não expõe essas opções; o fork expõe
  `set_occ_lock_buckets` e `set_shared_lock_buckets`. Não achei o custo de
  memória por bucket.

### 2.5 Como fica no crate

Assinaturas conferidas no docs.rs da 0.25.0:

```rust
use rocksdb::{BlockBasedIndexType, BlockBasedOptions, BlockBasedTablePinningTier,
              Cache, DBCompressionType, Env, Options, SliceTransform, WriteBufferManager};

// Once per process.
let cache = Cache::new_hyper_clock_cache(10 << 30, 0);
let wbm = WriteBufferManager::new_write_buffer_manager_with_cache(1 << 30, false, cache.clone());
let mut env = Env::new()?;                    // the process-wide default Env
env.set_background_threads(6);                // LOW pool: compactions
env.set_high_priority_background_threads(2);  // HIGH pool: flushes

// Per database.
let mut db_opts = Options::default();
db_opts.set_env(&env);
db_opts.set_write_buffer_manager(&wbm);
db_opts.set_max_background_jobs(4);

// Per column family, here `out`.
let mut table = BlockBasedOptions::default();
table.set_block_cache(&cache);
table.set_block_size(16 << 10);
table.set_index_type(BlockBasedIndexType::TwoLevelIndexSearch);
table.set_partition_filters(true);
table.set_metadata_block_size(4096);
table.set_cache_index_and_filter_blocks(true);
table.set_cache_index_and_filter_blocks_with_high_priority(true);
table.set_top_level_index_pinning_tier(BlockBasedTablePinningTier::All);
table.set_partition_pinning_tier(BlockBasedTablePinningTier::FlushAndSimilar);
table.set_bloom_filter(10.0, false);
table.set_whole_key_filtering(false);

let mut out = Options::default();
out.set_block_based_table_factory(&table);
out.set_prefix_extractor(SliceTransform::create_fixed_prefix(8));
out.set_memtable_prefix_bloom_ratio(0.1);
out.set_compression_type(DBCompressionType::Lz4);
out.set_bottommost_compression_type(DBCompressionType::Zstd);
out.set_bottommost_compression_options(-14, 3, 0, 16 << 10, true);
out.set_bottommost_zstd_max_train_bytes(100 * (16 << 10), true);
```

---

## 3. Compressão e prefixo

### 3.1 LZ4 em cima, ZSTD embaixo

- A wiki de ajuste básico recomenda LZ4 (ou Snappy) nos níveis em geral e ZSTD
  (ou Zlib) no último; a wiki de espaço dá a mesma receita e sugere bloco de
  16 KB.
- Com `level_compaction_dynamic_level_bytes = true`, `compression_per_level[0]`
  continua valendo para o L0, mas as outras posições contam a partir do nível
  base, o que recebe o L0, e mudam conforme o grafo cresce
  (`advanced_options.h`). **Recomendação**: `compression_type` e
  `bottommost_compression`, que não dependem do nível base.
- Com compressão diferente por nível, o RocksDB deixa de fazer o "trivial move"
  que violaria a regra de compressão (wiki de compressão).
- Precedente: o TiKV não comprime L0 e L1, usa LZ4 do L2 ao L4 e ZSTD em L5 e L6,
  com bloco de 32 KB, cache compartilhado de 45% da memória e bloom de 10 bits
  (arquivo de configuração do TiKV). Sem compressão em cima, o flush gasta menos
  CPU.

No crate: `set_compression_type`, `set_compression_per_level`,
`set_bottommost_compression_type`, `set_compression_options(window_bits, level,
strategy, max_dict_bytes)`, `set_bottommost_compression_options(..., enabled)` e
`set_compression_options_parallel_threads`. `DBCompressionType` tem `None`,
`Snappy`, `Zlib`, `Bz2`, `Lz4`, `Lz4hc` e `Zstd`; LZ4 e ZSTD são features padrão
do crate.

### 3.2 ZSTD com dicionário

- O dicionário é por SST: treinar no próprio arquivo deu compressão bem melhor
  que treinar num vizinho (post de 2021 do blog do RocksDB).
- Na compactação, os blocos descomprimidos ficam em buffer, até o tamanho alvo do
  arquivo, enquanto o dicionário não é treinado; desde a 6.25 essa memória é
  cobrada no block cache (wiki de dicionário).
- O dicionário vai num meta-block do SST. Com
  `cache_index_and_filter_blocks = true`, vai para o cache com prioridade alta; a
  forma usada na descompressão é maior que a gravada.
- Valores de referência do post: `max_dict_bytes` de 16 KB e
  `zstd_max_train_bytes` de 100 vezes o `max_dict_bytes`. O ganho aparece com
  bloco de dados de até 16 KB.

O `SstFileWriter` usa as opções do último nível: com `bottommost_compression`
definido, pega esse tipo e, se `enabled`, as `bottommost_compression_opts`
(`sst_file_writer.cc`). O SST da carga offline já sai com ZSTD e dicionário. Mas o
`SstFileWriter` passa tamanho alvo 0 ao construtor da tabela, e nesse caso o
limite do buffer do dicionário é `max_dict_buffer_bytes`, cujo padrão 0 desliga o
corte por tamanho (`block_based_table_builder.cc` só corta quando o limite é
diferente de 0). Pela leitura do código, o arquivo inteiro fica descomprimido em
memória até o `finish`. O outro gatilho que vi é a reserva no block cache, e não
conferi quando ela falha. Como o crate não expõe `max_dict_buffer_bytes`, o que
limita essa memória é o tamanho de cada SST gerado e o número de escritores em
paralelo.

No crate: `set_bottommost_compression_options(window_bits, level, strategy,
max_dict_bytes, enabled)` e `set_bottommost_zstd_max_train_bytes(value, enabled)`.
Faltam `max_dict_buffer_bytes` e `use_zstd_dict_trainer`. A busca pelo nome
`rocksdb_options_set_bottommost_compression_options_max_dict_buffer_bytes` acha a
doc do crate `ckb-rocksdb`, o que indica que a função existe no C API; não
conferi no `librocksdb-sys` 0.19.0. Existindo, dá para chamá-la com o ponteiro do
`Options` da feature `raw-ptr`.

### 3.3 Prefix extractor e prefix bloom para chave com id de 8 bytes

Em `out` e `in`, a chave começa pelo id do nó, 8 bytes big-endian:

- `Options::set_prefix_extractor(SliceTransform::create_fixed_prefix(8))`. Toda
  chave tem pelo menos 8 bytes, então todas estão no domínio.
- `BlockBasedOptions::set_whole_key_filtering(false)`: só o prefixo vai para o
  filtro.
- Memtable: `set_memtable_prefix_bloom_ratio(0.1)` cria um bloom de
  `write_buffer_size` × 0,1 dentro da memtable; `set_memtable_whole_key_filtering`
  põe a chave inteira ali também, e só vale com a razão acima de 0
  (`advanced_options.h`).
- Leitura: `ReadOptions::set_prefix_same_as_start(true)` com
  `set_iterate_upper_bound` no prefixo seguinte. `set_total_order_seek(true)`
  ignora o bloom. O `auto_prefix_mode`, que a wiki recomenda desde a 6.8 porque
  devolve sempre o mesmo resultado da ordem total, não está no crate.

Armadilhas, pela wiki de prefix seek:

- trocar o prefix extractor depois deixa SSTs com filtro de outro extractor, então
  o tamanho do prefixo precisa ser decidido antes da carga;
- `SeekToLast()` e iteração reversa não funcionam bem com prefixo (o tempo
  invertido de `event` já contorna isso);
- no modo manual, uso errado não dá erro, dá resultado indefinido;
- a wiki prefere o transform "capped" ao fixo. O crate não tem esse construtor;
  tem `SliceTransform::create(nome, função, in_domain)`, com uma função Rust
  chamada pelo C a cada chave em flush, compactação e seek.

Três famílias do spec não começam por um id de 8 bytes:

- `event`: stream primeiro, depois entidade. Um prefixo de 8 bytes pega o stream e
  parte da entidade. O prefixo precisa ter o tamanho do stream mais 8, ou a
  entidade precisa vir primeiro.
- `pk`: começa pelo `entity_id` com prefixo de tamanho, que é variável. O bloom de
  chave inteira atende o get com label; o seek sem label só teria bloom com um
  transform próprio que leia o tamanho.
- `idx` e `ident`: valor de tamanho variável depois do tipo, mesmo caso da `pk`.

---

## 4. Carga em massa

### 4.1 SstFileWriter e IngestExternalFile

Requisitos (wiki de criação e ingestão):

- chaves em ordem estritamente crescente no comparador (range tombstones podem vir
  fora de ordem);
- comparador idêntico ao do DB; as opções dadas ao `SstFileWriter` definem
  formato e compressão;
- toda chave sai com número de sequência 0 (doc do crate).

A ingestão copia ou liga o arquivo no diretório do DB, bloqueia escritas,
descarrega a memtable se ela se sobrepõe ao arquivo, escolhe o nível mais baixo em
que o arquivo cabe sem se sobrepor a esse nível, aos de cima e a compactações em
andamento, atribui um número de sequência global e libera as escritas. Num
database vazio, com arquivos de faixas disjuntas, tudo vai para o último nível sem
compactação: é o caminho offline do spec. Arquivos que se sobrepõem no mesmo lote
iam sempre para o L0; o `HISTORY.md` registra que hoje a atribuição é feita em
lotes, para que eles possam cair em níveis mais baixos. A FAQ sugere gerar os SSTs
de faixas disjuntas em paralelo.

`ingest_behind` ingere sempre no último nível e pula chave duplicada, mas só se o
DB tiver `allow_ingest_behind = true` desde a criação.

No crate:

- `SstFileWriter::create(&opts)`, `open(path)`, `put`, `merge`, `delete`,
  `delete_range` (novo na 0.25.0), `finish` e `file_size`;
- `ingest_external_file`, `ingest_external_file_opts`, `ingest_external_file_cf` e
  `ingest_external_file_cf_opts`;
- `IngestExternalFileOptions` com `set_move_files`, `set_snapshot_consistency`,
  `set_allow_global_seqno`, `set_allow_blocking_flush` e `set_ingest_behind`;
- `Options::set_allow_ingest_behind`.

Faltam `fail_if_not_bottommost_level`, `link_files`, `write_global_seqno`,
`verify_checksums_before_ingest`, `fill_cache`, `file_opening_threads`, o
`IngestExternalFiles` (várias famílias, tudo ou nada) e o `PrepareFileIngestion`
da 11.5.

**Recomendação**: na carga de um database novo, `set_allow_global_seqno(false)`,
que faz a ingestão falhar se houver sobreposição com o que já existe, e conferir
depois com `property_int_value(properties::num_files_at_level(6))` que tudo foi
para o último nível (7 níveis no padrão), já que falta
`fail_if_not_bottommost_level`. `set_move_files(true)` pede o SST no mesmo sistema
de arquivos do DB.

### 4.2 WAL desligado e flush no fim

- Pela FAQ, escrita com WAL desligado se perde num crash se não tiver ido para
  SST; com várias famílias, o flush consistente pede `atomic_flush = true`.
- A receita da FAQ para inserir rápido: um escritor, chaves em ordem, centenas de
  chaves por WriteBatch, memtable vetor, mais flushes em paralelo e compactação
  automática desligada durante a carga.
- `PrepareForBulkLoad()` faz parte disso, mas também põe `num_levels = 2`,
  `disable_auto_compactions = true`, gatilhos de L0 em `1 << 30`, os limites de
  compactação pendente desligados (0) e `target_file_size_base` de 256 MB,
  contando com uma compactação manual no fim (`options.cc`). Serve a um DB criado só para a carga;
  num database que responde consultas, não.
- `atomic_flush` e `enable_pipelined_write` juntos: o DB recusa abrir
  (`db_impl_open.cc`).

No crate: `WriteOptions::disable_wal(true)` ou `write_without_wal(batch)`;
`flush`, `flush_cf`, `flush_opt` e `flush_cfs_opt(&[...], &FlushOptions)` com
`FlushOptions::set_wait(true)`; `flush_wal(sync)`; `Options::set_atomic_flush`;
`Options::prepare_for_bulk_load`; `set_options(&[("chave", "valor")])` e
`set_options_cf` para mudar gatilhos com o DB aberto e voltar depois;
`wait_for_compact(&WaitForCompactOptions)`.

Custo comparado, conta minha a partir do exemplo do guia de tuning (500 GB,
amplificação de escrita de ~33 vezes): carregar 350 GB pelas memtables, com ou
sem WAL, escreve ~11,6 TB em disco ao longo das compactações. O caminho por SST
escreve cada byte uma vez, fora a ordenação externa. Desligar o WAL tira só a
primeira parcela da soma.

---

## 5. Transações e leitura das próprias escritas

### 5.1 WriteBatchWithIndex

No C++, é um WriteBatch com um índice de busca sobre as chaves inseridas.
`GetFromBatchAndDB` lê o lote e o DB e junta o resultado, com o merge operator se
preciso; `NewIteratorWithBase` põe o lote como delta sobre um iterador do DB. Com
`overwrite_key = true`, o iterador não mostra a mesma chave duas vezes. DeleteRange
não é suportado: as três variantes devolvem erro (`write_batch_with_index.h`). A
10.2.0 acrescentou `IngestWriteBatchWithIndex()`, que grava um WBWI no DB sem
passar pela memtable.

No crate: não existe. Alternativas:

1. `Transaction` (seções 5.2 e 5.3), que lê as próprias escritas.
2. FFI: o C API tem `rocksdb_writebatch_wi_create`,
   `rocksdb_writebatch_wi_get_from_batch_and_db`,
   `rocksdb_writebatch_wi_create_iterator_with_base` e
   `rocksdb_write_writebatch_wi` (`c.h`; no índice do `librocksdb-sys` conferi a
   primeira), e a feature `raw-ptr` entrega o `*mut rocksdb_t`. Snapshot, read
   options e iterador base também teriam de ser criados por FFI, porque o crate
   não dá o ponteiro deles.
3. O fork `rust-rocksdb` 0.53.0 tem `WriteBatchWithIndex` com
   `get_from_batch_and_db`, `get_pinned_from_batch_and_db`, `iterator_with_base`
   e savepoints. O `delete_range` dele grava a faixa, mas a doc avisa que ela não
   é indexada e não aparece nas leituras do lote.
4. Overlay próprio em Rust: mapa ordenado de chave para valor ou tombstone, mais
   as faixas apagadas, combinado com o iterador do snapshot. Como o WBWI não
   indexa faixa, o `DETACH DELETE` dentro de uma transação precisa de algo assim
   de qualquer forma.

### 5.2 OptimisticTransactionDB

- Não trava chave na escrita. No commit, confere se alguém escreveu a chave
  depois da primeira escrita dela na transação, ou depois do snapshot, se houver
  (`SetSnapshot`). `GetForUpdate` torna uma leitura condição do commit; o `Get`
  comum não (wiki de transações).
- A conferência usa o histórico de memtables em memória, e a wiki manda aumentar
  `max_write_buffer_size_to_maintain` se o commit falhar por falta de histórico.
  Em DB de transação, o padrão é `max_write_buffer_number × write_buffer_size`
  por família (`advanced_options.h`): 128 MB por família com os padrões, 1,25 GiB
  para as 10 famílias de um database.
- A validação padrão é `kValidateParallel`, feita antes de entrar no grupo de
  escrita para reduzir disputa de mutex, com `occ_lock_buckets = 1 << 20`.
- DeleteRange, inclusive dentro de WriteBatch passado a `Write()`, é incompatível
  e devolve erro (`optimistic_transaction_db.h`).

No crate: `OptimisticTransactionDB<T>` é um alias de `DBCommon<T,
OptimisticTransactionDBInner>`, cujo `inner()` devolve o DB base. Por isso ele
herda quase tudo do DB comum: `compact_range*`, `ingest_external_file*`,
propriedades, `snapshot()`, `delete_file_in_range*`, `Checkpoint::new` e
`BackupEngine::create_new_backup`. `delete_range_cf` existe desde a 0.23.0 e chama
`rocksdb_delete_range_cf` no DB base, por fora da validação; não achei
documentação sobre como a validação trata um DeleteRange feito assim. Transação:
`transaction()` e `transaction_opt(&WriteOptions, &OptimisticTransactionOptions)`,
com `OptimisticTransactionOptions::set_snapshot`. `commit()` devolve `Busy` no
conflito e `TryAgain` quando falta histórico (doc do `Transaction`). Não há
`OptimisticTransactionDBOptions`; o fork tem, com `set_validate_policy`,
`set_occ_lock_buckets` e `set_shared_lock_buckets`.

### 5.3 TransactionDB

- Pessimista: toda chave escrita é travada, com timeout e detecção de deadlock; o
  commit só falha se o DB não puder escrever (wiki de transações).
- Recusa DeleteRange desde a 6.15; a saída indicada é o DB base (issue 7913).
- Políticas de escrita `WriteCommitted` (padrão), `WritePrepared` e
  `WriteUnprepared`.

No crate: `TransactionDB` é uma struct à parte, com `transaction`,
`transaction_opt`, `prepared_transactions`, get, put, delete, merge, write,
iteradores, `snapshot`, `flush`, `flush_wal`, propriedades e famílias. Não tem
`compact_range`, `ingest_external_file` nem `delete_range`, e o `BackupEngine` só
recebe `&DBCommon`, que ele não é. Checkpoint: `checkpoint::TransactionDBCheckpoint`
(0.25.0). `TransactionDBOptions`: `set_default_lock_timeout`,
`set_txn_lock_timeout`, `set_max_num_locks` e `set_num_stripes`.
`TransactionOptions`: `set_snapshot`, `set_deadlock_detect`, `set_lock_timeout`,
`set_expiration`, `set_deadlock_detect_depth`, `set_max_write_batch_size` e
`set_skip_prepare`. Sem política de escrita; o fork tem `set_write_policy` e
`set_skip_concurrency_control`.

`Transaction`, nos dois tipos: `get*`, `get_for_update*`, `multi_get*`, `put`,
`delete`, `merge`, iteradores que leem as próprias escritas, `set_savepoint`,
`rollback_to_savepoint`, `prepare`, `set_name`, `commit`, `rollback`,
`snapshot()`, `get_writebatch()` (um `WriteBatchWithTransaction<true>`) e
`rebuild_from_writebatch`. Sem `delete_range`; a doc do WriteBatch diz que ele não
é suportado em `Transaction`.

### 5.4 Snapshots

- Um snapshot é um número de sequência numa lista ligada do DB, e flush e
  compactação preservam o que qualquer snapshot enxerga. Com centenas de milhares
  de snapshots, percorrer a lista chega a causar write stall (wiki de snapshot).
- Snapshot não impede a remoção de arquivos, mas a compactação nunca apaga uma
  chave que algum snapshot enxerga (visão geral do RocksDB). Snapshot velho segura
  espaço e atrasa purga. Iterador aberto segura as memtables e os SSTs do momento
  em que foi criado, e a wiki pede iterador de vida curta ou `Refresh()`.

No crate: `snapshot()` devolve `SnapshotWithThreadMode`, que é `Send` e `Sync`, lê
com `get*` e `multi_get*`, cria iteradores e tem `sequence_number()`; é liberado no
`Drop`. Também há `ReadOptions::set_snapshot`, `latest_sequence_number()` e as
propriedades `NUM_SNAPSHOTS` e `OLDEST_SNAPSHOT_TIME`.

### 5.5 Commit em grupo, pipelined write e sync

- O group commit é automático: escritas simultâneas que podem ser combinadas vão
  ao WAL de uma vez, com um fsync só, em grupos de até 1 MB; escritas com opções
  diferentes podem não se juntar, e o RocksDB não atrasa ninguém para encher o
  grupo (wiki de desempenho do WAL).
- `sync = true` faz fsync antes de devolver. `manual_wal_flush = true` segura o WAL
  no buffer do RocksDB até `FlushWAL()`, e nesse modo a escrita não resiste a
  crash.
- Pipelined write separa as filas do WAL e da memtable: +20% no db_bench com
  escritores concorrentes e WAL ligado, ~30% em tmpfs com 8 threads (wiki). O DB
  recusa abrir com ele junto de `unordered_write` ou de `atomic_flush`
  (`db_impl_open.cc`).
- `unordered_write` troca a imutabilidade do snapshot por vazão (+63% com WAL e
  +131% sem, na wiki) e exige `allow_concurrent_memtable_write`.
- `two_write_queues` serve ao commit em duas fases das transações `WritePrepared`
  (wiki de WritePrepared).

No crate: `WriteOptions::set_sync`, `disable_wal`, `set_no_slowdown` e
`set_low_pri`; `Options::set_enable_pipelined_write`, `set_unordered_write`,
`set_manual_wal_flush`, `set_allow_concurrent_memtable_write` e
`set_enable_write_thread_adaptive_yield`; `flush_wal(sync)`. Sem
`two_write_queues`.

Com uma thread escrevendo por database, como no commitador do spec, o group commit
do RocksDB não tem o que juntar: quem junta é o commitador, com um WriteBatch por
grupo e `sync = true`. O pipelined write ajuda quando há vários escritores
concorrentes e ainda bloqueia `atomic_flush`. **Recomendação**: deixar desligado e
medir.

### 5.6 O que muda no commitador do spec

Decisão em jogo: quem detecta conflito. Opções: o `OptimisticTransactionDB` do
RocksDB, ou DB comum com a validação no commitador. Modelo de linguagem tende a
recomendar construir, por isso a opção pronta vem primeiro.

**OptimisticTransactionDB.** Está no crate hoje, sem FFI. Traz snapshot, leitura
das próprias escritas, `get_for_update` para a `pk` ausente que o `MERGE` leu e
uma validação que o próprio RocksDB mantém. `Busy` e `TryAgain` viram o erro
transitório do Neo4j sem código novo. Custos: DeleteRange só pela base, fora da
validação; o histórico de memtables em memória (até 1,25 GiB por database no
padrão), ou mais `TryAgain` se ele for cortado; política de validação e buckets
fixos no crate; e o seguidor do Raft aplicaria o lote por `write`, fora de
transação, com dois caminhos de aplicação.

**DB comum e commitador próprio.** A validação não depende do histórico de
memtables: o commitador guarda, para as chaves confirmadas depois do snapshot mais
velho em uso, o número de sequência do commit (`latest_sequence_number()` depois
de cada `write_opt`) e compara com o `sequence_number()` do snapshot da transação.
O WriteBatch comum aceita `delete_range_cf`, e o mesmo `WriteBatch::data()` vira a
entrada do Raft (`WriteBatch::from_data` no seguidor), com um caminho de aplicação
só. Custos: leitura das próprias escritas e validação passam a ser código do
Brainwire, inclusive o caso da chave lida e ausente.

O que inverte: se o overlay e a validação própria não passarem no gate 1 (30/09),
entra o `OptimisticTransactionDB` com `set_max_write_buffer_size_to_maintain`
explícito, e o `DETACH DELETE` passa a apagar ponto a ponto dentro da transação.
Confiança média: o custo de memória do histórico vem do padrão documentado, não de
medida. Decisão: Allan.

---

## 6. Backup, recuperação e purga

### 6.1 Checkpoint

- SST e blob viram hard links quando o destino está no mesmo sistema de arquivos,
  e cópia quando não; MANIFEST e afins são sempre copiados; o diretório não pode
  existir (`checkpoint.h`).
- `log_size_for_flush` 0 (padrão) força flush sempre. Com outro valor, o
  checkpoint pode sair sem os dados recentes se houve escrita sem WAL, que é o
  caso do `bulk()` (`checkpoint.h`).
- O checkpoint abre como DB somente leitura ou normal; aberto normal, os SSTs
  continuam hard links até ficarem obsoletos (wiki de checkpoints). É o formato
  natural do snapshot do Raft.
- O C++ devolve um número de sequência garantido no checkpoint
  (`sequence_number_ptr`); o crate não. **Recomendação**: gravar o índice aplicado
  do Raft na família `meta`, no mesmo WriteBatch de cada entrada, para o
  checkpoint levar o índice junto.

No crate: `checkpoint::Checkpoint::new(&db)` (aceita `DBCommon`, isto é, DB e
`OptimisticTransactionDB`), `create_checkpoint(path)` (com `log_size_for_flush =
0`), `create_checkpoint_with_log_size(path, n)` e `TransactionDBCheckpoint`.
`ExportColumnFamily` não está; o C API tem `rocksdb_checkpoint_export_column_family`
e o fork tem `ExportImportFilesMetaData`.

### 6.2 BackupEngine incremental

- É incremental no padrão: com `share_table_files = true`, SST já presente no
  diretório de backup não é copiado de novo; com `share_files_with_checksum =
  true` (padrão), o nome do arquivo compartilhado leva tamanho e checksum
  (`backup_engine.h` e wiki de backup).
- Sem `flush_before_backup`, o backup leva os WALs das memtables vivas. Com WAL
  desligado, o header manda usar `flush_before_backup = true` para não perder o
  que está só na memtable.
- O restore confere o checksum de cada arquivo; `PurgeOldBackups(N)` mantém os N
  mais novos; num teste da wiki, 16 threads levaram o backup a um terço do tempo.
- O backup se apoia no mecanismo do checkpoint: suspende a remoção de arquivos,
  lista os vivos, copia e retoma.

No crate: `BackupEngine::open(&BackupEngineOptions, &Env)`,
`create_new_backup(&db)`, `create_new_backup_flush(&db, flush)`,
`purge_old_backups(n)`, `restore_from_latest_backup(db_dir, wal_dir,
&RestoreOptions)`, `restore_from_backup(..., id)`, `verify_backup(id)` e
`get_backup_info()`. `BackupEngineOptions` tem só `new(dir)`,
`set_max_background_operations` e `set_sync` (sync e fsync entraram na 0.24.0). O
padrão do C++ já é incremental; faltam rate limit, `backup_log_files` e os demais
ajustes, que o `librocksdb-sys` tem
(`rocksdb_backup_engine_options_set_share_table_files` e
`rocksdb_backup_engine_options_set_backup_rate_limit`). `create_new_backup` recebe
`&DBCommon`, então não aceita `TransactionDB`.

### 6.3 DeleteRange e DeleteFilesInRange

Pelo `db.h`, DeleteRange remove o intervalo do início (incluído) ao fim (excluído)
e está pronto para produção, com três ressalvas: muitos range tombstones na
memtable pioram a leitura (resolve com flush de vez em quando); limitar
`max_open_files` com range tombstones piora a leitura (usar -1); e é incompatível
com row cache, com erro na chamada. Como funciona (wiki de implementação): o
tombstone de faixa vai para uma memtable própria e, no SST, para um meta-block; é
fragmentado na leitura; a compactação apaga as chaves cobertas, e o próprio
tombstone só some no último nível, quando cada chave tem uma versão só ali e
nenhum snapshot o cobre. No teste do blog de 2018, gets com DeleteRange rodaram a
1,63 milhão por segundo, contra 1,67 milhão com o padrão de varrer e apagar chave a
chave.

`DeleteFilesInRange` apaga só arquivos inteiros dentro da faixa: pode sobrar chave
em arquivo que cruza a borda, os arquivos do L0 ficam, e snapshots anteriores podem
deixar de ver os dados (`convenience.h`). Serve para liberar espaço antes de um
DeleteRange seguido de compactação.

No crate: `delete_range_cf` e `delete_range_cf_opt` no DB;
`WriteBatch::delete_range` e `delete_range_cf`, só no lote sem transação;
`OptimisticTransactionDB::delete_range_cf`, pela base; `SstFileWriter::delete_range`;
`delete_file_in_range` e `delete_file_in_range_cf`. Nada de DeleteRange em
`Transaction` nem em `TransactionDB`. `Options::set_row_cache` existe;
**recomendação**: não usar, por causa do DeleteRange.

### 6.4 CompactRange para purga física

- A compactação manual descarta versões apagadas e sobrescritas, bloqueia até
  terminar, e o intervalo real pode ser maior que o pedido (`db.h`).
- Em compactação por nível, os arquivos da faixa descem até o último nível com
  dados. Só que o padrão, `kIfHaveCompactionFilter`, pula o último nível quando não
  há compaction filter (wiki de compactação manual). Para purga, `kForce` ou
  `kForceOptimized`, que evita recompactar no último nível arquivos criados pela
  mesma compactação.
- `exclusive_manual_compaction` vem `true`: nenhuma outra compactação roda junto.
  Com o banco em uso, `false`.

No crate: `compact_range`, `compact_range_opt`, `compact_range_cf` e
`compact_range_cf_opt`, com `CompactOptions`: `set_exclusive_manual_compaction`,
`set_bottommost_level_compaction(BottommostLevelCompaction::{Skip,
IfHaveCompactionFilter, Force, ForceOptimized})`, `set_change_level`,
`set_target_level` e `set_full_history_ts_low`. Faltam `max_subcompactions` por
chamada, `allow_write_stall` e `canceled`; `Options::set_max_subcompactions` vale
para o DB todo.

### 6.5 Tempo até o dado sumir de verdade

| Onde o dado pode continuar | Até quando | O que controla |
|---|---|---|
| Leitura com snapshot anterior ao delete | o snapshot ser liberado | vida de snapshots e transações |
| Memtable | o flush gravar o tombstone | `write_buffer_size`, WBM |
| WAL | todas as famílias descarregarem além do maior número de sequência do arquivo (wiki do WAL) | `max_total_wal_size` força flush de família parada; WAL arquivado só existe com `WAL_ttl_seconds` ou `WAL_size_limit_MB` acima de 0; com `recycle_log_file_num` acima de 0 (padrão 0), o arquivo é reaproveitado e sobrescrito em vez de apagado |
| SST, inclusive no último nível | uma compactação juntar o tombstone e a versão velha no último nível, sem snapshot no meio | CompactRange forçado (prazo: a duração da chamada); sem ele, ver abaixo |
| SST obsoleto depois da compactação | nenhum iterador segurar o arquivo e a remoção de arquivos estar ligada | iterador curto; `IS_FILE_DELETIONS_ENABLED` |
| Checkpoint | o diretório ser apagado | retenção dos checkpoints (o hard link segura o arquivo) |
| Backup | sair o último backup que usa o SST | `purge_old_backups` |

Sem compactação manual:

- `ttl` (padrão de 30 dias no formato block-based): no nível, arquivos fora do
  último nível com todas as chaves mais velhas que o TTL entram em compactação, em
  cascata até o último nível, e o comentário do `advanced_options.h` diz que o
  objetivo é tirar do disco entradas apagadas ou atualizadas. Pela letra do
  comentário, um arquivo que recebe chave nova não tem todas as chaves velhas; não
  achei garantia de prazo quando a mesma faixa continua recebendo escrita.
- `periodic_compaction_seconds` (padrão 0 no nível sem compaction filter): arquivos
  mais velhos que o valor são reescritos, para o nível seguinte com
  `level_compaction_dynamic_level_bytes`, ou no mesmo lugar se já estão no último.
  A idade vem primeiro de `file_creation_time`, então cada descida espera o prazo
  de novo. Desde a 10.4.0, o RocksDB procura essas compactações a cada 12 horas.

Procedimento para o motor dizer que o dado saiu do disco, montado com o que o
crate expõe (inferência, a testar):

1. gravar os deletes e fazer `flush_cf` nas famílias tocadas;
2. esperar `OLDEST_SNAPSHOT_TIME` passar do instante do delete, ou `NUM_SNAPSHOTS`
   zerar, e os iteradores anteriores fecharem;
3. `compact_range_cf_opt` em cada faixa, com `Force` e
   `exclusive_manual_compaction = false`;
4. conferir `MIN_OBSOLETE_SST_NUMBER_TO_KEEP` e `MIN_LOG_NUMBER_TO_KEEP` contra
   `live_files()` e os WALs do diretório;
5. registrar que checkpoints e backups anteriores guardam o dado até a retenção
   deles vencer.

Apagar o arquivo não zera o bloco no SSD. Se a exigência for apagamento
criptográfico, é outro assunto, que não pesquisei.

---

## 7. O que o crate `rocksdb` expõe

Todas as linhas foram conferidas no docs.rs da 0.25.0 ou no código da tag
`v0.25.0`. A coluna "Versão" diz 0.25.0 e, quando o changelog informa, desde
quando existe.

| Recurso | Expõe | API | Versão | Alternativa |
|---|---|---|---|---|
| Índice particionado | sim | `BlockBasedOptions::set_index_type(BlockBasedIndexType::TwoLevelIndexSearch)`, `set_metadata_block_size` | 0.25.0 | |
| Filtro particionado | sim | `set_partition_filters` | 0.25.0 | |
| Índice e filtro no cache, prioridade alta | sim | `set_cache_index_and_filter_blocks`, `set_cache_index_and_filter_blocks_with_high_priority` | 0.25.0 | |
| Fixar o topo e o L0 | sim | `set_pin_top_level_index_and_filter`, `set_pin_l0_filter_and_index_blocks_in_cache`, `set_top_level_index_pinning_tier`, `set_partition_pinning_tier`, `set_unpartitioned_pinning_tier` | 0.25.0 | |
| Bloom, Ribbon e híbrido | sim | `set_bloom_filter`, `set_ribbon_filter`, `set_hybrid_ribbon_filter` | 0.25.0 | |
| `optimize_filters_for_memory` e `optimize_filters_for_hits` | sim | `BlockBasedOptions::set_optimize_filters_for_memory`, `Options::set_optimize_filters_for_hits` | 0.25.0 | |
| Restart interval do índice, `format_version` | sim | `set_index_block_restart_interval`, `set_format_version` | 0.25.0 | |
| Cobrar table reader, construção de filtro e metadados no cache | não | | 0.25.0 | não conferi no C API; medir com `ESTIMATE_TABLE_READERS_MEM` |
| Medir memória | sim | `property_int_value` com `ESTIMATE_TABLE_READERS_MEM`, `BLOCK_CACHE_USAGE`, `BLOCK_CACHE_PINNED_USAGE`, `CUR_SIZE_ALL_MEM_TABLES`; `properties::aggregated_table_properties_at_level`; `Cache::get_usage` | 0.25.0 | |
| Cache LRU compartilhado | parcial | `Cache::new_lru_cache`, `new_lru_cache_opts` (só capacidade e shards) | 0.25.0 (opções de LRU desde 0.23.0) | `strict_capacity_limit` por FFI (`rocksdb_cache_create_lru_with_strict_capacity_limit`); `high_pri_pool_ratio` fica no padrão 0,5 |
| HyperClockCache | sim | `Cache::new_hyper_clock_cache(capacity, estimated_entry_charge)` | 0.25.0 | fork: `HyperClockCacheOptions` |
| WriteBufferManager compartilhado e cobrado no cache | sim | `WriteBufferManager::new_write_buffer_manager_with_cache`, `Options::set_write_buffer_manager` | 0.25.0 | |
| Escolher qual memtable descarregar entre DBs | não existe no RocksDB | | | PR 15047, aberto |
| Pools de flush e compactação compartilhados | sim | `Env::new`, `set_background_threads`, `set_high_priority_background_threads`, `set_bottom_priority_background_threads`, `Options::set_env`, `set_max_background_jobs` | 0.25.0 | |
| `Env` com pools próprios | não achei | só `Env::new()` e `Env::mem_env()` | 0.25.0 | |
| Rate limiter compartilhado | parcial | `set_ratelimiter`, `set_auto_tuned_ratelimiter` (um limitador por chamada) | 0.25.0 | clonar um `Options` base (inferência) |
| SstFileManager | não | | 0.25.0 | fork: `Options::set_sst_file_manager` |
| Opções do OptimisticTransactionDB (validação, buckets) | não | | 0.25.0 | fork: `OptimisticTransactionDBOptions` |
| Compressão por nível e no último nível | sim | `set_compression_type`, `set_compression_per_level`, `set_bottommost_compression_type` | 0.25.0 | |
| Dicionário ZSTD | parcial | `set_bottommost_compression_options(.., max_dict_bytes, enabled)`, `set_bottommost_zstd_max_train_bytes` | 0.25.0 | `max_dict_buffer_bytes` por FFI com `raw-ptr`; `use_zstd_dict_trainer` não conferi no C API |
| Compressão paralela | sim | `set_compression_options_parallel_threads` | 0.25.0 | |
| Prefix extractor fixo | sim | `SliceTransform::create_fixed_prefix` | 0.25.0 | |
| Prefix extractor "capped" | não | | 0.25.0 | `SliceTransform::create` com função Rust |
| Bloom na memtable | sim | `set_memtable_prefix_bloom_ratio`, `set_memtable_whole_key_filtering` | 0.25.0 | |
| Filtro só de prefixo | sim | `BlockBasedOptions::set_whole_key_filtering(false)` | 0.25.0 | |
| Leitura por prefixo | parcial | `ReadOptions::set_prefix_same_as_start`, `set_total_order_seek`, `set_iterate_upper_bound`, `set_iterate_lower_bound` | 0.25.0 | `auto_prefix_mode` não exposto; não conferi no C API |
| SstFileWriter | sim | `SstFileWriter::create`, `open`, `put`, `merge`, `delete`, `delete_range`, `finish` | 0.25.0 (`delete_range` desde 0.25.0) | |
| IngestExternalFile | parcial | `ingest_external_file*`; `IngestExternalFileOptions::set_move_files`, `set_snapshot_consistency`, `set_allow_global_seqno`, `set_allow_blocking_flush`, `set_ingest_behind` | 0.25.0 | sem `fail_if_not_bottommost_level` (conferir com `num_files_at_level`), `link_files`, `IngestExternalFiles` e `PrepareFileIngestion`; não conferi o fork |
| Ingestão atrás | sim | `set_ingest_behind`, `Options::set_allow_ingest_behind` | 0.25.0 | |
| WAL desligado | sim | `WriteOptions::disable_wal`, `write_without_wal` | 0.25.0 | |
| Flush no fim, atômico | sim | `flush*`, `flush_cfs_opt`, `FlushOptions::set_wait`, `Options::set_atomic_flush`, `flush_wal` | 0.25.0 | |
| PrepareForBulkLoad | sim | `Options::prepare_for_bulk_load` | 0.25.0 | cuidado com `num_levels = 2` |
| Mudar opção com o DB aberto | sim | `set_options`, `set_options_cf` | 0.25.0 | |
| WriteBatchWithIndex | não | | 0.25.0 | `Transaction`; FFI (`rocksdb_writebatch_wi_*` e `raw-ptr`); fork; overlay próprio |
| OptimisticTransactionDB | sim | `OptimisticTransactionDB`, `transaction_opt`, `OptimisticTransactionOptions::set_snapshot` | 0.25.0 | |
| TransactionDB | sim | `TransactionDB`, `TransactionDBOptions`, `TransactionOptions`, `prepared_transactions` | 0.25.0 | |
| Política de escrita `WritePrepared` e `WriteUnprepared` | não | | 0.25.0 | fork: `TransactionDBOptions::set_write_policy` |
| Ler as próprias escritas | sim, na transação | `Transaction::get*`, `get_for_update*`, iteradores | 0.25.0 | |
| DeleteRange na transação | não (nem o C++ aceita) | | 0.25.0 | DB base |
| Snapshot | sim | `snapshot()`, `SnapshotWithThreadMode::sequence_number`, `ReadOptions::set_snapshot`, `Transaction::snapshot` | 0.25.0 | |
| WriteBatch serializado para o Raft | sim | `WriteBatch::data`, `from_data`, `iterate` | 0.25.0 | |
| Group commit | automático | nenhuma | | |
| Pipelined write e unordered write | sim | `set_enable_pipelined_write`, `set_unordered_write` | 0.25.0 | |
| `two_write_queues` | não | | 0.25.0 | não conferi no C API |
| Sync e WAL manual | sim | `WriteOptions::set_sync`, `Options::set_manual_wal_flush`, `flush_wal` | 0.25.0 | |
| Checkpoint | sim | `Checkpoint::new`, `create_checkpoint`, `create_checkpoint_with_log_size`, `TransactionDBCheckpoint` | 0.25.0 (checkpoint de `TransactionDB` desde 0.25.0) | |
| Exportar família | não | | 0.25.0 | C API `rocksdb_checkpoint_export_column_family`; fork |
| BackupEngine | parcial | `BackupEngine::open`, `create_new_backup`, `create_new_backup_flush`, `purge_old_backups`, `restore_from_latest_backup`, `restore_from_backup`, `verify_backup`, `get_backup_info`; `BackupEngineOptions::set_max_background_operations`, `set_sync` | 0.25.0 (`set_sync` desde 0.24.0) | rate limit e `share_table_files` por FFI |
| DeleteRange | sim | `delete_range_cf`, `delete_range_cf_opt`, `WriteBatch::delete_range*`, `OptimisticTransactionDB::delete_range_cf`, `SstFileWriter::delete_range` | 0.25.0 (no OCC desde 0.23.0) | |
| DeleteFilesInRange | sim | `delete_file_in_range`, `delete_file_in_range_cf` | 0.25.0 | |
| CompactRange forçado | sim | `compact_range_cf_opt`, `CompactOptions::set_bottommost_level_compaction(Force)`, `set_exclusive_manual_compaction` | 0.25.0 | sem `max_subcompactions` por chamada |
| TTL e compactação periódica | sim | `set_ttl`, `set_periodic_compaction_seconds` | 0.25.0 | |
| Suspender remoção de arquivos, listar arquivos | sim | `disable_file_deletions`, `enable_file_deletions` (só no DB), `live_files` | 0.25.0 | |
| Esperar compactação | sim | `wait_for_compact` | 0.25.0 | |
| `max_open_files` e threads de abertura | sim | `set_max_open_files`, `set_max_file_opening_threads`, `set_skip_stats_update_on_db_open` | 0.25.0 | |
| `skip_checking_sst_file_sizes_on_db_open` | não | deprecado no RocksDB 10.5 e no crate 0.24.0 | 0.25.0 | a checagem agora roda em paralelo |
| `fast_sst_open`, `reuse_manifest_on_open` | não | | 0.25.0 | não conferi no C API |
| Ponteiro C para chamar o C API | sim, com a feature `raw-ptr` | `AsRawPtr` para `DB`, `Options` e `Env` | 0.25.0 | |

Decisão que a tabela levanta: ficar no `rocksdb` e completar por FFI, ou trocar
pelo fork. A favor do `rocksdb`: é o crate do plano, e o que falta para o desenho
recomendado (overlay próprio, DB comum) é pouco e cabe em FFI pontual. A favor do
fork: `WriteBatchWithIndex`, `SstFileManager`, opções do OCC, política de escrita
e mais tipos já prontos, com lançamentos mensais sobre a mesma 11.8.1. Não
verifiquei quem mantém o fork nem a política de compatibilidade dele. O que
inverte: se o Brainwire precisar de três ou mais itens do fork, a troca custa
menos que o FFI. Decisão: Allan.

---

## 8. Riscos em escala e ajustes

### 8.1 Amplificação de escrita

A compactação por nível reescreve cada byte várias vezes: no exemplo do guia de
tuning (500 GB, L1 de 512 MB, multiplicador 10), ~33 vezes, e a wiki de
compactação por nível diz que costuma passar de 10. No caminho online do
Brainwire, o teto de ingestão é de ~10 mil elementos por segundo (README); com 2
chaves de ~50 bytes por elemento (hipótese minha), são ~1 MB/s lógicos e ~33 MB/s
de compactação, folgado para NVMe. Na carga de 5 bilhões pelas memtables, são
~11,6 TB escritos (seção 4.2).

Ajustes: carga grande por SST (4.1); `write_buffer_size` maior e
`min_write_buffer_number_to_merge` reduzem a amplificação (wiki de write stalls),
mas pesam no WBM (8.3); `set_max_subcompactions` paraleliza o L0 para L1, que não
é paralelo por padrão (wiki de compactação por nível); `set_auto_tuned_ratelimiter`
suaviza os picos. A compactação em camadas reduz a amplificação de escrita ao custo
de espaço e leitura (wiki de compactação), e a por nível economiza mais espaço
(wiki de espaço). Com a meta de disco do README, a por nível fica.

### 8.2 Stalls de L0 e de compactação pendente

Padrões: escrita desacelera com 20 arquivos no L0 e para com 36; desacelera com
64 GB de compactação pendente e para com 256 GB; e para quando as memtables
imutáveis chegam a `max_write_buffer_number` (2) (`advanced_options.h` e wiki de
write stalls). Num database de 350 GB, uma rajada passa dos 64 GB pendentes com
facilidade. Com o pool dividido, o atraso de um database tira thread dos outros.

Ajustes da wiki: mais jobs em segundo plano, memtables maiores, pool de flush
separado. Durante carga, subir os gatilhos com `set_options` e voltar depois.
`WriteOptions::set_no_slowdown(true)` faz a escrita falhar com `Incomplete` em vez
de esperar. Monitorar `IS_WRITE_STOPPED`, `ACTUAL_DELAYED_WRITE_RATE`,
`ESTIMATE_PENDING_COMPACTION_BYTES`, `COMPACTION_PENDING` e
`NUM_RUNNING_COMPACTIONS` por database.

### 8.3 WriteBufferManager compartilhado

- PR 15047 do RocksDB (aberto, de agosto a setembro de 2026): o WBM compartilhado
  dispara o flush no DB cuja escrita percebe a pressão e escolhe a memtable mutável
  mais antiga desse DB. Com muitas famílias ou DBs, ele descarrega memtables
  pequenas várias vezes e não toca quem ocupa mais. O PR propõe descarregar a maior
  do DB, ou a maior entre todos os DBs.
- Harper (PR 2492): em produção, dois nós paravam todo dia. Com o orçamento
  espalhado por muitas famílias, nenhuma chegava ao gatilho, o contador de flush
  por motivo do WBM ficava em 0 e todas as threads de escrita esperavam; a saída
  era reiniciar. Eles mudaram o padrão de `allow_stall` para `false`.
- Camunda (issue 51669): WBM e cache dimensionados na partida; quando o número de
  instâncias cresceu, `min_write_buffer_number_to_merge = 3` impedia o flush, e
  baixaram para 1.

Ajustes: `allow_stall = false` e alerta em `WriteBufferManager::get_usage()` acima
do orçamento; `min_write_buffer_number_to_merge = 1` (padrão);
`write_buffer_size` pequeno nas famílias pequenas (`meta`, `acl`, `lbl`);
`max_total_wal_size` para forçar o flush de família parada; recalcular o WBM com
`set_buffer_size` quando um database for criado ou apagado.

### 8.4 Abertura com muitos SSTs

Com 175 a 350 GB por grafo, são 2.700 a 5.500 SSTs com arquivos de 64 MB (padrão
de `target_file_size_base`) e 700 a 1.400 com 256 MB, vezes o número de
databases. A wiki de abertura rápida lista as causas de lentidão: replay do
MANIFEST, replay do WAL, leitura do rodapé e dos metadados de todos os SSTs com
`max_open_files = -1` (leituras aleatórias), checagem do tamanho de todos os SSTs
e abertura de um DB por vez.

Ajustes: `set_max_file_opening_threads` (padrão 16),
`set_skip_stats_update_on_db_open(true)`, `set_max_manifest_file_size`,
`set_max_total_wal_size`, `target_file_size_base` maior e os databases abertos em
paralelo. No RocksDB, a 10.5 passou a checar tamanhos em paralelo; a 11.2
trouxe `fast_sst_open` (experimental) e a 11.3 `reuse_manifest_on_open`, que o
crate não expõe. Não achei medida pública de tempo de abertura com milhares de
SSTs.

### 8.5 `max_open_files` e descritores

Com -1, todos os arquivos ficam abertos (o guia de tuning recomenda, para evitar o
custo do table cache, e o `db.h` pede o mesmo por causa do DeleteRange). Cada SST
segura um descritor, então o `ulimit -n` do container precisa cobrir a soma de
SSTs, WALs, MANIFESTs e LOGs de todos os databases, mais sockets. O TiKV usa 40.960
como padrão. A memória dos table readers fica fora do cache (1.4). Limitar
`max_open_files` troca memória por reabertura e piora leitura com range
tombstones.

### 8.6 Snapshots e iteradores longos

Transação ou consulta longa segura versões (espaço e purga) e SSTs; centenas de
milhares de snapshots causam stall; iterador segura blocos e arquivos. Ajustes: o
teto de execução por consulta do spec, snapshot liberado no fim de cada
transação, iterador recriado em varredura longa. A wiki sugere `Refresh()`; não
conferi se o crate expõe.

### 8.7 Histórico e buckets do OptimisticTransactionDB

Se ele for escolhido, `set_max_write_buffer_size_to_maintain` precisa de valor
explícito: no padrão, até 1,25 GiB por database com 10 famílias, acima do
orçamento de memtables do spec. No caso do Harper, a mesma retenção chegava a
~256 MiB por família. Os 2^20 buckets de lock por database não mudam pelo crate.

### 8.8 Build e plataforma

O RocksDB exige compilador C++20 (GCC 11 ou mais, Clang 10 ou mais) desde a
10.7.0, inclusive para quem usa os headers, então a imagem que compila o
`librocksdb-sys` 0.19 precisa disso. O crate pede Rust 1.88.0 (README). O HCC
automático depende de mmap anônimo. O crate tem as features `jemalloc` e
`io-uring`.

### 8.9 O que não achei

- tamanho de memória por table reader aberto;
- se a compactação por nível usa o pool BOTTOM na versão atual;
- como a validação do `OptimisticTransactionDB` trata DeleteRange feito na base;
- prazo garantido do `ttl` quando a faixa continua recebendo escrita;
- tempo de abertura medido com milhares de SSTs;
- custo de memória por bucket de lock do OCC;
- quando falha a reserva de cache do buffer de dicionário;
- se o C API tem `two_write_queues`, `auto_prefix_mode`,
  `fail_if_not_bottommost_level`, `use_zstd_dict_trainer`, `fast_sst_open` e
  `cache_usage_options` (não conferi);
- se o crate expõe `Refresh()` de iterador.

---

## 9. Fontes

Wiki do RocksDB:

- https://github.com/facebook/rocksdb/wiki/Partitioned-Index-Filters
- https://github.com/facebook/rocksdb/wiki/RocksDB-Bloom-Filter
- https://github.com/facebook/rocksdb/wiki/Block-Cache
- https://github.com/facebook/rocksdb/wiki/Memory-usage-in-RocksDB
- https://github.com/facebook/rocksdb/wiki/Write-Buffer-Manager
- https://github.com/facebook/rocksdb/wiki/Thread-Pool
- https://github.com/facebook/rocksdb/wiki/Compression
- https://github.com/facebook/rocksdb/wiki/Dictionary-Compression
- https://github.com/facebook/rocksdb/wiki/Space-Tuning
- https://github.com/facebook/rocksdb/wiki/Prefix-Seek
- https://github.com/facebook/rocksdb/wiki/Index-Block-Format
- https://github.com/facebook/rocksdb/wiki/Creating-and-Ingesting-SST-files
- https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ
- https://github.com/facebook/rocksdb/wiki/Transactions
- https://github.com/facebook/rocksdb/wiki/WritePrepared-Transactions
- https://github.com/facebook/rocksdb/wiki/Snapshot
- https://github.com/facebook/rocksdb/wiki/Iterator
- https://github.com/facebook/rocksdb/wiki/Pipelined-Write
- https://github.com/facebook/rocksdb/wiki/unordered_write
- https://github.com/facebook/rocksdb/wiki/WAL-Performance
- https://github.com/facebook/rocksdb/wiki/Write-Ahead-Log-(WAL)
- https://github.com/facebook/rocksdb/wiki/Checkpoints
- https://github.com/facebook/rocksdb/wiki/How-to-backup-RocksDB
- https://github.com/facebook/rocksdb/wiki/DeleteRange
- https://github.com/facebook/rocksdb/wiki/DeleteRange-Implementation
- https://github.com/facebook/rocksdb/wiki/Manual-Compaction
- https://github.com/facebook/rocksdb/wiki/Leveled-Compaction
- https://github.com/facebook/rocksdb/wiki/Compaction
- https://github.com/facebook/rocksdb/wiki/Write-Stalls
- https://github.com/facebook/rocksdb/wiki/Speed-Up-DB-Open
- https://github.com/facebook/rocksdb/wiki/RocksDB-Tuning-Guide
- https://github.com/facebook/rocksdb/wiki/Setup-Options-and-Basic-Tuning
- https://github.com/facebook/rocksdb/wiki/RocksDB-Overview

Headers e código do RocksDB (branch `main`):

- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/table.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/cache.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/advanced_options.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/options.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/db.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/convenience.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/filter_policy.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/write_buffer_manager.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/c.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/utilities/write_batch_with_index.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/utilities/optimistic_transaction_db.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/utilities/checkpoint.h
- https://github.com/facebook/rocksdb/blob/main/include/rocksdb/utilities/backup_engine.h
- https://github.com/facebook/rocksdb/blob/main/options/options.cc
- https://github.com/facebook/rocksdb/blob/main/db/db_impl/db_impl_open.cc
- https://github.com/facebook/rocksdb/blob/main/table/sst_file_writer.cc
- https://github.com/facebook/rocksdb/blob/main/table/block_based/block_based_table_builder.cc
- https://github.com/facebook/rocksdb/blob/main/table/block_based/filter_policy.cc
- https://github.com/facebook/rocksdb/blob/main/table/block_based/full_filter_block.cc
- https://github.com/facebook/rocksdb/blob/main/table/block_based/partitioned_filter_block.cc
- https://github.com/facebook/rocksdb/blob/main/HISTORY.md

Blog, notas de versão, issues e PRs do RocksDB:

- https://rocksdb.org/blog/2021/05/31/dictionary-compression.html
- https://rocksdb.org/blog/2018/11/21/delete-range.html
- https://rocksdb.org/blog/2015/02/27/write-batch-with-index.html
- https://rocksdb.org/blog/2017/09/28/rocksdb-5-8-released.html
- https://rocksdb.org/blog/2025/09/24/unified-memory-tracking.html
- https://github.com/facebook/rocksdb/releases/tag/v10.7.5
- https://github.com/facebook/rocksdb/issues/7913
- https://github.com/facebook/rocksdb/issues/11097
- https://github.com/facebook/rocksdb/issues/12942
- https://github.com/facebook/rocksdb/pull/15047

Crate `rocksdb` 0.25.0:

- https://crates.io/api/v1/crates/rocksdb
- https://crates.io/api/v1/crates/librocksdb-sys
- https://docs.rs/rocksdb/0.25.0/rocksdb/
- https://docs.rs/crate/rocksdb/0.25.0/features
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.Options.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.BlockBasedOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/enum.BlockBasedTablePinningTier.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.Cache.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.LruCacheOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.WriteBufferManager.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.Env.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.ReadOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.WriteOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.FlushOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.SliceTransform.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/enum.DBCompressionType.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.SstFileWriter.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.IngestExternalFileOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.CompactOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/enum.BottommostLevelCompaction.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.DBCommon.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.WriteBatchWithTransaction.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.SnapshotWithThreadMode.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.Transaction.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.TransactionDB.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/type.OptimisticTransactionDB.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.TransactionOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.TransactionDBOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/struct.OptimisticTransactionOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/backup/index.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/backup/struct.BackupEngine.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/backup/struct.BackupEngineOptions.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/checkpoint/index.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/checkpoint/struct.Checkpoint.html
- https://docs.rs/rocksdb/0.25.0/rocksdb/properties/index.html
- https://docs.rs/librocksdb-sys/0.19.0+11.8.1/librocksdb_sys/index.html
- https://github.com/rust-rocksdb/rust-rocksdb/blob/master/CHANGELOG.md
- https://github.com/rust-rocksdb/rust-rocksdb/blob/v0.25.0/Cargo.toml
- https://github.com/rust-rocksdb/rust-rocksdb/blob/v0.25.0/README.md
- https://github.com/rust-rocksdb/rust-rocksdb/blob/v0.25.0/src/db_options.rs
- https://github.com/rust-rocksdb/rust-rocksdb/blob/v0.25.0/src/db.rs
- https://github.com/rust-rocksdb/rust-rocksdb/blob/v0.25.0/src/lib.rs
- https://github.com/rust-rocksdb/rust-rocksdb/blob/v0.25.0/src/raw_ptr.rs
- https://github.com/rust-rocksdb/rust-rocksdb/blob/v0.25.0/src/transactions/optimistic_transaction_db.rs

Fork `rust-rocksdb` 0.53.0:

- https://crates.io/api/v1/crates/rust-rocksdb
- https://crates.io/api/v1/crates/rust-librocksdb-sys
- https://docs.rs/rust-rocksdb/0.53.0/rust_rocksdb/
- https://docs.rs/rust-rocksdb/0.53.0/rust_rocksdb/struct.WriteBatchWithIndex.html
- https://docs.rs/rust-rocksdb/0.53.0/rust_rocksdb/struct.TransactionDBOptions.html
- https://docs.rs/rust-rocksdb/0.53.0/rust_rocksdb/struct.OptimisticTransactionDBOptions.html
- https://docs.rs/rust-rocksdb/0.53.0/rust_rocksdb/struct.LruCacheOptions.html
- https://docs.rs/rust-rocksdb/0.53.0/rust_rocksdb/struct.HyperClockCacheOptions.html
- https://docs.rs/rust-rocksdb/0.53.0/rust_rocksdb/sst_file_manager/index.html
- https://github.com/zaidoon1/rust-rocksdb/blob/master/CHANGELOG.md

Outras fontes técnicas públicas:

- https://docs.confluent.io/platform/current/streams/developer-guide/memory-mgmt.html
- https://docs.pingcap.com/tidb/stable/tikv-configuration-file/
- https://github.com/tikv/tikv/issues/6486
- http://smalldatum.blogspot.com/2022/07/the-impact-of-maxbackgroundjobs-on.html
- https://github.com/HarperFast/harper/pull/2492
- https://github.com/camunda/camunda/issues/51669
- https://docs.rs/ckb-rocksdb/latest/ckb_rocksdb/struct.Options.html (só como
  resultado de busca, para o nome da função de `max_dict_buffer_bytes`)

Neste repositório:

- `client-brainwire/README.md` (volume, meta de disco, teto de ingestão)
- `client-brainwire/PLANO.md`
- `client-brainwire/docs/superpowers/specs/2026-09-24-brainwire-design.md`
  (seção 1: famílias, chaves, memória, transações, carga e purga)

---

Proveniência: rascunho apoiado por IA (Claude Opus 5.5, 24/09/2026). Protocolo
anti-trendslop aplicado nas decisões das seções 5.6 e 7. Tensões identificadas:
validação pronta do RocksDB contra commitador próprio; crate `rocksdb` com FFI
contra o fork `rust-rocksdb`. Decisão final: Allan.
