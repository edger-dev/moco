//! A node's own recipes: the manual entries of the node's manifest, listed
//! and started by name from any host — through the gate, as an ad-hoc start
//! of the same argv would be.
//!
//! implements: 7735b6fcb04483f5bb958b825bd6e7057146db208d5f5e5b9da25d4ad9dcff01
//! implements: a457400202f392b8f3cb8c1266e0e7a8b4ca92ed3cff07e1fcd54e1952442226

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use moco_job::scope::{Caller, Scope};
use moco_job::wire::{
    self, RecipesReply, RecipesRequest, StartNamedRequest, StartReply, WireCaller,
};
use moco_job::{JobRegistry, JobStatus, MANIFEST_FILE, NodePolicy, RuleSet, SeedConfig, Verdict};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "moco-recipes-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A node named `pc` whose own manifest is `manifest` (`{cwd}` stands for a
/// directory the jobs may run in).
fn node(name: &str, manifest: &str) -> (JobRegistry, PathBuf) {
    let d = dir(name);
    write_manifest(&d, manifest);
    let reg = JobRegistry::ungoverned()
        .unwrap()
        .with_dir(&d)
        .unwrap()
        .with_node("pc");
    (reg, d)
}

fn write_manifest(d: &Path, manifest: &str) {
    std::fs::write(
        d.join(MANIFEST_FILE),
        manifest.replace("{cwd}", &d.display().to_string()),
    )
    .unwrap();
}

/// A workspace whose own manifest declares nothing.
fn session() -> (Caller, PathBuf) {
    let ws = dir("workspace");
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    std::fs::write(ws.join(MANIFEST_FILE), "proc ()").unwrap();
    (Caller::Scoped(Scope::resolve(&ws)), ws)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn settled(reg: &JobRegistry, id: &moco_job::JobId) -> JobStatus {
    for _ in 0..200 {
        let status = reg.status_of(id).unwrap();
        if status.is_terminal() || status == JobStatus::PendingApproval {
            return status;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    reg.status_of(id).unwrap()
}

const MIXED: &str = r#"proc (
  {name housekeeping, argv (sleep 30), cwd "{cwd}", autostart @Boot}
  {name refresh, argv (echo refreshed), cwd "{cwd}", about "refresh the session cookie"}
  {name here, argv (echo here), cwd "{cwd}", hosts (pc)}
  {name elsewhere, argv (echo there), cwd "{cwd}", hosts (laptop)}
)"#;

/// The recipes are the manual entries this node may run, each with what it
/// runs, where, and what it is for.
#[test]
fn recipes_are_the_manual_entries_this_node_may_run() {
    let (reg, d) = node("list", MIXED);
    let recipes = reg.recipes().unwrap();
    let names: Vec<&str> = recipes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        ["refresh", "here"],
        "no boot entry, none for another host"
    );
    let refresh = &recipes[0];
    assert_eq!(refresh.argv, argv(&["echo", "refreshed"]));
    assert_eq!(refresh.cwd, d.display().to_string());
    assert_eq!(refresh.about, "refresh the session cookie");

    let (empty, _) = node("none", "proc ()");
    assert!(empty.recipes().unwrap().is_empty());
    let bare = JobRegistry::ungoverned()
        .unwrap()
        .with_dir(dir("no-manifest"))
        .unwrap();
    assert!(
        bare.recipes().unwrap().is_empty(),
        "no manifest, no recipes"
    );
}

/// A recipe starts from the node's manifest, not the caller's workspace, in
/// the entry's directory, owned by the caller — who may then stop it.
#[test]
fn a_recipe_starts_by_name_owned_by_its_caller() {
    let (reg, d) = node("start", MIXED);
    let (caller, ws) = session();
    let id = reg.start_recipe("refresh", &caller).unwrap();
    assert_eq!(reg.name_of(&id).as_deref(), Some("node:refresh"));
    assert_eq!(reg.scope_of(&id), Some(Scope::resolve(&ws)));
    assert_eq!(reg.cwd_of(&id), Some(d.clone()));
    assert_eq!(reg.argv_of(&id), Some(argv(&["echo", "refreshed"])));
    assert_eq!(settled(&reg, &id), JobStatus::Done { code: 0 });

    let long = dir("long");
    write_manifest(&long, r#"proc ({name nap, argv (sleep 30), cwd "{cwd}"})"#);
    let reg = JobRegistry::ungoverned()
        .unwrap()
        .with_dir(&long)
        .unwrap()
        .with_node("pc");
    let id = reg.start_recipe("nap", &caller).unwrap();
    reg.kill(&id, &caller)
        .expect("the caller may stop its recipe job");

    // The console's start is the node's own.
    let id = reg.start_recipe("nap", &Caller::Console).unwrap();
    assert_eq!(reg.scope_of(&id), Some(Scope::System));
    let _ = reg.kill(&id, &Caller::Console);
}

/// A recipe names a command; it does not grant one. The gate decides as it
/// would for an ad-hoc start of the same argv.
#[test]
fn a_recipe_passes_the_gate() {
    let d = dir("gate");
    write_manifest(
        &d,
        r#"proc (
  {name ok, argv (echo ok), cwd "{cwd}"}
  {name ask, argv (echo ask), cwd "{cwd}"}
  {name nope, argv (echo nope), cwd "{cwd}"}
)"#,
    );
    let rules = RuleSet::from_seed(SeedConfig {
        allow: vec![argv(&["echo", "ok"])],
        deny: vec![argv(&["echo", "nope"])],
    });
    let reg = JobRegistry::with_policy(
        NodePolicy::new(rules, &d).with_approval_timeout(Duration::from_secs(5)),
    )
    .unwrap()
    .with_dir(&d)
    .unwrap()
    .with_node("pc");
    let (caller, _) = session();

    let ok = reg.start_recipe("ok", &caller).unwrap();
    assert_eq!(settled(&reg, &ok), JobStatus::Done { code: 0 });
    assert_eq!(reg.verdict_of(&ok), Some(Verdict::SeedAllow));

    let ask = reg.start_recipe("ask", &caller).unwrap();
    assert_eq!(settled(&reg, &ask), JobStatus::PendingApproval);
    let _ = reg.kill(&ask, &Caller::Console);

    let nope = reg.start_recipe("nope", &caller);
    let denied = match nope {
        Err(_) => true,
        Ok(id) => matches!(settled(&reg, &id), JobStatus::Denied { .. }),
    };
    assert!(denied, "a denied argv stays denied as a recipe");
}

/// What is not a recipe here is refused, naming the entry and why.
#[test]
fn what_is_not_a_recipe_here_is_refused_with_a_reason() {
    let (reg, _d) = node(
        "refusals",
        r#"proc (
  {name housekeeping, argv (sleep 30), cwd "{cwd}", autostart @Boot}
  {name rootless, argv (echo hi)}
  {name elsewhere, argv (echo there), cwd "{cwd}", hosts (laptop)}
  {name refresh, argv (echo refreshed), cwd "{cwd}"}
)"#,
    );
    let (caller, _) = session();
    let text = |name: &str| reg.start_recipe(name, &caller).unwrap_err().to_string();

    let unknown = text("missing");
    assert!(
        unknown.contains("missing") && unknown.contains("refresh"),
        "{unknown}"
    );
    let boot = text("housekeeping");
    assert!(
        boot.contains("housekeeping") && boot.contains("manual"),
        "{boot}"
    );
    let rootless = text("rootless");
    assert!(
        rootless.contains("rootless") && rootless.contains("cwd"),
        "{rootless}"
    );
    let elsewhere = text("elsewhere");
    assert!(
        elsewhere.contains("elsewhere") && elsewhere.contains("pc"),
        "{elsewhere}"
    );
}

/// Restarting a recipe job reads the node's manifest again — not the
/// caller's workspace, which does not declare it.
#[test]
fn restarting_a_recipe_job_reads_the_node_manifest_again() {
    let (reg, d) = node(
        "restart",
        r#"proc ({name nap, argv (sleep 30), cwd "{cwd}"})"#,
    );
    let (caller, _) = session();
    let first = reg.start_recipe("nap", &caller).unwrap();
    write_manifest(&d, r#"proc ({name nap, argv (sleep 31), cwd "{cwd}"})"#);
    let second = reg.restart(&first, &caller).unwrap();
    assert_eq!(reg.name_of(&second).as_deref(), Some("node:nap"));
    assert_eq!(reg.argv_of(&second), Some(argv(&["sleep", "31"])));
    let _ = reg.kill(&second, &caller);
}

/// Over the wire: `recipes` lists them; a named start that says
/// `node_recipe` starts one; a caller that does not say so (an older one)
/// still means its workspace's manifest.
#[test]
fn recipes_cross_the_wire() {
    let (reg, _d) = node("wire", MIXED);
    let (_, ws) = session();
    let session = WireCaller::Session {
        cwd: ws.display().to_string(),
    };

    let reply: RecipesReply = wire::decode(
        &wire::dispatch(&reg, "recipes", &wire::encode(&RecipesRequest {}).unwrap()).unwrap(),
    )
    .unwrap();
    let names: Vec<&str> = reply.recipes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["refresh", "here"]);
    assert_eq!(reply.recipes[0].about, "refresh the session cookie");

    let start = |node_recipe: bool| {
        wire::dispatch(
            &reg,
            "start_named",
            &wire::encode(&StartNamedRequest {
                name: "refresh".into(),
                caller: session.clone(),
                node_recipe,
            })
            .unwrap(),
        )
    };
    let started: StartReply = wire::decode(&start(true).unwrap()).unwrap();
    let id = moco_job::JobId(started.id);
    assert_eq!(reg.name_of(&id).as_deref(), Some("node:refresh"));
    assert!(
        start(false).is_err(),
        "without node_recipe the name is the workspace's, which has none"
    );

    // An older caller sends no node_recipe at all.
    let text = String::from_utf8(
        wire::encode(&StartNamedRequest {
            name: "refresh".into(),
            caller: session.clone(),
            node_recipe: false,
        })
        .unwrap(),
    )
    .unwrap();
    let older = text
        .replace(", node_recipe false", "")
        .replace("node_recipe false", "");
    assert!(!older.contains("node_recipe"), "{older}");
    let decoded: StartNamedRequest = wire::decode(older.as_bytes()).unwrap();
    assert!(!decoded.node_recipe);
}
