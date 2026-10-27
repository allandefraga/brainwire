use brainwire_core::{acl::ReadVisibility, schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, ExecError, Out, QueryAccess, QueryOutput, QueryTransaction, QueryTransactionState,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn open(path: &std::path::Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}
fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let schema:GraphMapping=serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Doc","properties":["title","term","authority","document_id","ACL_Secret"],"indexed_fields":["title","term","ACL_Secret"]}],"edges":[{"type":"LINK","from":{"label":"Doc"},"to":{"label":"Doc"},"properties":["weight"]}]}"#).unwrap();
    db.apply_ontology(&schema).unwrap();
    execute(&db,"CREATE (a:Doc {entity_id:'a',title:'A',term:'needle',authority:0.2}),(b:Doc {entity_id:'b',title:'B',term:'needle',authority:0.8}),(h:Doc {entity_id:'hidden',title:'Hidden',term:'needle',authority:99}),(m:Doc {entity_id:'missing',title:'Missing'}),(d:Doc {entity_id:'denied',title:'Denied'}),(x:Doc {entity_id:'document',document_id:'source-document',title:'Document',term:'other'}) CREATE (a)-[:LINK {weight:1}]->(b) CREATE (b)-[:LINK {weight:1}]->(a) CREATE (a)-[:LINK {weight:'invalid-hidden-weight'}]->(h) CREATE (h)-[:LINK]->(b) CREATE (h)-[:LINK]->(m)",&BTreeMap::new()).unwrap();
    for (id, allow, deny) in [
        ("a", vec!["alice"], vec![]),
        ("b", vec!["alice"], vec![]),
        ("hidden", vec!["bob"], vec![]),
        ("denied", vec!["alice"], vec!["alice"]),
        ("document", vec!["alice"], vec![]),
    ] {
        stamp(&db, id, allow, deny);
    }
    (dir, db)
}
fn stamp(db: &Database, id: &str, allow: Vec<&str>, deny: Vec<&str>) {
    let params = BTreeMap::from([
        ("id".into(), Value::Str(id.into())),
        (
            "allow".into(),
            Value::List(allow.into_iter().map(|v| Value::Str(v.into())).collect()),
        ),
        (
            "deny".into(),
            Value::List(deny.into_iter().map(|v| Value::Str(v.into())).collect()),
        ),
    ]);
    let result=execute(db,"CALL db.setNodeAcl({label:'Doc',entityId:$id,allow:$allow,deny:$deny}) YIELD updated RETURN updated",&params).unwrap();
    assert_eq!(result.rows, vec![vec![Out::Value(Value::Bool(true))]]);
}
fn policy() -> ReadVisibility {
    ReadVisibility {
        principals: vec!["alice".into()],
        ..Default::default()
    }
}
fn restricted(db: &Database, query: &str) -> Result<QueryOutput, ExecError> {
    let mut transaction = QueryTransaction::begin(db)?.with_read_visibility(policy())?;
    let output = transaction.run_with_access(
        query,
        &BTreeMap::new(),
        QueryAccess {
            write: false,
            instance_admin: false,
        },
    )?;
    assert_eq!(transaction.commit()?, None);
    Ok(output)
}
fn string(value: &str) -> Out {
    Out::Value(Value::Str(value.into()))
}
fn int(value: i64) -> Out {
    Out::Value(Value::Int(value))
}

#[test]
fn acl_filters_scans_primary_keys_indexes_bound_variables_counts_and_both_edge_directions() {
    let (_dir, db) = setup();
    assert_eq!(
        restricted(&db, "MATCH (n:Doc) RETURN n.entity_id")
            .unwrap()
            .rows,
        vec![
            vec![string("a")],
            vec![string("b")],
            vec![string("document")]
        ]
    );
    for (id, title) in [
        ("hidden", "Hidden"),
        ("missing", "Missing"),
        ("denied", "Denied"),
    ] {
        for query in [
            format!("MATCH (n:Doc {{entity_id:'{id}'}}) RETURN count(*) AS n"),
            format!("MATCH (n:Doc) WHERE n.title='{title}' RETURN count(n) AS n"),
        ] {
            assert_eq!(
                restricted(&db, &query).unwrap().rows,
                vec![vec![int(0)]],
                "{query}"
            );
        }
    }
    assert_eq!(
        restricted(
            &db,
            "MATCH (n:Doc {entity_id:'a'}) MATCH (n:Doc) RETURN count(n)"
        )
        .unwrap()
        .rows,
        vec![vec![int(1)]]
    );
    for query in [
        "MATCH (a:Doc)-[:LINK]->(b:Doc) RETURN a.entity_id,b.entity_id",
        "MATCH (b:Doc)<-[:LINK]-(a:Doc) RETURN b.entity_id,a.entity_id",
    ] {
        assert_eq!(
            restricted(&db, query).unwrap().rows,
            vec![
                vec![string("a"), string("b")],
                vec![string("b"), string("a")]
            ]
        );
    }
    assert_eq!(
        restricted(&db, "MATCH (a:Doc)-[:LINK]->(b:Doc) RETURN count(*)")
            .unwrap()
            .rows,
        vec![vec![int(2)]]
    );
}

#[test]
fn private_acl_properties_are_stripped_from_scalar_and_whole_node_outputs_even_legacy() {
    let (_dir, db) = setup();
    // Simulate a pre-reservation legacy row without using today's rejected property writes.
    let catalog = db.catalog().unwrap().unwrap();
    let ty = catalog.node_type("Doc").unwrap();
    let mut transaction = db.begin();
    let id = brainwire_store::graph::find_by_entity_id(&transaction, "a", Some(ty.id)).unwrap()[0];
    let mut record = brainwire_store::graph::get_node(&transaction, id)
        .unwrap()
        .unwrap();
    record
        .row
        .extra
        .insert("ACL_Secret".into(), Value::Str("secret-a".into()));
    record.row.extra.insert(
        "acl_allow".into(),
        Value::List(vec![Value::Str("private-alice".into())]),
    );
    let mut encoded = (record.entity_id.len() as u32).to_be_bytes().to_vec();
    encoded.extend_from_slice(record.entity_id.as_bytes());
    encoded.extend_from_slice(&brainwire_core::codec::encode_row(&record.row));
    transaction.put(
        brainwire_store::txn::Cf::Node,
        id.0.to_be_bytes().to_vec(),
        encoded,
    );
    transaction.commit().unwrap();
    for visibility in [false, true] {
        let result = if visibility {
            restricted(
                &db,
                "MATCH (n:Doc {entity_id:'a'}) RETURN n,n.ACL_Secret,n.acl_allow",
            )
            .unwrap()
        } else {
            execute(
                &db,
                "MATCH (n:Doc {entity_id:'a'}) RETURN n,n.ACL_Secret,n.acl_allow",
                &BTreeMap::new(),
            )
            .unwrap()
        };
        let Out::Node(node) = &result.rows[0][0] else {
            panic!("node")
        };
        assert!(node
            .props
            .keys()
            .all(|key| !key.to_ascii_lowercase().starts_with("acl_")));
        assert_eq!(
            &result.rows[0][1..],
            &[Out::Value(Value::Null), Out::Value(Value::Null)]
        );
    }
    assert!(restricted(
        &db,
        "MATCH (n:Doc) WHERE n.ACL_Secret='secret-a' RETURN n.entity_id"
    )
    .unwrap()
    .rows
    .is_empty());
}

#[test]
fn every_csr_analytics_procedure_and_knowledge_rank_uses_only_visible_nodes_and_edges() {
    let (_dir, db) = setup();
    assert_eq!(restricted(&db,"CALL graph.project() YIELD nodeCount,relationshipCount,nonZeroCount RETURN nodeCount,relationshipCount,nonZeroCount").unwrap().rows,vec![vec![int(3),int(2),int(2)]]);
    assert!(matches!(restricted(&db,"CALL graph.project() YIELD scannedRelationshipCount RETURN scannedRelationshipCount LIMIT 0"),Err(ExecError::Forbidden(_))));
    assert_eq!(restricted(&db,"CALL graph.degree() YIELD node,outDegree,inDegree RETURN node.entity_id,outDegree,inDegree").unwrap().rows,vec![vec![string("a"),int(1),int(1)],vec![string("b"),int(1),int(1)],vec![string("document"),int(0),int(0)]]);
    for name in ["graph.wcc", "graph.scc"] {
        assert_eq!(restricted(&db,&format!("CALL {name}() YIELD node,component,size RETURN node.entity_id,component.entity_id,size")).unwrap().rows,vec![vec![string("a"),string("a"),int(2)],vec![string("b"),string("a"),int(2)],vec![string("document"),string("document"),int(1)]]);
    }
    assert_eq!(restricted(&db,"CALL graph.reachable({source:'a',sourceLabel:'Doc'}) YIELD node,depth RETURN node.entity_id,depth").unwrap().rows,vec![vec![string("a"),int(0)],vec![string("b"),int(1)]]);
    assert_eq!(restricted(&db,"CALL graph.shortestPath({source:'a',sourceLabel:'Doc',target:'b',targetLabel:'Doc'}) YIELD node,depth RETURN node.entity_id,depth").unwrap().rows,vec![vec![string("a"),int(0)],vec![string("b"),int(1)]]);
    let page=restricted(&db,"CALL graph.pagerank({weightProperty:'weight'}) YIELD node,score RETURN node.entity_id,score").unwrap();
    assert_eq!(page.rows.len(), 3);
    assert!(page
        .rows
        .iter()
        .all(|row| ![string("hidden"), string("missing"), string("denied")].contains(&row[0])));
    assert_eq!(restricted(&db,"CALL knowledge.rank({label:'Doc',term:'needle',topK:1}) YIELD node,score RETURN node.entity_id").unwrap().rows,vec![vec![string("b")]]);
    for id in ["hidden", "not-found"] {
        let error = restricted(
            &db,
            &format!(
                "CALL graph.reachable({{source:'{id}',sourceLabel:'Doc'}}) YIELD node RETURN node"
            ),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "semantic error: source node not found");
        let error=restricted(&db,&format!("CALL graph.shortestPath({{source:'a',sourceLabel:'Doc',target:'{id}',targetLabel:'Doc'}}) YIELD node RETURN node")).unwrap_err();
        assert_eq!(error.to_string(), "semantic error: target node not found");
    }
}

#[test]
fn policy_is_trusted_immutable_and_rejects_all_mutations_before_execution() {
    let (_dir, db) = setup();
    for query in ["CREATE (n:Doc {entity_id:'new'})","MERGE (n:Doc {entity_id:'new'})","MATCH (n:Doc {entity_id:'a'}) SET n.title='changed'","MATCH (n:Doc {entity_id:'a'}) DETACH DELETE n","CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:['bob']}) YIELD updated RETURN updated LIMIT 0"] {
        let mut tx=QueryTransaction::begin(&db).unwrap().with_read_visibility(policy()).unwrap();assert!(matches!(tx.run(query,&BTreeMap::new()),Err(ExecError::Forbidden(_))),"{query}");assert_eq!(tx.state(),QueryTransactionState::Failed);
    }
    assert_eq!(
        execute(
            &db,
            "MATCH (n:Doc {entity_id:'a'}) RETURN n.title",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![string("A")]]
    );
    let attached = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(policy())
        .unwrap();
    assert!(attached
        .with_read_visibility(ReadVisibility {
            allow_all: true,
            ..Default::default()
        })
        .is_err());
    let mut used = QueryTransaction::begin(&db).unwrap();
    used.run("RETURN 1", &BTreeMap::new()).unwrap();
    assert!(used.with_read_visibility(policy()).is_err());
    let invalid = ReadVisibility {
        principals: vec![String::new()],
        ..Default::default()
    };
    assert!(QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(invalid)
        .is_err());
}

#[test]
fn stamp_requires_both_privileges_validates_whole_metadata_and_is_transactional() {
    let (dir, db) = setup();
    let query="CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:['bob'],connectorIds:['c1'],sourceTable:'clean/table',sources:['s1']}) YIELD updated AS ok RETURN ok";
    for access in [
        QueryAccess {
            write: false,
            instance_admin: true,
        },
        QueryAccess {
            write: true,
            instance_admin: false,
        },
        QueryAccess {
            write: false,
            instance_admin: false,
        },
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        assert!(matches!(
            tx.run_with_access(query, &BTreeMap::new(), access),
            Err(ExecError::Forbidden(_))
        ));
    }
    for options in [
        "{label:'Doc',entityId:'a',allow:[1]}",
        "{label:'Doc',entityId:'a',allow:['']}",
        "{label:'Doc',entityId:'a',sourceTable:''}",
        "{label:'Doc',entityId:'a',unknown:[]}",
        "{label:'Doc',entityId:'a',deny:null}",
    ] {
        let query = format!("CALL db.setNodeAcl({options}) YIELD updated RETURN updated LIMIT 0");
        assert!(execute(&db, &query, &BTreeMap::new()).is_err(), "{query}");
    }
    assert!(execute(&db,
        "CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:['bob']}) YIELD updated RETURN unknown LIMIT 0",
        &BTreeMap::new()).is_err());
    let too_many = BTreeMap::from([(
        "allow".into(),
        Value::List((0..5001).map(|_| Value::Str("alice".into())).collect()),
    )]);
    assert!(execute(&db,
        "CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:$allow}) YIELD updated RETURN updated LIMIT 0",
        &too_many).is_err());
    let null_source = execute(&db,
        "CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:['alice'],sourceTable:null}) YIELD updated RETURN updated",
        &BTreeMap::new()).unwrap();
    assert_eq!(null_source.rows, vec![vec![Out::Value(Value::Bool(true))]]);
    let mut old = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(policy())
        .unwrap();
    let mut writer = QueryTransaction::begin(&db).unwrap();
    assert_eq!(
        writer.run(query, &BTreeMap::new()).unwrap().rows,
        vec![vec![Out::Value(Value::Bool(true))]]
    );
    writer.rollback();
    drop(writer);
    assert_eq!(
        restricted(&db, "MATCH (n:Doc {entity_id:'a'}) RETURN n.entity_id")
            .unwrap()
            .rows,
        vec![vec![string("a")]]
    );
    let mut writer = QueryTransaction::begin(&db).unwrap();
    writer.run(query, &BTreeMap::new()).unwrap();
    assert!(writer.commit().unwrap().is_some());
    drop(writer);
    assert_eq!(
        old.run(
            "MATCH (n:Doc {entity_id:'a'}) RETURN n.entity_id",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![string("a")]]
    );
    old.rollback();
    drop(old);
    assert!(
        restricted(&db, "MATCH (n:Doc {entity_id:'a'}) RETURN n.entity_id")
            .unwrap()
            .rows
            .is_empty()
    );
    drop(db);
    let reopened = open(dir.path());
    assert!(restricted(
        &reopened,
        "MATCH (n:Doc {entity_id:'a'}) RETURN n.entity_id"
    )
    .unwrap()
    .rows
    .is_empty());
}

#[test]
fn authoritative_document_grants_override_allow_all_when_actual_document_id_is_present() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            allow_all: true,
            knowledge_grants: Some(vec![]),
            ..Default::default()
        })
        .unwrap();
    assert!(tx
        .run(
            "MATCH (n:Doc {entity_id:'document'}) RETURN n.entity_id",
            &BTreeMap::new()
        )
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        tx.run("MATCH (n:Doc) RETURN count(*)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![int(5)]]
    );
}

#[test]
fn hidden_payload_is_not_decoded_for_visibility_only_reads() {
    use brainwire_cypher::{ExecutionLimits, QueryMemoryPool};
    use std::sync::Arc;
    let (_dir, db) = setup();
    execute(
        &db,
        "CREATE (n:Doc {entity_id:'large-hidden',payload:$payload})",
        &BTreeMap::from([("payload".into(), Value::Str("x".repeat(16_384)))]),
    )
    .unwrap();
    stamp(&db, "large-hidden", vec!["bob"], vec![]);
    let query = "MATCH (n:Doc {entity_id:'large-hidden'}) RETURN n.entity_id";
    let mut small = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 8192,
            timeout: None,
        },
    )
    .unwrap()
    .with_read_visibility(policy())
    .unwrap();
    assert!(small.run(query, &BTreeMap::new()).unwrap().rows.is_empty());
    assert_eq!(small.commit().unwrap(), None);
    let pool = Arc::new(QueryMemoryPool::new(8192));
    let mut pooled =
        QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
            .unwrap()
            .with_read_visibility(policy())
            .unwrap();
    assert!(pooled.run(query, &BTreeMap::new()).unwrap().rows.is_empty());
    assert!(pool.usage().peak_bytes <= 8192);
    assert_eq!(pool.usage().rejections, 0);
    assert_eq!(pooled.commit().unwrap(), None);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn null_heavy_payloads_are_guarded_only_when_the_read_needs_them() {
    use brainwire_cypher::{ExecutionLimits, QueryMemoryPool};
    use std::sync::Arc;
    let (_dir, db) = setup();
    let params = BTreeMap::from([("payload".into(), Value::List(vec![Value::Null; 4096]))]);
    for (id, principal) in [("null-visible", "alice"), ("null-hidden", "bob")] {
        execute(
            &db,
            &format!("CREATE (n:Doc {{entity_id:'{id}',payload:$payload}})"),
            &params,
        )
        .unwrap();
        stamp(&db, id, vec![principal], vec![]);
    }
    for (id, expected) in [
        ("null-visible", vec![vec![string("null-visible")]]),
        ("null-hidden", vec![]),
    ] {
        let query = format!("MATCH (n:Doc {{entity_id:'{id}'}}) RETURN n.entity_id");
        let mut tx = QueryTransaction::begin_with_limits(
            &db,
            ExecutionLimits {
                memory_bytes: 48 * 1024,
                timeout: None,
            },
        )
        .unwrap()
        .with_read_visibility(policy())
        .unwrap();
        assert_eq!(tx.run(&query, &BTreeMap::new()).unwrap().rows, expected);
        assert_eq!(tx.commit().unwrap(), None);
        let pool = Arc::new(QueryMemoryPool::new(48 * 1024));
        let mut tx =
            QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
                .unwrap()
                .with_read_visibility(policy())
                .unwrap();
        assert_eq!(tx.run(&query, &BTreeMap::new()).unwrap().rows, expected);
        assert_eq!(tx.commit().unwrap(), None);
        assert_eq!(pool.usage().reserved_bytes, 0);
        assert_eq!(pool.usage().rejections, 0);
    }
    // Owning the visible payload or whole record still requires predecode admission.
    for result in ["n.payload", "n"] {
        let query = format!("MATCH (n:Doc {{entity_id:'null-visible'}}) RETURN {result}");
        let mut tx = QueryTransaction::begin_with_limits(
            &db,
            ExecutionLimits {
                memory_bytes: 48 * 1024,
                timeout: None,
            },
        )
        .unwrap()
        .with_read_visibility(policy())
        .unwrap();
        assert!(matches!(
            tx.run(&query, &BTreeMap::new()),
            Err(ExecError::MemoryLimit { .. })
        ));
        assert_eq!(tx.state(), QueryTransactionState::Failed);
        let pool = Arc::new(QueryMemoryPool::new(48 * 1024));
        let mut tx =
            QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
                .unwrap()
                .with_read_visibility(policy())
                .unwrap();
        assert!(matches!(
            tx.run(&query, &BTreeMap::new()),
            Err(ExecError::MemoryPoolExhausted { .. })
        ));
        assert_eq!(pool.usage().reserved_bytes, 0);
        assert_eq!(pool.usage().rejections, 1);
    }
    // Some(empty) remains authoritative and retains the guarded document path.
    let mut grants = policy();
    grants.knowledge_grants = Some(vec![]);
    let mut tx = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 48 * 1024,
            timeout: None,
        },
    )
    .unwrap()
    .with_read_visibility(grants)
    .unwrap();
    assert!(matches!(
        tx.run(
            "MATCH (n:Doc {entity_id:'null-visible'}) RETURN n.entity_id",
            &BTreeMap::new()
        ),
        Err(ExecError::MemoryLimit { .. })
    ));
}

#[test]
fn acl_stamp_and_identity_read_do_not_decode_null_heavy_payloads() {
    use brainwire_cypher::ExecutionLimits;
    let (_dir, db) = setup();
    let catalog = db.catalog().unwrap().unwrap();
    let mut offline = db.begin();
    brainwire_store::graph::merge_node(
        &mut offline,
        &catalog,
        "Doc",
        "stamp-null-heavy",
        &BTreeMap::from([("payload".into(), Value::List(vec![Value::Null; 4096]))]),
    )
    .unwrap();
    offline.commit().unwrap();
    let mut admin = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 16 * 1024,
            timeout: None,
        },
    )
    .unwrap();
    let result = admin.run_with_access(
        "CALL db.setNodeAcl({label:'Doc',entityId:'stamp-null-heavy',allow:['alice']}) YIELD updated RETURN updated",
        &BTreeMap::new(), QueryAccess { write: true, instance_admin: true },
    ).unwrap();
    assert_eq!(result.rows, vec![vec![Out::Value(Value::Bool(true))]]);
    assert!(admin.commit().unwrap().is_some());
    let mut reader = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 48 * 1024,
            timeout: None,
        },
    )
    .unwrap()
    .with_read_visibility(policy())
    .unwrap();
    assert_eq!(
        reader
            .run(
                "MATCH (n:Doc {entity_id:'stamp-null-heavy'}) RETURN n.entity_id",
                &BTreeMap::new()
            )
            .unwrap()
            .rows,
        vec![vec![string("stamp-null-heavy")]]
    );
    assert_eq!(reader.commit().unwrap(), None);
}

#[test]
fn malformed_stored_node_acl_stamp_fails_and_rolls_back_all_prior_metadata_changes() {
    let (_dir, db) = setup();
    let catalog = db.catalog().unwrap().unwrap();
    let ty = catalog.node_type("Doc").unwrap();
    let mut offline = db.begin();
    let missing =
        brainwire_store::graph::find_by_entity_id(&offline, "missing", Some(ty.id)).unwrap()[0];
    let allowed = brainwire_store::graph::find_by_entity_id(&offline, "a", Some(ty.id)).unwrap()[0];
    assert!(brainwire_store::graph::get_node_acl(&offline, missing)
        .unwrap()
        .is_none());
    offline.put(
        brainwire_store::txn::Cf::Node,
        missing.0.to_be_bytes().to_vec(),
        Vec::new(),
    );
    offline.commit().unwrap();
    let mut admin = QueryTransaction::begin(&db).unwrap();
    admin.run(
        "CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:['bob']}) YIELD updated RETURN updated",
        &BTreeMap::new(),
    ).unwrap();
    assert!(admin.run(
        "CALL db.setNodeAcl({label:'Doc',entityId:'missing',allow:['alice']}) YIELD updated RETURN updated",
        &BTreeMap::new(),
    ).is_err());
    assert_eq!(admin.state(), QueryTransactionState::Failed);
    assert!(admin.commit().is_err());
    let inspected = db.begin();
    assert!(brainwire_store::graph::get_node_acl(&inspected, missing)
        .unwrap()
        .is_none());
    assert_eq!(
        brainwire_store::graph::get_node_acl(&inspected, allowed)
            .unwrap()
            .unwrap()
            .allow,
        vec!["alice"]
    );
}

fn acl_rows(rows: &[(&str, &str)]) -> BTreeMap<String, Value> {
    let rows = rows
        .iter()
        .map(|(id, principal)| {
            Value::Map(BTreeMap::from([
                ("label".into(), Value::Str("Doc".into())),
                ("entityId".into(), Value::Str((*id).into())),
                (
                    "allow".into(),
                    Value::List(vec![Value::Str((*principal).into())]),
                ),
                (
                    "connectorIds".into(),
                    Value::List(vec![Value::Str("c1".into())]),
                ),
            ]))
        })
        .collect();
    BTreeMap::from([("rows".into(), Value::List(rows))])
}

const BATCH: &str = "CALL db.setNodeAcls($rows) YIELD updated RETURN updated";

#[test]
fn batch_stamp_replaces_many_acls_in_one_call() {
    let (_dir, db) = setup();
    let output = execute(
        &db,
        BATCH,
        &acl_rows(&[("hidden", "alice"), ("missing", "alice"), ("a", "bob")]),
    )
    .unwrap();
    assert_eq!(output.rows, vec![vec![Out::Value(Value::Int(3))]]);
    let visible = restricted(&db, "MATCH (n:Doc) RETURN n.entity_id").unwrap();
    let mut ids: Vec<_> = visible
        .rows
        .into_iter()
        .map(|row| format!("{:?}", row[0]))
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        ["b", "document", "hidden", "missing"]
            .map(|id| format!("{:?}", Out::Value(Value::Str(id.into()))))
    );
    assert_eq!(
        execute(&db, BATCH, &acl_rows(&[])).unwrap().rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
}

#[test]
fn batch_stamp_is_atomic_privileged_and_validated() {
    let (_dir, db) = setup();
    // One missing node or invalid entry rolls back every stamp in the call.
    for rows in [
        acl_rows(&[("hidden", "alice"), ("nope", "alice")]),
        acl_rows(&[("hidden", "alice"), ("a", "")]),
    ] {
        assert!(execute(&db, BATCH, &rows).is_err());
    }
    let visible = restricted(&db, "MATCH (n:Doc {entity_id:'hidden'}) RETURN count(*)").unwrap();
    assert_eq!(visible.rows, vec![vec![Out::Value(Value::Int(0))]]);
    for bad in [
        BTreeMap::from([("rows".into(), Value::Str("x".into()))]),
        BTreeMap::from([("rows".into(), Value::List(vec![Value::Int(1)]))]),
    ] {
        assert!(execute(&db, BATCH, &bad).is_err());
    }
    let mut tx = QueryTransaction::begin(&db).unwrap();
    assert!(matches!(
        tx.run_with_access(
            BATCH,
            &acl_rows(&[("hidden", "alice")]),
            QueryAccess {
                write: true,
                instance_admin: false
            }
        ),
        Err(ExecError::Forbidden(_))
    ));
    assert!(restricted(&db, BATCH).is_err());
}

#[test]
fn an_administrator_reads_back_node_acls_and_a_restricted_reader_cannot() {
    let (_dir, db) = setup();
    execute(&db, BATCH, &acl_rows(&[("hidden", "alice")])).unwrap();
    let refs = |ids: &[&str]| {
        BTreeMap::from([(
            "refs".into(),
            Value::List(
                ids.iter()
                    .map(|id| {
                        Value::Map(BTreeMap::from([
                            ("label".into(), Value::Str("Doc".into())),
                            ("entityId".into(), Value::Str((*id).into())),
                        ]))
                    })
                    .collect(),
            ),
        )])
    };
    const READ: &str = "CALL db.nodeAcls($refs) YIELD entityId, allow, deny, sourceTable \
                        RETURN entityId, allow, deny, sourceTable";
    let rows = execute(&db, READ, &refs(&["hidden", "nope"])).unwrap().rows;
    assert_eq!(rows.len(), 1, "a node that does not exist answers nothing");
    assert_eq!(rows[0][0], string("hidden"));
    assert_eq!(
        rows[0][1],
        Out::Value(Value::List(vec![Value::Str("alice".into())]))
    );
    assert_eq!(rows[0][2], Out::Value(Value::List(vec![])));
    let mut tx = QueryTransaction::begin(&db).unwrap();
    assert!(matches!(
        tx.run_with_access(
            READ,
            &refs(&["hidden"]),
            QueryAccess {
                write: true,
                instance_admin: false
            }
        ),
        Err(ExecError::Forbidden(_))
    ));
    assert!(restricted(&db, READ).is_err());
}

#[test]
fn acl_lists_of_5000_entries_round_trip_and_5001_are_rejected() {
    let (_dir, db) = setup();
    let grants = |n: usize| -> Vec<String> { (0..n).map(|i| format!("principal-{i}")).collect() };
    let set = |n: usize| {
        let params = BTreeMap::from([(
            "allow".into(),
            Value::List(grants(n).into_iter().map(Value::Str).collect()),
        )]);
        execute(&db, "CALL db.setNodeAcl({label:'Doc',entityId:'hidden',allow:$allow}) YIELD updated RETURN updated", &params)
    };
    let error = set(5001).unwrap_err().to_string();
    assert!(error.contains("at most 5000"), "{error}");
    set(5000).unwrap();
    let refs = BTreeMap::from([(
        "refs".into(),
        Value::List(vec![Value::Map(BTreeMap::from([
            ("label".into(), Value::Str("Doc".into())),
            ("entityId".into(), Value::Str("hidden".into())),
        ]))]),
    )]);
    let rows = execute(
        &db,
        "CALL db.nodeAcls($refs) YIELD allow RETURN allow",
        &refs,
    )
    .unwrap()
    .rows;
    let Out::Value(Value::List(stored)) = &rows[0][0] else {
        panic!("allow is a list")
    };
    assert_eq!(stored.len(), 5000);
    for who in ["principal-0", "principal-4999"] {
        let mut tx = QueryTransaction::begin(&db)
            .unwrap()
            .with_read_visibility(ReadVisibility {
                principals: vec![who.into()],
                ..Default::default()
            })
            .unwrap();
        let out = tx
            .run_with_access(
                "MATCH (n:Doc) RETURN n.entity_id",
                &BTreeMap::new(),
                QueryAccess {
                    write: false,
                    instance_admin: false,
                },
            )
            .unwrap();
        assert_eq!(out.rows, vec![vec![string("hidden")]]);
    }
}

#[test]
fn reserved_acl_properties_are_semantic_client_errors() {
    let (_dir, db) = setup();
    for q in [
        "CREATE (n:Doc {entity_id:'r', acl_allow:['a']})",
        "MATCH (n:Doc {entity_id:'a'}) SET n.ACL_deny = ['a']",
    ] {
        let error = execute(&db, q, &BTreeMap::new()).unwrap_err();
        assert!(matches!(error, ExecError::Semantic(_)), "{q}: {error:?}");
    }
}

#[test]
fn allow_all_count_keeps_the_fast_path_without_reading_large_node_payloads() {
    use brainwire_cypher::ExecutionLimits;
    let (_dir, db) = setup();
    execute(
        &db,
        "CREATE (:Doc {entity_id:'large', payload:$payload})",
        &BTreeMap::from([("payload".into(), Value::Str("x".repeat(65_536)))]),
    )
    .unwrap();
    let mut tx = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 8192,
            timeout: None,
        },
    )
    .unwrap()
    .with_read_visibility(ReadVisibility {
        allow_all: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        tx.run("MATCH (n:Doc) RETURN count(n)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![int(7)]]
    );
    assert_eq!(tx.commit().unwrap(), None);
}

#[test]
fn allow_all_remains_read_only_and_cannot_read_private_acl_metadata() {
    let (_dir, db) = setup();
    for query in [
        "CREATE (:Doc {entity_id:'new'})",
        "MATCH (n:Doc {entity_id:'a'}) SET n.title='changed'",
        "CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:['bob']}) YIELD updated RETURN updated",
        "CALL db.nodeAcls([{label:'Doc',entityId:'a'}]) YIELD allow RETURN allow",
        "CALL graph.project() YIELD scannedRelationshipCount RETURN scannedRelationshipCount LIMIT 0",
        "RETURN 1 AS ignored UNION CALL graph.project() YIELD scannedRelationshipCount RETURN scannedRelationshipCount AS ignored",
        "RETURN 1 AS ignored UNION CALL db.nodeAcls([{label:'Doc',entityId:'a'}]) YIELD allow RETURN allow AS ignored",
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap()
            .with_read_visibility(ReadVisibility { allow_all: true, ..Default::default() }).unwrap();
        assert!(matches!(tx.run(query, &BTreeMap::new()), Err(ExecError::Forbidden(_))), "{query}");
        assert_eq!(tx.state(), QueryTransactionState::Failed);
    }
}

#[test]
fn allow_all_with_denial_or_authoritative_grants_stays_filtered() {
    let (_dir, db) = setup();
    for (visibility, expected) in [
        (
            ReadVisibility {
                allow_all: true,
                deny_all: true,
                ..Default::default()
            },
            0,
        ),
        (
            ReadVisibility {
                allow_all: true,
                knowledge_grants: Some(vec![]),
                ..Default::default()
            },
            5,
        ),
    ] {
        let mut tx = QueryTransaction::begin(&db)
            .unwrap()
            .with_read_visibility(visibility)
            .unwrap();
        assert_eq!(
            tx.run("MATCH (n:Doc) RETURN count(n)", &BTreeMap::new())
                .unwrap()
                .rows,
            vec![vec![int(expected)]]
        );
    }
}

#[test]
fn parallel_acl_scans_preserve_filtered_aggregations_top_k_row_order_and_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let schema: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Doc","properties":["group","score"]}],"edges":[{"type":"LINK","from":{"label":"Doc"},"to":{"label":"Doc"}}]}"#,
    )
    .unwrap();
    db.apply_ontology(&schema).unwrap();
    // Exceed the parallel scan threshold without relying on elapsed time.
    for start in (0..50_100).step_by(1_000) {
        let end = (start + 1_000).min(50_100);
        let mut tx = QueryTransaction::begin(&db).unwrap();
        tx.run(&format!("UNWIND range({start}, {}) AS i CREATE (:Doc {{entity_id:'doc-' + i, group:i % 2, score:i}})", end - 1), &BTreeMap::new()).unwrap();
        tx.commit().unwrap();
        let catalog = db.catalog().unwrap().unwrap();
        let ty = catalog.node_type("Doc").unwrap();
        let mut tx = db.begin();
        for i in start..end {
            let id =
                brainwire_store::graph::find_by_entity_id(&tx, &format!("doc-{i}"), Some(ty.id))
                    .unwrap()[0];
            brainwire_store::graph::set_node_acl(
                &mut tx,
                id,
                &brainwire_core::acl::NodeAcl {
                    allow: vec![if i % 2 == 0 { "alice" } else { "bob" }.into()],
                    ..Default::default()
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    // Spread visible and hidden endpoints across the scanned label's ID range.
    for source in [0, 10_000, 20_000, 30_000, 40_000, 50_000] {
        execute(&db, &format!(
            "MATCH (a:Doc {{entity_id:'doc-{source}'}}), (hidden:Doc {{entity_id:'doc-{}'}}), (visible:Doc {{entity_id:'doc-{}'}}) CREATE (a)-[:LINK]->(hidden), (a)-[:LINK]->(visible), (hidden)-[:LINK]->(visible)",
            source + 1, source + 2), &BTreeMap::new()).unwrap();
    }
    assert_eq!(
        execute(
            &db,
            "MATCH (n:Doc)-[:LINK]->(m:Doc) RETURN count(*)",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![int(18)]]
    );
    assert_eq!(
        restricted(&db, "MATCH (n:Doc)-[:LINK]->(m:Doc) RETURN count(*)")
            .unwrap()
            .rows,
        vec![vec![int(6)]]
    );
    let traversed = restricted(
        &db,
        "MATCH (n:Doc)-[:LINK]->(m:Doc) RETURN n.entity_id, m.entity_id",
    )
    .unwrap();
    let expected: Vec<_> = [0, 10_000, 20_000, 30_000, 40_000, 50_000]
        .into_iter()
        .map(|source| {
            vec![
                string(&format!("doc-{source}")),
                string(&format!("doc-{}", source + 2)),
            ]
        })
        .collect();
    assert_eq!(traversed.rows, expected);
    let queries = [
        (
            "MATCH (n:Doc) RETURN n.group AS g, count(*) AS n, sum(n.score) AS s ORDER BY g",
            vec![vec![int(0), int(25_050), int(627_477_450)]],
        ),
        (
            "MATCH (n:Doc) RETURN n.entity_id AS id ORDER BY n.score DESC LIMIT 2",
            vec![vec![string("doc-50098")], vec![string("doc-50096")]],
        ),
    ];
    for (query, expected) in queries {
        assert_eq!(restricted(&db, query).unwrap().rows, expected, "{query}");
    }
    let output = restricted(&db, "MATCH (n:Doc) WHERE n.score < 8 RETURN n.entity_id").unwrap();
    assert_eq!(
        output.rows,
        vec![
            vec![string("doc-0")],
            vec![string("doc-2")],
            vec![string("doc-4")],
            vec![string("doc-6")]
        ]
    );
}
