# RocksDB in Brainwire: memory, shared resources, loading and purging

Research from 2026-09-24 for section 1 (storage) of the Brainwire spec:
a graph engine in Rust on top of RocksDB, 5 billion elements per graph, a process
limited to 16 GB of RAM, data on NVMe and one RocksDB per database in the same
instance.

**How it was done.** Everything was read over the web, with no clone and no download: the
RocksDB wiki, headers and some code files from the `main` branch of
`facebook/rocksdb` (through GitHub raw), the `HISTORY.md`, posts from the official blog,
issues and PRs, the crate documentation on docs.rs, the crate code at tag
`v0.25.0` and the crates.io API. The headers are the ones from `main`, which on 2026-09-24 was
at 11.11.0; the crate bundles 11.8.1, so some comment or default may
differ between the two. The text of the sources is paraphrased in English, with the
source next to it. Where the source did not confirm something, it says "not found". What
is marked as **recommendation** is my inference. A calculation with no source next to it is
mine, with the assumptions written out.

**Versions.** The `rocksdb` crate (repository `rust-rocksdb/rust-rocksdb`) is at
0.25.0, from 2026-08-16, on top of `librocksdb-sys` 0.19.0+11.8.1, that is, RocksDB
11.8.1. The previous version, 0.24.0, came out on 2025-08-10. There is a fork published
under another name, the `rust-rocksdb` crate (repository `zaidoon1/rust-rocksdb`), at
0.53.0 from 2026-09-05, also on top of 11.8.1 (`rust-librocksdb-sys`
0.48.1+11.8.1), with ten versions between January and September 2026. In this text,
"the crate" is `rocksdb` 0.25.0; the fork appears as an alternative when the crate
does not expose something.

---

## Summary

Recommended configuration, all of it to be measured at the 5 billion gate:

1. A single `Cache::new_hyper_clock_cache(capacity, 0)` in the process, passed to
   all families of all databases. Since the memtables' cost is charged
   to it, the capacity includes the memtables: for ~9 GB of blocks and ~1 GB of
   memtables, the cache is 10 GB. Table 1.5 of the spec adds the two up separately.
2. A single 1 GB `WriteBufferManager`, with its cost charged to the cache and
   `allow_stall = false`, because of reported hangs with many families
   and databases on the same manager (section 8.3).
3. Partitioned index and filter in the large families, with the top level pinned and the
   partitions in the cache, and a 16 KB data block. What needs to stay resident
   drops to tens of MB per graph (section 1.3).
4. Filter chosen per family. A 10-bit Bloom on all 10 billion
   keys would cost 12.5 GB. With an 8-byte prefix on `out` and `in`,
   `optimize_filters_for_hits` where lookups almost always hit, and a whole-key
   filter only on `pk`, the total comes to between 2.3 and 5.4 GB, which stay in the cache and do not
   need to be resident.
5. LZ4 in `compression_type` and ZSTD in `bottommost_compression`, with
   `max_dict_bytes` of 16 KB and `zstd_max_train_bytes` of 100 times that. Avoid
   `compression_per_level`, whose positions start counting from the base
   level with `level_compaction_dynamic_level_bytes`.
6. Loading 5 billion through the offline path: SSTs written in order, disjoint
   ranges per family, ingested into an empty database. The files go straight
   to the last level and do not pay the write amplification of compaction
   (33 times in the tuning guide's example).
7. No `prepare_for_bulk_load()` in the online `bulk()`: it reduces the DB to 2
   levels and turns off automatic compaction.
8. For the spec's committer, a plain DB with its own validation. The
   `OptimisticTransactionDB` is ready in the crate, but it rejects DeleteRange on the
   transaction path and holds memtable history in memory (section 5.6).
9. Physical purge with `compact_range_cf_opt` and `BottommostLevelCompaction::Force`
   on the affected range, with `exclusive_manual_compaction = false`. The engine only
   declares the data deleted after checking snapshots, obsolete files, WAL,
   checkpoints and backups (section 6.5).
10. `max_open_files = -1` (default) and the container's `ulimit -n` above the total number of
    SSTs across all databases.

What the crate does not expose and matters here: `WriteBatchWithIndex`; the options of
`OptimisticTransactionDB` (validation policy and lock buckets) and the write
policy of `TransactionDB`; `SstFileManager`; `strict_capacity_limit` and
`high_pri_pool_ratio` of the LRU; `cache_usage_options`;
`fail_if_not_bottommost_level`, `link_files` and `PrepareFileIngestion` in
ingestion; `max_dict_buffer_bytes` and `use_zstd_dict_trainer`; settings of
`BackupEngineOptions` beyond threads and sync; `two_write_queues`;
`auto_prefix_mode`. The fork covers a good part of it (section 7). With the `raw-ptr` feature, the
crate hands out the C pointer of the DB, of `Options` and of `Env` (trait `AsRawPtr`), and
`librocksdb-sys` ships the C API, so several of these items can be reached through FFI, with
`unsafe`.

---

## 1. Memory with billions of keys

### 1.1 Partitioned index and filter

Without partitioning, each SST has one whole index block and one whole filter block. The partitioned
index wiki gives the order of magnitude: in a 256 MB SST, it is common for the index
to be 0.5 MB and the filter 5 MB, against data blocks of 4 to 32 KB. With partitioning, the
index and the filter become small blocks (the partitions) with a top-level index;
only the top level needs to be in memory, and the partitions enter the block cache when
someone needs them.

| C++ option | In the crate | Effect |
|---|---|---|
| `index_type = kTwoLevelIndexSearch` | `BlockBasedOptions::set_index_type(BlockBasedIndexType::TwoLevelIndexSearch)` | index in partitions; they use the block cache even with `cache_index_and_filter_blocks = false` (`table.h`) |
| `partition_filters = true` | `set_partition_filters(true)` | filter in partitions; requires a "full" filter, not the old per-block one |
| `metadata_block_size = 4096` | `set_metadata_block_size(4096)` | target size of each partition |
| `cache_index_and_filter_blocks = true` | `set_cache_index_and_filter_blocks(true)` | the top level also goes into the cache and counts toward its capacity |
| `cache_index_and_filter_blocks_with_high_priority` (default `true`) | `set_cache_index_and_filter_blocks_with_high_priority(true)` | index, filter and dictionary enter with high priority |
| `pin_top_level_index_and_filter` (default `true`) | `set_pin_top_level_index_and_filter(true)` | the table reader holds the top level, which only leaves the cache when the file closes |
| `metadata_cache_options` | `set_top_level_index_pinning_tier`, `set_partition_pinning_tier`, `set_unpartitioned_pinning_tier`, with `BlockBasedTablePinningTier::{Fallback, None, FlushAndSimilar, All}` | pinning per block type and per file tier |
| `decouple_partitioned_filters` | not exposed | default `true` since 10.6.0, already marked as deprecated |

The two `pin_*` are marked as deprecated in the current `table.h` in favor of
`metadata_cache_options`, whose default (`kFallback`) repeats the old
behavior. The crate exposes both forms.

Costs, according to the wiki: the top level takes 0.1% to 1% of the index and filter; if it is not
in the cache, the read does one extra I/O; frequent random reads in the
same SST load a different partition each time. The wiki also warns that
pinned blocks can exceed the cache capacity when `strict_capacity_limit`
is off, which is the default. In the Linkbench test on a 300 GB SSD, reducing
memory from 6 to 2 GB dropped throughput from 38 thousand to 30 thousand with partitioning and
to 23 thousand without.

### 1.2 Bloom, Ribbon and cost per key

Bloom, according to the filter wiki:

| Bits per key | False positive |
|---|---|
| 1.5 | 50% |
| 2.9 | 25% |
| 4.9 | 10% |
| 9.9 | 1% |
| 15.5 | 0.1% |

The size is linear: with 10 bits, the filter takes number of keys × 10 bits (memory
usage wiki). Bloom from `format_version = 5` onward uses a 64-bit hash
and, according to the wiki, scales to many billions of keys in a single filter; the old
format did not go below ~0.1% false positives even with 100 bits per key.

Ribbon takes up about 30% less than Bloom for the same rate, at the cost of 3 to
4 times more CPU during construction: `NewRibbonFilterPolicy(9.9)` gives 1% with ~7 bits per
key. `filter_policy.h` warns that building a very large filter uses temporary
memory that nobody accounts for: ~3 GB for 100 million keys in a single
filter, against 1 GB for Bloom. With a partitioned filter, each partition is finalized
when it is cut (`CutAFilterBlock` calls `Finish`, in
`partitioned_filter_block.cc`), so the peak is per partition, not per file.
The hybrid mode (`bloom_before_level`) uses Bloom in the upper levels and Ribbon
below; the default value 0 produces Bloom only on flushes.

Three options change the math:

- `optimize_filters_for_memory` (default `true` since 9.2.0) adjusts the size of
  each filter to reduce allocator fragmentation, keeping the aggregate false
  positive rate on target.
- `optimize_filters_for_hits` (family option, default `false`) does not write a filter in the
  last level, which holds most of the data. For a lookup that hits, that filter
  does not help; the ones in the other levels remain useful, because they tell whether it is worth going down
  into that level (`advanced_options.h`).
- With a prefix extractor and `whole_key_filtering = false`, only the prefix goes into the
  filter, and prefixes repeated in sequence become a single entry:
  `filter_policy.cc` discards a hash equal to the previous one and comments that, with a prefix,
  repetition is common and always adjacent. Since the edges of a node are contiguous
  in `out` and `in`, the filter of these families grows with the number of distinct nodes
  per SST, not with the number of edges.

### 1.3 The math for 10 billion entries

Assumptions (mine):

- 5 billion elements: 1.0 billion nodes, 3.5 billion edges and 0.5 billion
  events.
- 10 billion keys: `node` 1.0; `pk` 1.0; `out` 3.5; `in` 3.5; `event` 0.5;
  `lbl`, `idx` and `ident` together 0.5.
- On disk, from 175 GB (the README target) to 350 GB (70 bytes per element, the Neo4j
  yardstick); uncompressed, ~500 GB.
- With `level_compaction_dynamic_level_bytes` (default since 8.4), the leveled
  compaction wiki guarantees ~90% of the data in the last level and ~9% in the
  second-to-last.

**Filters.** 10 bits per key are 1.25 bytes per entry.

| Policy | Entries in the filter | Bloom, 10 bits | Ribbon, ~7 bits |
|---|---|---|---|
| Whole key in all families and levels | 10 bn | 12.5 GB | 8.75 GB |
| Whole key, `optimize_filters_for_hits` everywhere | ~1 bn | 1.25 GB | 0.88 GB |
| Plan P1 (below) | up to 4.3 bn | up to 5.4 GB | up to 3.8 GB |
| Plan P2: P1 with `optimize_filters_for_hits` also on `out`, `in` and `event` | up to 1.85 bn | up to 2.3 GB | up to 1.6 GB |

Plan P1:

| Family | Keys | Filter | Entries | Bloom |
|---|---|---|---|---|
| `node` | 1.0 bn | whole key and `optimize_filters_for_hits`: a lookup by id almost always hits | ~0.1 bn | ~0.13 GB |
| `pk` | 1.0 bn | whole key in all levels: the `MERGE` of a new entity is a lookup that misses | 1.0 bn | 1.25 GB |
| `out` | 3.5 bn | 8-byte prefix, no whole key | up to 1.0 bn in the last level (one per node with an edge) and up to 0.35 bn in the others | up to 1.7 GB |
| `in` | 3.5 bn | same | up to 1.35 bn | up to 1.7 GB |
| `event` | 0.5 bn | prefix with stream and entity | up to 0.5 bn | up to 0.63 GB |
| `lbl`, `idx`, `ident` | 0.5 bn | none: they are read by range, and the whole-key filter only serves point lookups (filter wiki) | 0 | 0 |

The "up to" figures assume that every key in the upper levels has its own prefix and that every
node has an edge; in practice the number is lower.

**Index.** Each entry stores a separator, which is the shortened key without the
sequence number since `format_version = 3`, and the block position,
delta-encoded since 4 (index format wiki). Assuming 16 to 32
bytes per entry and 500 GB uncompressed:

| Data block | Blocks | Index |
|---|---|---|
| 4 KB (default) | 122 million | 2.0 to 3.9 GB |
| 16 KB | 30.5 million | 0.5 to 1.0 GB |
| 32 KB | 15.3 million | 0.24 to 0.5 GB |

With 1 TB uncompressed, the numbers double. Two public measurements match the
16 KB row: the wiki's ratio (0.5 MB of index in a 256 MB SST, 0.2%) gives
0.34 to 0.68 GB for 175 to 350 GB on disk, and the "Space Tuning" wiki example
shows 0.97 GB of index and 1.16 GB of filter for 624.57 GB of data. The same
page says that `format_version = 5` with `index_block_restart_interval = 16` makes
the index at least 4 times smaller; the index format wiki mentions half just
from the restart interval, paying more CPU.

**Pinned top level.** One entry per 4 KB partition, at ~25 bytes each:

| Structure | Total | Partitions | Top level |
|---|---|---|---|
| Index | 0.5 to 1.0 GB | 0.12 to 0.24 million | 3 to 6 MB |
| Filter P2 | 2.3 GB | 0.57 million | ~14 MB |
| Filter P1 | 5.4 GB | 1.3 million | ~33 MB |
| Filter everywhere | 12.5 GB | 3.05 million | ~76 MB |

This falls within the wiki's range of 0.1% to 1%.

**Cold read.** With the top level pinned and the whole index (0.5 to 1 GB) in the cache at
high priority, a cold get on `node` that reaches the last level reads one data
block from disk. If the index partition is also cold, it takes two reads.
Out of 9 GB of blocks, P2 leaves ~6 GB for data; P1, ~3 GB.

### 1.4 What needs to stay resident

1. The top level of the index and filter of each open SST: tens of MB per graph.
2. The active and immutable memtables, up to the `WriteBufferManager` ceiling.
3. The table readers of all SSTs open with `max_open_files = -1`. That
   memory stays outside the cache by default; charging it to the cache is
   `cache_usage_options`, which the crate does not expose. Official number per
   file: not found; the measure is `properties::ESTIMATE_TABLE_READERS_MEM`.
4. The blocks that open iterators hold: according to the wiki, about iterators ×
   block size × (levels minus 1, plus files in L0). A thousand iterators with
   a 16 KB block, 7 levels and 4 files in L0 give ~164 MB.
5. With `OptimisticTransactionDB` or `TransactionDB`, the history of already flushed
   memtables that validation uses (section 5.2).

Index and filter partitions, dictionaries and data blocks only need to fit in the
cache for the hot part.

A public report shows the opposite design: 302 million keys, 8.34 GiB on
disk, 4 KB block, 10-bit Ribbon with prefix and
`cache_index_and_filter_blocks = false`, with 4.68 GB of index and filter in
memory and rising (RocksDB issue 12942, with no maintainer reply when I read it).
The keys were URLs. The index grows with key size, and outside the cache it
has no ceiling.

---

## 2. Resources shared between databases

The RocksDB FAQ accepts one DB per shard and says that the thread pool, block cache and
rate limiter can be shared between instances. The memory documentation
of Kafka Streams (Confluent) gives the recipe for several RocksDB instances in the same
process: index and filter in the block cache, memtables bounded by a
shared `WriteBufferManager` charged to the cache, and the same `Cache` in all
instances.

### 2.1 A single block cache

- The basic tuning wiki says to use the same `Cache` in the table options of
  all families of all DBs in the process, and suggests about one third of the
  memory budget for it.
- A family with no cache configured gets its own 32 MB cache (`table.h`). With 10
  families per database, forgetting one means creating a hidden cache.
- LRU: split into up to 64 shards of at least 512 KB (block cache wiki), with
  `high_pri_pool_ratio = 0.5` and `strict_capacity_limit = false` by default
  (`cache.h`). With index and filter at high priority, the LRU list splits
  in two.
- HyperClockCache (HCC): since 10.7.0, HCC without `estimated_entry_charge` is
  considered production-ready and preferred over LRU, and it became the default
  implementation when no cache is passed (`HISTORY.md` and the v10.7.5 notes).
  Limits, according to `cache.h`: it only serves as a block cache (not as a row cache or blob
  cache), priority is applied less strictly than in LRU, and the automatic mode
  depends on anonymous mmap. `estimated_entry_charge` becomes hard to predict
  when the `WriteBufferManager` charges memtables to the cache, so 0 is the right value
  here. The 2025 blog post on unified memory tracking configures
  exactly an HCC with the memtables' cost charged to it.

In the crate:

- `Cache::new_hyper_clock_cache(capacity, estimated_entry_charge)`. The crate
  doc also presents HCC as recommended and indicates 0.
- `Cache::new_lru_cache(capacity)` and `Cache::new_lru_cache_opts(&LruCacheOptions)`.
  `LruCacheOptions` only has `set_capacity` and `set_num_shard_bits`: there is no
  `high_pri_pool_ratio`, `low_pri_pool_ratio` or `strict_capacity_limit`. The
  `librocksdb-sys` has `rocksdb_cache_create_lru_with_strict_capacity_limit`.
- `Cache` is `Clone` over an `Arc` of the C pointer, and
  `BlockBasedOptions::set_block_cache(&cache)` keeps a clone
  (`src/db_options.rs`). Usage: `get_usage()`, `get_pinned_usage()`; adjustment:
  `set_capacity()`.

### 2.2 WriteBufferManager shared and charged to the cache

According to the wiki and `write_buffer_manager.h`:

- a single object, passed to the options of all DBs, bounds the sum of the memtables
  of all families;
- flush triggers in a family when the mutable memtable goes over ~90% of the limit,
  or when the total goes over the limit and the mutable one is already over 50%;
- with a cache, the WBM reserves in the cache, in empty entries, the size of the memory that
  the memtables allocated, and the cache evicts blocks to make room;
- with `allow_stall = true`, when usage goes over the limit, all threads that
  write, in all DBs, stop until usage drops.

Consequence for table 1.5 of the spec: with the cost charged to the cache, the memtables stay
within the cache capacity. A 9 GB cache with 1 GB of memtables leaves 8 GB
for blocks.

In the crate: `WriteBufferManager::new_write_buffer_manager(buffer_size, allow_stall)`
and `new_write_buffer_manager_with_cache(buffer_size, allow_stall, cache)`, attached
via `Options::set_write_buffer_manager(&wbm)`. The type is `Clone`, `Send` and `Sync`,
with `get_usage()`, `get_buffer_size()`, `set_buffer_size()`, `enabled()` and
`set_allow_stall()`. Which memtable to flush is not configurable, not even in C++
(section 8.3).

### 2.3 Flush and compaction threads

- The thread pool belongs to the `Env` and is shared by all DBs that use the
  same `Env`. Flush goes to the HIGH pool and compaction to LOW, and the wiki asks for
  separate, non-empty pools precisely when several DBs share the same
  `Env` (thread pool wiki).
- `max_background_jobs` (default 2) is per DB. With the old limits at -1, one
  quarter of the jobs goes to flush and the rest to compaction (analysis on the
  Small Datum blog, which measured 8 versus 16 jobs).
- On open, each DB only grows the pool: it calls `IncBackgroundThreadsIfNeeded` with
  its compaction limit on LOW and its flush limit on HIGH (`db_impl_open.cc`). The
  pool goes to the largest limit among the DBs, not to the sum, and ten DBs with 4 jobs each
  compete for the same threads.
- `IncreaseParallelism(n)` sets `max_background_jobs = n`, LOW to `n` and HIGH
  to 1, directly on the `Env` (`options.cc`). Called on the options of each DB, it changes the
  pool for all of them.
- BOTTOM pool: the 5.8 note says that universal compactions that include the last
  level can run in their own pool, which reduces blocking of the compaction
  queue, especially with several instances. Not found in the documentation: whether
  leveled compaction uses this pool in the current version.

In the crate: `Env::new()`, whose doc says it returns the default `Env` (it calls
`rocksdb_create_default_env`, which is the C++ `Env::Default()`);
`set_background_threads` (LOW), `set_high_priority_background_threads`,
`set_low_priority_background_threads` and `set_bottom_priority_background_threads`;
`Options::set_env(&env)`, `set_max_background_jobs`, `set_max_subcompactions` and
`increase_parallelism`. Not found in the crate: an `Env` with its own pools (only
`Env::new()` and `Env::mem_env()`), so all DBs in the process share the
same pools.

### 2.4 Rate limiter, SstFileManager and lock buckets

- Rate limiter: `Options::set_ratelimiter` and `set_auto_tuned_ratelimiter` take
  numbers, not an object, and create a limiter on each call. Since
  `Options::clone()` calls `rocksdb_options_create_copy`, which copies the
  C++ `shared_ptr`, deriving the options of all DBs from the same base `Options`
  should share the limiter. This is an inference from the copy semantics; I did not
  test it.
- `SstFileManager` (space ceiling, file deletion rate, sum of the SSTs of
  several DBs): the crate does not have it. The fork does (module `sst_file_manager` and
  `Options::set_sst_file_manager`, since 0.44.0).
- Each `OptimisticTransactionDB` creates `occ_lock_buckets = 1 << 20` lock buckets
  by default (`optimistic_transaction_db.h`). C++ allows sharing one set
  between DBs (`shared_lock_buckets`). The crate does not expose these options; the fork exposes
  `set_occ_lock_buckets` and `set_shared_lock_buckets`. Memory cost per
  bucket: not found.

### 2.5 How it looks in the crate

Signatures checked on docs.rs for 0.25.0:

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

## 3. Compression and prefix

### 3.1 LZ4 on top, ZSTD at the bottom

- The basic tuning wiki recommends LZ4 (or Snappy) for the levels in general and ZSTD
  (or Zlib) for the last one; the space wiki gives the same recipe and suggests a block size of
  16 KB.
- With `level_compaction_dynamic_level_bytes = true`, `compression_per_level[0]`
  still applies to L0, but the other positions count from the base
  level, the one that receives L0, and change as the graph grows
  (`advanced_options.h`). **Recommendation**: `compression_type` and
  `bottommost_compression`, which do not depend on the base level.
- With a different compression per level, RocksDB stops doing the "trivial move"
  that would violate the compression rule (compression wiki).
- Precedent: TiKV does not compress L0 and L1, uses LZ4 from L2 to L4 and ZSTD on L5 and L6,
  with 32 KB blocks, a shared cache of 45% of memory and a 10-bit bloom
  (TiKV configuration file). Without compression on top, the flush uses less
  CPU.

In the crate: `set_compression_type`, `set_compression_per_level`,
`set_bottommost_compression_type`, `set_compression_options(window_bits, level,
strategy, max_dict_bytes)`, `set_bottommost_compression_options(..., enabled)` and
`set_compression_options_parallel_threads`. `DBCompressionType` has `None`,
`Snappy`, `Zlib`, `Bz2`, `Lz4`, `Lz4hc` and `Zstd`; LZ4 and ZSTD are default features
of the crate.

### 3.2 ZSTD with a dictionary

- The dictionary is per SST: training on the file itself gave much better compression
  than training on a neighbor (2021 post on the RocksDB blog).
- During compaction, the uncompressed blocks stay in a buffer, up to the target size of the
  file, while the dictionary is not yet trained; since 6.25 this memory is
  charged to the block cache (dictionary wiki).
- The dictionary goes into a meta-block of the SST. With
  `cache_index_and_filter_blocks = true`, it goes into the cache with high priority; the
  form used for decompression is larger than the stored one.
- Reference values from the post: `max_dict_bytes` of 16 KB and
  `zstd_max_train_bytes` of 100 times `max_dict_bytes`. The gain shows up with
  data blocks of up to 16 KB.

The `SstFileWriter` uses the last level's options: with `bottommost_compression`
set, it takes that type and, if `enabled`, the `bottommost_compression_opts`
(`sst_file_writer.cc`). The SST from the offline load already comes out with ZSTD and a dictionary. But the
`SstFileWriter` passes a target size of 0 to the table constructor, and in that case the
dictionary buffer limit is `max_dict_buffer_bytes`, whose default of 0 turns off the
size cutoff (`block_based_table_builder.cc` only cuts when the limit is
nonzero). From reading the code, the whole file stays uncompressed in
memory until `finish`. The other trigger I saw is the block cache reservation, and I did not
check when it fails. Since the crate does not expose `max_dict_buffer_bytes`, what
limits this memory is the size of each generated SST and the number of writers in
parallel.

In the crate: `set_bottommost_compression_options(window_bits, level, strategy,
max_dict_bytes, enabled)` and `set_bottommost_zstd_max_train_bytes(value, enabled)`.
Missing: `max_dict_buffer_bytes` and `use_zstd_dict_trainer`. A search for the name
`rocksdb_options_set_bottommost_compression_options_max_dict_buffer_bytes` finds the
doc of the `ckb-rocksdb` crate, which indicates that the function exists in the C API; I did not
check it in `librocksdb-sys` 0.19.0. If it exists, it can be called with the
`Options` pointer from the `raw-ptr` feature.

### 3.3 Prefix extractor and prefix bloom for keys with an 8-byte id

In `out` and `in`, the key starts with the node id, 8 bytes big-endian:

- `Options::set_prefix_extractor(SliceTransform::create_fixed_prefix(8))`. Every
  key has at least 8 bytes, so all of them are in the domain.
- `BlockBasedOptions::set_whole_key_filtering(false)`: only the prefix goes into the
  filter.
- Memtable: `set_memtable_prefix_bloom_ratio(0.1)` creates a bloom of
  `write_buffer_size` × 0.1 inside the memtable; `set_memtable_whole_key_filtering`
  puts the whole key there too, and only applies with the ratio above 0
  (`advanced_options.h`).
- Reads: `ReadOptions::set_prefix_same_as_start(true)` with
  `set_iterate_upper_bound` at the next prefix. `set_total_order_seek(true)`
  ignores the bloom. `auto_prefix_mode`, which the wiki has recommended since 6.8 because
  it always returns the same result as total order, is not in the crate.

Pitfalls, according to the prefix seek wiki:

- changing the prefix extractor later leaves SSTs with a filter from another extractor, so
  the prefix length has to be decided before the load;
- `SeekToLast()` and reverse iteration do not work well with a prefix (the
  inverted time in `event` already works around this);
- in manual mode, wrong usage does not raise an error, it gives an undefined result;
- the wiki prefers the "capped" transform over the fixed one. The crate does not have that constructor;
  it has `SliceTransform::create(name, function, in_domain)`, with a Rust function
  called from C for each key in flush, compaction and seek.

Three families in the spec do not start with an 8-byte id:

- `event`: stream first, then entity. An 8-byte prefix covers the stream and
  part of the entity. The prefix needs to be the length of the stream plus 8, or the
  entity needs to come first.
- `pk`: starts with the `entity_id` with a length prefix, which is variable. The whole-key
  bloom serves the get with a label; the seek without a label would only have a bloom with a
  custom transform that reads the length.
- `idx` and `ident`: variable-length value after the type, same case as `pk`.

---

## 4. Bulk load

### 4.1 SstFileWriter and IngestExternalFile

Requirements (creation and ingestion wiki):

- keys in strictly increasing order under the comparator (range tombstones may come
  out of order);
- comparator identical to the DB's; the options given to `SstFileWriter` define
  format and compression;
- every key comes out with sequence number 0 (crate doc).

Ingestion copies or links the file into the DB directory, blocks writes,
flushes the memtable if it overlaps the file, picks the lowest level at
which the file fits without overlapping that level, the ones above it and compactions in
progress, assigns a global sequence number and unblocks writes. In an
empty database, with files of disjoint ranges, everything goes to the last level without
compaction: this is the spec's offline path. Files that overlap within the same batch
always went to L0; `HISTORY.md` records that the assignment is now done in
batches, so that they can land in lower levels. The FAQ suggests generating the SSTs
of disjoint ranges in parallel.

`ingest_behind` always ingests into the last level and skips duplicate keys, but only if the
DB has had `allow_ingest_behind = true` since its creation.

In the crate:

- `SstFileWriter::create(&opts)`, `open(path)`, `put`, `merge`, `delete`,
  `delete_range` (new in 0.25.0), `finish` and `file_size`;
- `ingest_external_file`, `ingest_external_file_opts`, `ingest_external_file_cf` and
  `ingest_external_file_cf_opts`;
- `IngestExternalFileOptions` with `set_move_files`, `set_snapshot_consistency`,
  `set_allow_global_seqno`, `set_allow_blocking_flush` and `set_ingest_behind`;
- `Options::set_allow_ingest_behind`.

Missing: `fail_if_not_bottommost_level`, `link_files`, `write_global_seqno`,
`verify_checksums_before_ingest`, `fill_cache`, `file_opening_threads`,
`IngestExternalFiles` (several families, all or nothing) and the `PrepareFileIngestion`
from 11.5.

**Recommendation**: when loading a new database, `set_allow_global_seqno(false)`,
which makes the ingestion fail if there is overlap with what already exists, and check
afterwards with `property_int_value(properties::num_files_at_level(6))` that everything went
to the last level (7 levels by default), since
`fail_if_not_bottommost_level` is missing. `set_move_files(true)` requires the SST on the same file
system as the DB.

### 4.2 WAL off and flush at the end

- Per the FAQ, a write with the WAL off is lost in a crash if it has not reached an
  SST; with several families, a consistent flush requires `atomic_flush = true`.
- The FAQ recipe for fast inserts: one writer, keys in order, hundreds of
  keys per WriteBatch, vector memtable, more flushes in parallel and automatic
  compaction turned off during the load.
- `PrepareForBulkLoad()` is part of this, but it also sets `num_levels = 2`,
  `disable_auto_compactions = true`, L0 triggers at `1 << 30`, the pending
  compaction limits turned off (0) and `target_file_size_base` of 256 MB,
  counting on a manual compaction at the end (`options.cc`). It suits a DB created only for the load;
  not a database that answers queries.
- `atomic_flush` and `enable_pipelined_write` together: the DB refuses to open
  (`db_impl_open.cc`).

In the crate: `WriteOptions::disable_wal(true)` or `write_without_wal(batch)`;
`flush`, `flush_cf`, `flush_opt` and `flush_cfs_opt(&[...], &FlushOptions)` with
`FlushOptions::set_wait(true)`; `flush_wal(sync)`; `Options::set_atomic_flush`;
`Options::prepare_for_bulk_load`; `set_options(&[("key", "value")])` and
`set_options_cf` to change triggers with the DB open and revert them later;
`wait_for_compact(&WaitForCompactOptions)`.

Compared cost, my own calculation from the tuning guide example (500 GB,
write amplification of ~33 times): loading 350 GB through the memtables, with or
without WAL, writes ~11.6 TB to disk over the compactions. The SST path
writes each byte once, apart from the external sort. Turning off the WAL removes only the
first term of the sum.

---

## 5. Transactions and reading your own writes

### 5.1 WriteBatchWithIndex

In C++, it is a WriteBatch with a search index over the inserted keys.
`GetFromBatchAndDB` reads the batch and the DB and combines the result, with the merge operator if
needed; `NewIteratorWithBase` puts the batch as a delta over a DB iterator. With
`overwrite_key = true`, the iterator does not show the same key twice. DeleteRange
is not supported: the three variants return an error (`write_batch_with_index.h`). Version
10.2.0 added `IngestWriteBatchWithIndex()`, which writes a WBWI into the DB without
going through the memtable.

In the crate: it does not exist. Alternatives:

1. `Transaction` (sections 5.2 and 5.3), which reads its own writes.
2. FFI: the C API has `rocksdb_writebatch_wi_create`,
   `rocksdb_writebatch_wi_get_from_batch_and_db`,
   `rocksdb_writebatch_wi_create_iterator_with_base` and
   `rocksdb_write_writebatch_wi` (`c.h`; in the `librocksdb-sys` index I checked the
   first one), and the `raw-ptr` feature provides the `*mut rocksdb_t`. Snapshot, read
   options and base iterator would also have to be created through FFI, because the crate
   does not expose their pointers.
3. The `rust-rocksdb` 0.53.0 fork has `WriteBatchWithIndex` with
   `get_from_batch_and_db`, `get_pinned_from_batch_and_db`, `iterator_with_base`
   and savepoints. Its `delete_range` writes the range, but the doc warns that it is not
   indexed and does not appear in the batch reads.
4. Our own overlay in Rust: an ordered map from key to value or tombstone, plus
   the deleted ranges, combined with the snapshot iterator. Since the WBWI does not
   index ranges, `DETACH DELETE` inside a transaction needs something like this
   anyway.

### 5.2 OptimisticTransactionDB

- It does not lock keys on write. At commit, it checks whether someone wrote the key
  after its first write in the transaction, or after the snapshot, if there is one
  (`SetSnapshot`). `GetForUpdate` makes a read a condition of the commit; the plain `Get`
  does not (transactions wiki).
- The check uses the in-memory memtable history, and the wiki says to increase
  `max_write_buffer_size_to_maintain` if the commit fails for lack of history.
  In a transaction DB, the default is `max_write_buffer_number × write_buffer_size`
  per family (`advanced_options.h`): 128 MB per family with the defaults, 1.25 GiB
  for the 10 families of a database.
- The default validation is `kValidateParallel`, done before entering the write
  group to reduce mutex contention, with `occ_lock_buckets = 1 << 20`.
- DeleteRange, including inside a WriteBatch passed to `Write()`, is incompatible
  and returns an error (`optimistic_transaction_db.h`).

In the crate: `OptimisticTransactionDB<T>` is an alias of `DBCommon<T,
OptimisticTransactionDBInner>`, whose `inner()` returns the base DB. That is why it
inherits almost everything from the plain DB: `compact_range*`, `ingest_external_file*`,
properties, `snapshot()`, `delete_file_in_range*`, `Checkpoint::new` and
`BackupEngine::create_new_backup`. `delete_range_cf` has existed since 0.23.0 and calls
`rocksdb_delete_range_cf` on the base DB, outside the validation; documentation on
how the validation treats a DeleteRange done this way: not found. Transaction:
`transaction()` and `transaction_opt(&WriteOptions, &OptimisticTransactionOptions)`,
with `OptimisticTransactionOptions::set_snapshot`. `commit()` returns `Busy` on a
conflict and `TryAgain` when history is missing (`Transaction` doc). There is no
`OptimisticTransactionDBOptions`; the fork has it, with `set_validate_policy`,
`set_occ_lock_buckets` and `set_shared_lock_buckets`.

### 5.3 TransactionDB

- Pessimistic: every written key is locked, with timeout and deadlock detection; the
  commit only fails if the DB cannot write (transactions wiki).
- It has refused DeleteRange since 6.15; the recommended way out is the base DB (issue 7913).
- Write policies `WriteCommitted` (default), `WritePrepared` and
  `WriteUnprepared`.

In the crate: `TransactionDB` is a separate struct, with `transaction`,
`transaction_opt`, `prepared_transactions`, get, put, delete, merge, write,
iterators, `snapshot`, `flush`, `flush_wal`, properties and families. It does not have
`compact_range`, `ingest_external_file` or `delete_range`, and `BackupEngine` only
accepts `&DBCommon`, which it is not. Checkpoint: `checkpoint::TransactionDBCheckpoint`
(0.25.0). `TransactionDBOptions`: `set_default_lock_timeout`,
`set_txn_lock_timeout`, `set_max_num_locks` and `set_num_stripes`.
`TransactionOptions`: `set_snapshot`, `set_deadlock_detect`, `set_lock_timeout`,
`set_expiration`, `set_deadlock_detect_depth`, `set_max_write_batch_size` and
`set_skip_prepare`. No write policy; the fork has `set_write_policy` and
`set_skip_concurrency_control`.

`Transaction`, in both types: `get*`, `get_for_update*`, `multi_get*`, `put`,
`delete`, `merge`, iterators that read their own writes, `set_savepoint`,
`rollback_to_savepoint`, `prepare`, `set_name`, `commit`, `rollback`,
`snapshot()`, `get_writebatch()` (a `WriteBatchWithTransaction<true>`) and
`rebuild_from_writebatch`. No `delete_range`; the WriteBatch doc says it is not
supported in `Transaction`.

### 5.4 Snapshots

- A snapshot is a sequence number in a linked list in the DB, and flush and
  compaction preserve what any snapshot sees. With hundreds of thousands
  of snapshots, walking the list can even cause a write stall (snapshot wiki).
- A snapshot does not prevent file removal, but compaction never deletes a
  key that some snapshot sees (RocksDB overview). An old snapshot holds on to
  space and delays purging. An open iterator holds on to the memtables and SSTs from the moment
  it was created, and the wiki asks for short-lived iterators or `Refresh()`.

In the crate: `snapshot()` returns `SnapshotWithThreadMode`, which is `Send` and `Sync`, reads
with `get*` and `multi_get*`, creates iterators and has `sequence_number()`; it is released on
`Drop`. There are also `ReadOptions::set_snapshot`, `latest_sequence_number()` and the
properties `NUM_SNAPSHOTS` and `OLDEST_SNAPSHOT_TIME`.

### 5.5 Group commit, pipelined write and sync

- Group commit is automatic: simultaneous writes that can be combined go
  to the WAL at once, with a single fsync, in groups of up to 1 MB; writes with different
  options may not be grouped together, and RocksDB does not delay anyone to fill the
  group (WAL performance wiki).
- `sync = true` does an fsync before returning. `manual_wal_flush = true` keeps the WAL
  in RocksDB's buffer until `FlushWAL()`, and in that mode the write does not survive a
  crash.
- Pipelined write separates the WAL and memtable queues: +20% in db_bench with
  concurrent writers and WAL on, ~30% on tmpfs with 8 threads (wiki). The DB
  refuses to open when it is combined with `unordered_write` or `atomic_flush`
  (`db_impl_open.cc`).
- `unordered_write` trades snapshot immutability for throughput (+63% with WAL and
  +131% without, in the wiki) and requires `allow_concurrent_memtable_write`.
- `two_write_queues` serves the two-phase commit of `WritePrepared` transactions
  (WritePrepared wiki).

In the crate: `WriteOptions::set_sync`, `disable_wal`, `set_no_slowdown` and
`set_low_pri`; `Options::set_enable_pipelined_write`, `set_unordered_write`,
`set_manual_wal_flush`, `set_allow_concurrent_memtable_write` and
`set_enable_write_thread_adaptive_yield`; `flush_wal(sync)`. No
`two_write_queues`.

With one thread writing per database, as in the spec's committer, RocksDB's group
commit has nothing to group: the committer does the grouping, with one WriteBatch per
group and `sync = true`. Pipelined write helps when there are several concurrent
writers and also blocks `atomic_flush`. **Recommendation**: leave it off and
measure.

### 5.6 What changes in the spec's committer

Decision at stake: who detects conflicts. Options: RocksDB's `OptimisticTransactionDB`,
or a plain DB with the validation in the committer. A language model tends to
recommend building, so the ready-made option comes first.

**OptimisticTransactionDB.** It is in the crate today, without FFI. It brings snapshots, reading
your own writes, `get_for_update` for the absent `pk` that `MERGE` read and
a validation that RocksDB itself maintains. `Busy` and `TryAgain` become the Neo4j
transient error without new code. Costs: DeleteRange only through the base DB, outside the
validation; the in-memory memtable history (up to 1.25 GiB per database by
default), or more `TryAgain` if it is trimmed; validation policy and buckets
fixed in the crate; and the Raft follower would apply the batch via `write`, outside a
transaction, with two apply paths.

**Plain DB and our own committer.** The validation does not depend on the memtable
history: the committer keeps, for the keys committed after the oldest snapshot
in use, the commit's sequence number (`latest_sequence_number()` after
each `write_opt`) and compares it with the `sequence_number()` of the transaction's snapshot.
The plain WriteBatch accepts `delete_range_cf`, and the same `WriteBatch::data()` becomes the
Raft entry (`WriteBatch::from_data` on the follower), with a single apply
path. Costs: reading your own writes and the validation become Brainwire
code, including the case of a key that was read and was absent.

What reverses it: if the overlay and our own validation do not pass gate 1 (2026-09-30),
`OptimisticTransactionDB` comes in with `set_max_write_buffer_size_to_maintain`
set explicitly, and `DETACH DELETE` switches to deleting point by point inside the transaction.
Medium confidence: the memory cost of the history comes from the documented default, not from
measurement. Decision: Allan.

---

## 6. Backup, recovery and purge

### 6.1 Checkpoint

- SST and blob files become hard links when the destination is on the same file system,
  and copies when it is not; MANIFEST and the like are always copied; the directory must not
  exist (`checkpoint.h`).
- `log_size_for_flush` 0 (default) always forces a flush. With another value, the
  checkpoint may come out without the recent data if there were writes without WAL, which is the
  case of `bulk()` (`checkpoint.h`).
- The checkpoint opens as a read-only or normal DB; opened as normal, the SSTs
  remain hard links until they become obsolete (checkpoints wiki). It is the natural
  format for the Raft snapshot.
- The C++ API returns a sequence number guaranteed in the checkpoint
  (`sequence_number_ptr`); the crate does not. **Recommendation**: write the Raft applied
  index to the `meta` family, in the same WriteBatch as each entry, so that the
  checkpoint carries the index along.

In the crate: `checkpoint::Checkpoint::new(&db)` (accepts `DBCommon`, that is, DB and
`OptimisticTransactionDB`), `create_checkpoint(path)` (with `log_size_for_flush =
0`), `create_checkpoint_with_log_size(path, n)` and `TransactionDBCheckpoint`.
`ExportColumnFamily` is missing; the C API has `rocksdb_checkpoint_export_column_family`
and the fork has `ExportImportFilesMetaData`.

### 6.2 Incremental BackupEngine

- It is incremental by default: with `share_table_files = true`, an SST already present in the
  backup directory is not copied again; with `share_files_with_checksum =
  true` (default), the name of the shared file carries size and checksum
  (`backup_engine.h` and backup wiki).
- Without `flush_before_backup`, the backup includes the WALs of the live memtables. With WAL
  disabled, the header says to use `flush_before_backup = true` so as not to lose what
  is only in the memtable.
- Restore checks the checksum of each file; `PurgeOldBackups(N)` keeps the N
  newest; in a wiki test, 16 threads brought the backup down to a third of the time.
- Backup relies on the checkpoint mechanism: it suspends file deletion,
  lists the live files, copies and resumes.

In the crate: `BackupEngine::open(&BackupEngineOptions, &Env)`,
`create_new_backup(&db)`, `create_new_backup_flush(&db, flush)`,
`purge_old_backups(n)`, `restore_from_latest_backup(db_dir, wal_dir,
&RestoreOptions)`, `restore_from_backup(..., id)`, `verify_backup(id)` and
`get_backup_info()`. `BackupEngineOptions` has only `new(dir)`,
`set_max_background_operations` and `set_sync` (sync and fsync were added in 0.24.0). The
C++ default is already incremental; missing are rate limit, `backup_log_files` and the other
settings, which `librocksdb-sys` has
(`rocksdb_backup_engine_options_set_share_table_files` and
`rocksdb_backup_engine_options_set_backup_rate_limit`). `create_new_backup` takes
`&DBCommon`, so it does not accept `TransactionDB`.

### 6.3 DeleteRange and DeleteFilesInRange

Per `db.h`, DeleteRange removes the interval from the start (included) to the end (excluded)
and is production-ready, with three caveats: many range tombstones in the
memtable degrade reads (fixed by flushing now and then); limiting
`max_open_files` with range tombstones degrades reads (use -1); and it is incompatible
with row cache, with an error on the call. How it works (implementation wiki): the
range tombstone goes into its own memtable and, in the SST, into a meta-block; it is
fragmented on read; compaction deletes the covered keys, and the tombstone
itself only goes away at the last level, when each key has a single version there and
no snapshot covers it. In the 2018 blog test, gets with DeleteRange ran at
1.63 million per second, versus 1.67 million with the pattern of scanning and deleting key by
key.

`DeleteFilesInRange` deletes only whole files inside the range: keys may remain
in a file that crosses the boundary, L0 files stay, and earlier snapshots may
stop seeing the data (`convenience.h`). It serves to free space before a
DeleteRange followed by compaction.

In the crate: `delete_range_cf` and `delete_range_cf_opt` on the DB;
`WriteBatch::delete_range` and `delete_range_cf`, only on the non-transactional batch;
`OptimisticTransactionDB::delete_range_cf`, via the base; `SstFileWriter::delete_range`;
`delete_file_in_range` and `delete_file_in_range_cf`. No DeleteRange in
`Transaction` or in `TransactionDB`. `Options::set_row_cache` exists;
**recommendation**: do not use it, because of DeleteRange.

### 6.4 CompactRange for physical purge

- Manual compaction discards deleted and overwritten versions, blocks until
  it finishes, and the actual interval may be larger than the requested one (`db.h`).
- In leveled compaction, the files in the range move down to the last level with
  data. But the default, `kIfHaveCompactionFilter`, skips the last level when there is no
  compaction filter (manual compaction wiki). For purge, `kForce` or
  `kForceOptimized`, which avoids recompacting, at the last level, files created by the
  same compaction.
- `exclusive_manual_compaction` defaults to `true`: no other compaction runs alongside.
  With the database in use, `false`.

In the crate: `compact_range`, `compact_range_opt`, `compact_range_cf` and
`compact_range_cf_opt`, with `CompactOptions`: `set_exclusive_manual_compaction`,
`set_bottommost_level_compaction(BottommostLevelCompaction::{Skip,
IfHaveCompactionFilter, Force, ForceOptimized})`, `set_change_level`,
`set_target_level` and `set_full_history_ts_low`. Missing are `max_subcompactions` per
call, `allow_write_stall` and `canceled`; `Options::set_max_subcompactions` applies
to the whole DB.

### 6.5 Time until the data is actually gone

| Where the data can remain | Until when | What controls it |
|---|---|---|
| Read with a snapshot prior to the delete | the snapshot is released | lifetime of snapshots and transactions |
| Memtable | the flush writes the tombstone | `write_buffer_size`, WBM |
| WAL | all families flush past the largest sequence number in the file (WAL wiki) | `max_total_wal_size` forces a flush of an idle family; archived WAL only exists with `WAL_ttl_seconds` or `WAL_size_limit_MB` above 0; with `recycle_log_file_num` above 0 (default 0), the file is reused and overwritten instead of deleted |
| SST, including at the last level | a compaction brings the tombstone and the old version together at the last level, with no snapshot in between | forced CompactRange (deadline: the duration of the call); without it, see below |
| Obsolete SST after compaction | no iterator holds the file and file deletion is enabled | short-lived iterator; `IS_FILE_DELETIONS_ENABLED` |
| Checkpoint | the directory is deleted | checkpoint retention (the hard link holds the file) |
| Backup | the last backup that uses the SST is removed | `purge_old_backups` |

Without manual compaction:

- `ttl` (default of 30 days in the block-based format): in leveled compaction, files outside the
  last level with all keys older than the TTL enter compaction, cascading
  down to the last level, and the comment in `advanced_options.h` says the
  goal is to remove deleted or updated entries from disk. By the letter of the
  comment, a file that receives a new key no longer has only old keys; not
  found: a guaranteed deadline when the same range keeps receiving writes.
- `periodic_compaction_seconds` (default 0 in leveled compaction without a compaction filter): files
  older than the value are rewritten, into the next level with
  `level_compaction_dynamic_level_bytes`, or in place if they are already in the last one.
  The age comes first from `file_creation_time`, so each move down waits for the period
  again. Since 10.4.0, RocksDB looks for these compactions every 12 hours.

Procedure for the engine to state that the data has left the disk, built from what the
crate exposes (inference, to be tested):

1. write the deletes and run `flush_cf` on the families touched;
2. wait for `OLDEST_SNAPSHOT_TIME` to pass the moment of the delete, or for `NUM_SNAPSHOTS`
   to reach zero, and for the earlier iterators to close;
3. `compact_range_cf_opt` on each range, with `Force` and
   `exclusive_manual_compaction = false`;
4. check `MIN_OBSOLETE_SST_NUMBER_TO_KEEP` and `MIN_LOG_NUMBER_TO_KEEP` against
   `live_files()` and the WALs in the directory;
5. record that earlier checkpoints and backups keep the data until their retention
   expires.

Deleting the file does not zero the block on the SSD. If the requirement is cryptographic
erasure, that is a different subject, which I did not research.

---

## 7. What the `rocksdb` crate exposes

All rows were checked on docs.rs for 0.25.0 or in the code at tag
`v0.25.0`. The "Version" column says 0.25.0 and, where the changelog gives it, since
when it has existed.

| Feature | Exposes | API | Version | Alternative |
|---|---|---|---|---|
| Partitioned index | yes | `BlockBasedOptions::set_index_type(BlockBasedIndexType::TwoLevelIndexSearch)`, `set_metadata_block_size` | 0.25.0 | |
| Partitioned filter | yes | `set_partition_filters` | 0.25.0 | |
| Index and filter in the cache, high priority | yes | `set_cache_index_and_filter_blocks`, `set_cache_index_and_filter_blocks_with_high_priority` | 0.25.0 | |
| Pin the top level and L0 | yes | `set_pin_top_level_index_and_filter`, `set_pin_l0_filter_and_index_blocks_in_cache`, `set_top_level_index_pinning_tier`, `set_partition_pinning_tier`, `set_unpartitioned_pinning_tier` | 0.25.0 | |
| Bloom, Ribbon and hybrid | yes | `set_bloom_filter`, `set_ribbon_filter`, `set_hybrid_ribbon_filter` | 0.25.0 | |
| `optimize_filters_for_memory` and `optimize_filters_for_hits` | yes | `BlockBasedOptions::set_optimize_filters_for_memory`, `Options::set_optimize_filters_for_hits` | 0.25.0 | |
| Index restart interval, `format_version` | yes | `set_index_block_restart_interval`, `set_format_version` | 0.25.0 | |
| Charge table reader, filter construction and metadata to the cache | no | | 0.25.0 | not checked in the C API; measure with `ESTIMATE_TABLE_READERS_MEM` |
| Measure memory | yes | `property_int_value` with `ESTIMATE_TABLE_READERS_MEM`, `BLOCK_CACHE_USAGE`, `BLOCK_CACHE_PINNED_USAGE`, `CUR_SIZE_ALL_MEM_TABLES`; `properties::aggregated_table_properties_at_level`; `Cache::get_usage` | 0.25.0 | |
| Shared LRU cache | partial | `Cache::new_lru_cache`, `new_lru_cache_opts` (only capacity and shards) | 0.25.0 (LRU options since 0.23.0) | `strict_capacity_limit` via FFI (`rocksdb_cache_create_lru_with_strict_capacity_limit`); `high_pri_pool_ratio` stays at the default 0.5 |
| HyperClockCache | yes | `Cache::new_hyper_clock_cache(capacity, estimated_entry_charge)` | 0.25.0 | fork: `HyperClockCacheOptions` |
| Shared WriteBufferManager charged to the cache | yes | `WriteBufferManager::new_write_buffer_manager_with_cache`, `Options::set_write_buffer_manager` | 0.25.0 | |
| Choose which memtable to flush across DBs | does not exist in RocksDB | | | PR 15047, open |
| Shared flush and compaction pools | yes | `Env::new`, `set_background_threads`, `set_high_priority_background_threads`, `set_bottom_priority_background_threads`, `Options::set_env`, `set_max_background_jobs` | 0.25.0 | |
| `Env` with its own pools | not found | only `Env::new()` and `Env::mem_env()` | 0.25.0 | |
| Shared rate limiter | partial | `set_ratelimiter`, `set_auto_tuned_ratelimiter` (one limiter per call) | 0.25.0 | clone a base `Options` (inference) |
| SstFileManager | no | | 0.25.0 | fork: `Options::set_sst_file_manager` |
| OptimisticTransactionDB options (validation, buckets) | no | | 0.25.0 | fork: `OptimisticTransactionDBOptions` |
| Per-level and last-level compression | yes | `set_compression_type`, `set_compression_per_level`, `set_bottommost_compression_type` | 0.25.0 | |
| ZSTD dictionary | partial | `set_bottommost_compression_options(.., max_dict_bytes, enabled)`, `set_bottommost_zstd_max_train_bytes` | 0.25.0 | `max_dict_buffer_bytes` via FFI with `raw-ptr`; `use_zstd_dict_trainer` not checked in the C API |
| Parallel compression | yes | `set_compression_options_parallel_threads` | 0.25.0 | |
| Fixed prefix extractor | yes | `SliceTransform::create_fixed_prefix` | 0.25.0 | |
| "Capped" prefix extractor | no | | 0.25.0 | `SliceTransform::create` with a Rust function |
| Bloom in the memtable | yes | `set_memtable_prefix_bloom_ratio`, `set_memtable_whole_key_filtering` | 0.25.0 | |
| Prefix-only filter | yes | `BlockBasedOptions::set_whole_key_filtering(false)` | 0.25.0 | |
| Prefix reads | partial | `ReadOptions::set_prefix_same_as_start`, `set_total_order_seek`, `set_iterate_upper_bound`, `set_iterate_lower_bound` | 0.25.0 | `auto_prefix_mode` not exposed; not checked in the C API |
| SstFileWriter | yes | `SstFileWriter::create`, `open`, `put`, `merge`, `delete`, `delete_range`, `finish` | 0.25.0 (`delete_range` since 0.25.0) | |
| IngestExternalFile | partial | `ingest_external_file*`; `IngestExternalFileOptions::set_move_files`, `set_snapshot_consistency`, `set_allow_global_seqno`, `set_allow_blocking_flush`, `set_ingest_behind` | 0.25.0 | without `fail_if_not_bottommost_level` (check with `num_files_at_level`), `link_files`, `IngestExternalFiles` and `PrepareFileIngestion`; fork not checked |
| Ingest behind | yes | `set_ingest_behind`, `Options::set_allow_ingest_behind` | 0.25.0 | |
| WAL disabled | yes | `WriteOptions::disable_wal`, `write_without_wal` | 0.25.0 | |
| Flush at the end, atomic | yes | `flush*`, `flush_cfs_opt`, `FlushOptions::set_wait`, `Options::set_atomic_flush`, `flush_wal` | 0.25.0 | |
| PrepareForBulkLoad | yes | `Options::prepare_for_bulk_load` | 0.25.0 | watch out for `num_levels = 2` |
| Change an option with the DB open | yes | `set_options`, `set_options_cf` | 0.25.0 | |
| WriteBatchWithIndex | no | | 0.25.0 | `Transaction`; FFI (`rocksdb_writebatch_wi_*` and `raw-ptr`); fork; custom overlay |
| OptimisticTransactionDB | yes | `OptimisticTransactionDB`, `transaction_opt`, `OptimisticTransactionOptions::set_snapshot` | 0.25.0 | |
| TransactionDB | yes | `TransactionDB`, `TransactionDBOptions`, `TransactionOptions`, `prepared_transactions` | 0.25.0 | |
| `WritePrepared` and `WriteUnprepared` write policy | no | | 0.25.0 | fork: `TransactionDBOptions::set_write_policy` |
| Read your own writes | yes, in the transaction | `Transaction::get*`, `get_for_update*`, iterators | 0.25.0 | |
| DeleteRange in the transaction | no (not even C++ accepts it) | | 0.25.0 | base DB |
| Snapshot | yes | `snapshot()`, `SnapshotWithThreadMode::sequence_number`, `ReadOptions::set_snapshot`, `Transaction::snapshot` | 0.25.0 | |
| Serialized WriteBatch for Raft | yes | `WriteBatch::data`, `from_data`, `iterate` | 0.25.0 | |
| Group commit | automatic | none | | |
| Pipelined write and unordered write | yes | `set_enable_pipelined_write`, `set_unordered_write` | 0.25.0 | |
| `two_write_queues` | no | | 0.25.0 | not checked in the C API |
| Sync and manual WAL | yes | `WriteOptions::set_sync`, `Options::set_manual_wal_flush`, `flush_wal` | 0.25.0 | |
| Checkpoint | yes | `Checkpoint::new`, `create_checkpoint`, `create_checkpoint_with_log_size`, `TransactionDBCheckpoint` | 0.25.0 (`TransactionDB` checkpoint since 0.25.0) | |
| Export a family | no | | 0.25.0 | C API `rocksdb_checkpoint_export_column_family`; fork |
| BackupEngine | partial | `BackupEngine::open`, `create_new_backup`, `create_new_backup_flush`, `purge_old_backups`, `restore_from_latest_backup`, `restore_from_backup`, `verify_backup`, `get_backup_info`; `BackupEngineOptions::set_max_background_operations`, `set_sync` | 0.25.0 (`set_sync` since 0.24.0) | rate limit and `share_table_files` via FFI |
| DeleteRange | yes | `delete_range_cf`, `delete_range_cf_opt`, `WriteBatch::delete_range*`, `OptimisticTransactionDB::delete_range_cf`, `SstFileWriter::delete_range` | 0.25.0 (in OCC since 0.23.0) | |
| DeleteFilesInRange | yes | `delete_file_in_range`, `delete_file_in_range_cf` | 0.25.0 | |
| Forced CompactRange | yes | `compact_range_cf_opt`, `CompactOptions::set_bottommost_level_compaction(Force)`, `set_exclusive_manual_compaction` | 0.25.0 | no per-call `max_subcompactions` |
| TTL and periodic compaction | yes | `set_ttl`, `set_periodic_compaction_seconds` | 0.25.0 | |
| Suspend file deletion, list files | yes | `disable_file_deletions`, `enable_file_deletions` (only on the DB), `live_files` | 0.25.0 | |
| Wait for compaction | yes | `wait_for_compact` | 0.25.0 | |
| `max_open_files` and opening threads | yes | `set_max_open_files`, `set_max_file_opening_threads`, `set_skip_stats_update_on_db_open` | 0.25.0 | |
| `skip_checking_sst_file_sizes_on_db_open` | no | deprecated in RocksDB 10.5 and in crate 0.24.0 | 0.25.0 | the check now runs in parallel |
| `fast_sst_open`, `reuse_manifest_on_open` | no | | 0.25.0 | not checked in the C API |
| C pointer to call the C API | yes, with the `raw-ptr` feature | `AsRawPtr` for `DB`, `Options` and `Env` | 0.25.0 | |

The decision the table raises: stay with `rocksdb` and complete it via FFI, or switch
to the fork. In favor of `rocksdb`: it is the plan's crate, and what is missing for the
recommended design (custom overlay, plain DB) is small and fits in targeted FFI. In favor of the
fork: `WriteBatchWithIndex`, `SstFileManager`, OCC options, write policy
and more types already built, with monthly releases on the same 11.8.1. I did not
verify who maintains the fork or its compatibility policy. What
flips it: if Brainwire needs three or more items from the fork, switching costs
less than the FFI. Decision: Allan.

---

## 8. Risks at scale and adjustments

### 8.1 Write amplification

Leveled compaction rewrites each byte several times: in the example from the
tuning guide (500 GB, 512 MB L1, multiplier 10), ~33 times, and the leveled
compaction wiki says it usually exceeds 10. On the online path of
Brainwire, the ingestion ceiling is ~10,000 elements per second (README); with 2
keys of ~50 bytes per element (my assumption), that is ~1 MB/s logical and ~33 MB/s
of compaction, comfortable for NVMe. For the 5 billion load through the memtables, that is
~11.6 TB written (section 4.2).

Adjustments: large loads via SST (4.1); a larger `write_buffer_size` and
`min_write_buffer_number_to_merge` reduce amplification (write stalls wiki),
but weigh on the WBM (8.3); `set_max_subcompactions` parallelizes L0 to L1, which is not
parallel by default (leveled compaction wiki); `set_auto_tuned_ratelimiter`
smooths the peaks. Tiered compaction reduces write amplification at the cost
of space and reads (compaction wiki), and leveled saves more space
(space wiki). Given the README's disk target, leveled stays.

### 8.2 L0 and pending compaction stalls

Defaults: writes slow down at 20 files in L0 and stop at 36; they slow down at
64 GB of pending compaction and stop at 256 GB; and they stop when the immutable
memtables reach `max_write_buffer_number` (2) (`advanced_options.h` and the write
stalls wiki). In a 350 GB database, a burst goes past the 64 GB pending with
ease. With the pool shared, one database's backlog takes threads from the others.

Adjustments from the wiki: more background jobs, larger memtables, a separate flush
pool. During a load, raise the triggers with `set_options` and revert them afterwards.
`WriteOptions::set_no_slowdown(true)` makes the write fail with `Incomplete` instead
of waiting. Monitor `IS_WRITE_STOPPED`, `ACTUAL_DELAYED_WRITE_RATE`,
`ESTIMATE_PENDING_COMPACTION_BYTES`, `COMPACTION_PENDING` and
`NUM_RUNNING_COMPACTIONS` per database.

### 8.3 Shared WriteBufferManager

- RocksDB PR 15047 (open, from 2026-08 to 2026-09): the shared WBM
  triggers the flush in the DB whose write notices the pressure and picks the oldest mutable
  memtable of that DB. With many families or DBs, it flushes small memtables
  several times and does not touch the ones that take up the most. The PR proposes flushing the largest
  one in the DB, or the largest across all DBs.
- Harper (PR 2492): in production, two nodes stopped every day. With the budget
  spread across many families, none reached the trigger, the flush counter
  for the WBM reason stayed at 0 and all write threads waited; the way out
  was to restart. They changed the default of `allow_stall` to `false`.
- Camunda (issue 51669): WBM and cache sized at startup; when the number of
  instances grew, `min_write_buffer_number_to_merge = 3` prevented the flush, and
  they lowered it to 1.

Adjustments: `allow_stall = false` and an alert on `WriteBufferManager::get_usage()` above
the budget; `min_write_buffer_number_to_merge = 1` (default);
a small `write_buffer_size` in the small families (`meta`, `acl`, `lbl`);
`max_total_wal_size` to force the flush of an idle family; recompute the WBM with
`set_buffer_size` when a database is created or deleted.

### 8.4 Opening with many SSTs

With 175 to 350 GB per graph, that is 2,700 to 5,500 SSTs with 64 MB files (the default
for `target_file_size_base`) and 700 to 1,400 with 256 MB, times the number of
databases. The fast-open wiki lists the causes of slowness: replay of the
MANIFEST, replay of the WAL, reading the footer and metadata of all SSTs with
`max_open_files = -1` (random reads), checking the size of all SSTs
and opening one DB at a time.

Adjustments: `set_max_file_opening_threads` (default 16),
`set_skip_stats_update_on_db_open(true)`, `set_max_manifest_file_size`,
`set_max_total_wal_size`, a larger `target_file_size_base` and the databases opened in
parallel. In RocksDB, 10.5 started checking sizes in parallel; 11.2
brought `fast_sst_open` (experimental) and 11.3 `reuse_manifest_on_open`, which the
crate does not expose. Not found: a public measurement of open time with thousands of
SSTs.

### 8.5 `max_open_files` and descriptors

With -1, all files stay open (the tuning guide recommends it, to avoid the
cost of the table cache, and `db.h` asks for the same because of DeleteRange). Each SST
holds a descriptor, so the container's `ulimit -n` must cover the sum of
SSTs, WALs, MANIFESTs and LOGs of all databases, plus sockets. TiKV uses 40,960
as its default. Table reader memory stays outside the cache (1.4). Limiting
`max_open_files` trades memory for reopening and degrades reads with range
tombstones.

### 8.6 Long-lived snapshots and iterators

A long transaction or query holds versions (space and purge) and SSTs; hundreds of
thousands of snapshots cause stalls; an iterator holds blocks and files. Adjustments: the
spec's per-query execution cap, snapshot released at the end of each
transaction, iterator recreated in long scans. The wiki suggests `Refresh()`; I did not
check whether the crate exposes it.

### 8.7 OptimisticTransactionDB history and buckets

If it is chosen, `set_max_write_buffer_size_to_maintain` needs an explicit
value: by default, up to 1.25 GiB per database with 10 families, above the
spec's memtable budget. In Harper's case, the same retention reached
~256 MiB per family. The 2^20 lock buckets per database cannot be changed through the crate.

### 8.8 Build and platform

RocksDB requires a C++20 compiler (GCC 11 or later, Clang 10 or later) since
10.7.0, including for those who use the headers, so the image that compiles
`librocksdb-sys` 0.19 needs it. The crate requires Rust 1.88.0 (README). Automatic
HCC depends on anonymous mmap. The crate has the `jemalloc` and
`io-uring` features.

### 8.9 What was not found

- memory size per open table reader;
- whether leveled compaction uses the BOTTOM pool in the current version;
- how `OptimisticTransactionDB` validation handles a DeleteRange done on the base;
- the guaranteed deadline for `ttl` when the range keeps receiving writes;
- open time measured with thousands of SSTs;
- memory cost per OCC lock bucket;
- when the cache reservation for the dictionary buffer fails;
- whether the C API has `two_write_queues`, `auto_prefix_mode`,
  `fail_if_not_bottommost_level`, `use_zstd_dict_trainer`, `fast_sst_open` and
  `cache_usage_options` (not checked);
- whether the crate exposes iterator `Refresh()`.

---

## 9. Sources

RocksDB wiki:

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

RocksDB headers and code (branch `main`):

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

RocksDB blog, release notes, issues and PRs:

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

`rocksdb` crate 0.25.0:

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

`rust-rocksdb` fork 0.53.0:

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

Other public technical sources:

- https://docs.confluent.io/platform/current/streams/developer-guide/memory-mgmt.html
- https://docs.pingcap.com/tidb/stable/tikv-configuration-file/
- https://github.com/tikv/tikv/issues/6486
- http://smalldatum.blogspot.com/2022/07/the-impact-of-maxbackgroundjobs-on.html
- https://github.com/HarperFast/harper/pull/2492
- https://github.com/camunda/camunda/issues/51669
- https://docs.rs/ckb-rocksdb/latest/ckb_rocksdb/struct.Options.html (only as a
  search result, for the name of the function for `max_dict_buffer_bytes`)

In this repository:

- `external-brainwire/README.md` (volume, disk target, ingestion ceiling)
- `external-brainwire/PLAN.md`
- `external-brainwire/docs/superpowers/specs/2026-09-24-brainwire-design.md`
  (section 1: families, keys, memory, transactions, loading and purge)

---

Provenance: AI-assisted draft (Claude Opus 5.5, 2026-09-24). Anti-trendslop
protocol applied to the decisions in sections 5.6 and 7. Tensions identified:
RocksDB's ready-made validation versus a custom committer; `rocksdb` crate with FFI
versus the `rust-rocksdb` fork. Final decision: Allan.
