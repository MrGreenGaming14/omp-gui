//! Golden files for the C++ port, written by `cargo test -p loa-bus --test
//! golden -- --ignored` into `$LOA_GOLDEN_OUT`.

use loa_bus::{Journal, JournalOptions, proto};
use loa_core::{Dom, Draft, ElementKind, NodeId, Value};
use prost::Message as _;
use std::fs;
use std::path::{Path, PathBuf};

fn out(sub: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").expect("LOA_GOLDEN_OUT")).join(sub);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
#[ignore = "writes golden files; run with LOA_GOLDEN_OUT set"]
fn write_golden_journal_and_bus() {
    let journal_out = out("journal");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal");
    for rel in ["f/journal.loaj", "s/journal.loaj", "s/snap-00000000000000000016.loas", "s/snap-00000000000000000032.loas"] {
        fs::create_dir_all(journal_out.join(rel).parent().unwrap()).unwrap();
        fs::copy(fixtures.join(rel), journal_out.join(rel)).unwrap();
    }

    // Every Op and every Value variant.
    let mut dom = Dom::new();
    let mut patches = Vec::new();
    let mut commit = |dom: &mut Dom, d: Draft| {
        let p = dom.commit(d, 1_700_000_000_000 + patches.len() as i64).unwrap();
        patches.push(p.clone());
        p.node
    };
    let attrs = [
        ("id", Value::Str("golden".into())),
        ("harness", Value::Str("omp".into())),
        ("node", Value::Str("k".into())),
        ("created", Value::Int(1)),
    ];
    let root = commit(&mut dom, Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""));
    let ext_attrs = [
        ("harness", Value::Str("x".into())),
        ("x-bool", Value::Bool(true)),
        ("x-int", Value::Int(-42)),
        ("x-float", Value::Float(0.5)),
        ("x-str", Value::Str("naïve \u{1F600}".into())),
        ("x-bytes", Value::Bytes((0..=255).collect())),
        ("x-list", Value::List(vec![Value::Bool(false), Value::Int(7), Value::Str("s".into())])),
    ];
    let ext = commit(&mut dom, Draft::insert(root, ElementKind::Ext, ext_attrs, "raw"));
    commit(&mut dom, Draft::insert_at(root, 0, ElementKind::Ext, [("harness", Value::Str("y".into()))], ""));
    commit(&mut dom, Draft::set(ext, "x-n", Value::Float(-1.25)));
    commit(&mut dom, Draft::clear(ext, "x-bool"));
    commit(&mut dom, Draft::append(ext, " tok"));
    let gone = commit(&mut dom, Draft::insert(root, ElementKind::Ext, [("harness", Value::Str("z".into()))], ""));
    commit(&mut dom, Draft::remove(gone));

    let dir = journal_out.join("g");
    let _ = fs::remove_dir_all(&dir);
    let mut journal = Journal::create(&dir, "g", JournalOptions { snapshot_every: 0, keep_snapshots: 2 }).unwrap();
    let mut replay = Dom::new();
    for p in &patches {
        replay.apply(p).unwrap();
        journal.append_applied(p, &replay).unwrap();
    }
    journal.sync().unwrap();
    drop(journal);
    let _ = fs::remove_file(dir.join("lock"));
    assert_eq!(replay, dom);

    let bus = out("bus");
    for (i, p) in patches.iter().enumerate() {
        fs::write(bus.join(format!("patch-{:02}.pb", i + 1)), proto::Patch::from(p).encode_to_vec()).unwrap();
    }
    fs::write(bus.join("dom.pb"), proto::Dom::from(&dom).encode_to_vec()).unwrap();
}
