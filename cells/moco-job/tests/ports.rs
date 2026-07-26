//! The node hands out dev ports, and the registry is the reserved set.
//!
//! implements: node-is-the-port-authority
//! implements: node-supplied-argv-tokens

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moco_job::manifest::{MANIFEST_FILE, Manifest};
use moco_job::port::{PortRange, PortRequest};
use moco_job::scope::{Caller, Scope};
use moco_job::{JobRegistry, RecordStore};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "moco-ports-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create");
    dir.canonicalize().expect("canonicalize")
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn workspace(name: &str, manifest: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::create_dir_all(dir.join(".git")).expect("repo");
    std::fs::write(dir.join(MANIFEST_FILE), manifest).expect("manifest");
    dir
}

#[allow(
    clippy::expect_used,
    reason = "test helper: a failure is a broken harness"
)]
fn registry_in(dir: &PathBuf) -> JobRegistry {
    JobRegistry::ungoverned()
        .expect("registry")
        .with_dir(dir)
        .expect("with_dir")
}

/// A small range, so "lowest free" is observable.
fn range() -> PortRange {
    PortRange::new(19100, 19199)
}

/// `auto` hands out a port inside the node's range.
#[test]
fn auto_allocates_inside_the_range() {
    let dir = scratch("in-range");
    let store = RecordStore::open(&dir).expect("store");
    let scope = Scope::workspace("/ws/a");

    let port = moco_job::port::allocate(&store, range(), &scope, Some("dev"), PortRequest::Auto)
        .expect("allocate")
        .expect("auto yields a port");

    assert!((19100..=19199).contains(&port), "got {port}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **Sticky.** The same `(scope, name)` gets its previous port back, so a dev
/// server's URL does not move every time it is restarted.
#[test]
fn auto_is_sticky_for_the_same_scope_and_name() {
    let dir = scratch("sticky");
    let store = RecordStore::open(&dir).expect("store");
    let scope = Scope::workspace("/ws/sticky");

    let first = moco_job::port::allocate(&store, range(), &scope, Some("dev"), PortRequest::Auto)
        .expect("allocate")
        .expect("port");

    // Record it as this declaration's port, then ask again.
    moco_job::port::remember(&store, &scope, "dev", first).expect("remember");

    let second = moco_job::port::allocate(&store, range(), &scope, Some("dev"), PortRequest::Auto)
        .expect("allocate")
        .expect("port");

    assert_eq!(first, second, "the same declaration should keep its port");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two different declarations do not collide, even sharing one registry — which
/// is the case two daemons on one node are in.
#[test]
fn two_live_jobs_get_different_ports() {
    let dir = scratch("distinct");
    let a = workspace(
        "live-a",
        r#"proc ({name dev, argv (sleep 30), port @Auto})"#,
    );
    let b = workspace(
        "live-b",
        r#"proc ({name dev, argv (sleep 30), port @Auto})"#,
    );
    let reg = registry_in(&dir);

    let ja = reg
        .start_named("dev", &Caller::Scoped(Scope::resolve(&a)))
        .expect("a");
    let jb = reg
        .start_named("dev", &Caller::Scoped(Scope::resolve(&b)))
        .expect("b");

    // Both are *live*, so both reserve — which is the only case where sharing
    // would actually collide. (A finished job releases its port immediately;
    // stickiness, not reservation, is what brings it back.)
    assert_ne!(
        reg.port_of(&ja),
        reg.port_of(&jb),
        "two running servers must not be handed the same port"
    );

    let _ = reg.kill(&ja, &Caller::Console);
    let _ = reg.kill(&jb, &Caller::Console);
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A port something else is already listening on is skipped. The live probe is
/// what respects processes the supervisor knows nothing about.
#[test]
fn a_port_held_by_a_stranger_is_skipped() {
    let dir = scratch("stranger");
    let store = RecordStore::open(&dir).expect("store");
    let scope = Scope::workspace("/ws/stranger");

    // Occupy the bottom of the range with something the registry cannot see.
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 19100)).expect("bind squatter");

    let port = moco_job::port::allocate(&store, range(), &scope, Some("dev"), PortRequest::Auto)
        .expect("allocate")
        .expect("port");
    assert_ne!(
        port, 19100,
        "a live listener must be respected even though no record mentions it"
    );

    drop(squatter);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A **fixed** port is handed back as declared, and never invented.
#[test]
fn a_fixed_port_is_used_as_declared() {
    let dir = scratch("fixed");
    let store = RecordStore::open(&dir).expect("store");
    let scope = Scope::workspace("/ws/fixed");

    let port = moco_job::port::allocate(
        &store,
        range(),
        &scope,
        Some("srv"),
        PortRequest::Fixed { port: 8080 },
    )
    .expect("allocate")
    .expect("port");

    assert_eq!(port, 8080, "a declared port is not negotiable");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Declaring no port gets none. Most jobs do not want one.
#[test]
fn no_port_request_allocates_nothing() {
    let dir = scratch("none");
    let store = RecordStore::open(&dir).expect("store");
    let scope = Scope::workspace("/ws/none");

    assert_eq!(
        moco_job::port::allocate(&store, range(), &scope, Some("x"), PortRequest::None)
            .expect("allocate"),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The allocated port reaches the process **as an environment variable**, and is
/// visible on the job — not hidden in the child's environment where nobody can
/// tell which checkout's server is on which port.
#[test]
fn the_port_is_injected_and_visible() {
    let dir = scratch("visible-reg");
    let ws = workspace(
        "visible",
        r#"proc ({name srv, argv (sh -c "echo port=$MOCO_PORT"), port @Auto})"#,
    );
    let reg = registry_in(&dir);
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("srv", &caller).expect("start");
    let port = reg.port_of(&id).expect("the job should carry its port");
    assert!((10000..=65535).contains(&port), "got {port}");

    reg.wait(&id).expect("wait");
    let tail = reg.tail(&id, 0).expect("tail");
    let seen = String::from_utf8_lossy(&tail.bytes);
    assert!(
        seen.contains(&format!("port={port}")),
        "the process must see the port it was given; got {seen:?}"
    );

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `@MOCO_PORT` in argv is substituted with the allocated port — for a program
/// that takes its port as a flag rather than from the environment.
#[test]
fn the_argv_token_is_substituted() {
    let dir = scratch("token-reg");
    let ws = workspace(
        "token",
        r#"proc ({name srv, argv (echo --port "@MOCO_PORT"), port @Auto})"#,
    );
    let reg = registry_in(&dir);
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("srv", &caller).expect("start");
    let port = reg.port_of(&id).expect("port");
    reg.wait(&id).expect("wait");

    let tail = reg.tail(&id, 0).expect("tail");
    let seen = String::from_utf8_lossy(&tail.bytes);
    assert!(
        seen.contains(&format!("--port {port}")),
        "the token must be replaced before the spawn; got {seen:?}"
    );

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Substitution works **inside** an element too, because it cannot change the
/// argument count: the value is node-supplied and there is no shell to re-split.
#[test]
fn the_token_substitutes_within_an_element() {
    let dir = scratch("partial-reg");
    let ws = workspace(
        "partial",
        r#"proc ({name srv, argv (echo "--port=@MOCO_PORT"), port @Auto})"#,
    );
    let reg = registry_in(&dir);
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("srv", &caller).expect("start");
    let port = reg.port_of(&id).expect("port");
    reg.wait(&id).expect("wait");

    let tail = reg.tail(&id, 0).expect("tail");
    assert!(
        String::from_utf8_lossy(&tail.bytes).contains(&format!("--port={port}")),
        "`--port=@MOCO_PORT` is one argument before and after"
    );

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **An unknown `@MOCO_` token is a loud error**, listing what does exist. The
/// alternative is a literal `@MOCO_PROT` reaching the program as an argument.
#[test]
fn an_unknown_token_is_refused_at_load() {
    let ws = workspace(
        "typo",
        r#"proc ({name srv, argv (echo "@MOCO_PROT"), port @Auto})"#,
    );
    let err = Manifest::load(&ws).expect_err("a typo must not reach the program");
    let message = err.to_string();

    assert!(message.contains("@MOCO_PROT"), "must name it: {message}");
    assert!(
        message.contains("@MOCO_PORT"),
        "must list what exists: {message}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **`$` is not policed, and must not be.** A job may ship a shell as `argv[0]`
/// — the hatch `argv-not-shell` grants for a pipeline — and `sh -c "… $MOCO_PORT"`
/// is then the *correct* way to read the port, since the node injects it into
/// the environment under that very name. Rejecting it would refuse the intended
/// usage.
#[test]
fn a_shell_wrapper_reading_the_port_variable_is_allowed() {
    let ws = workspace(
        "shell",
        r#"proc ({name srv, argv (sh -c "echo $MOCO_PORT"), port @Auto})"#,
    );
    Manifest::load(&ws).expect("a shell wrapper reading $MOCO_PORT is correct usage");
    let _ = std::fs::remove_dir_all(&ws);
}

/// Any other `@` is ordinary text — `user@host`, `@file` — so nothing needs
/// escaping and the footgun surface is only the `@MOCO_` prefix.
#[test]
fn an_unrelated_at_sign_is_left_alone() {
    let ws = workspace(
        "at",
        r#"proc ({name srv, argv (echo "user@host" "@notes.txt")})"#,
    );
    let manifest = Manifest::load(&ws).expect("an ordinary @ is not a token");
    assert_eq!(
        manifest.get("srv").expect("declared").argv,
        vec![
            "echo".to_string(),
            "user@host".to_string(),
            "@notes.txt".to_string()
        ]
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// A **terminal** job stops reserving its port immediately: there is no
/// cooldown, because stickiness — not reservation — is what keeps a port stable
/// across the gap.
#[test]
fn a_finished_job_stops_reserving_its_port() {
    let dir = scratch("reclaim-reg");
    let ws = workspace("reclaim", r#"proc ({name srv, argv ("true"), port @Auto})"#);
    let reg = registry_in(&dir);
    let caller = Caller::Scoped(Scope::resolve(&ws));

    let id = reg.start_named("srv", &caller).expect("start");
    let port = reg.port_of(&id).expect("port");
    reg.wait(&id).expect("wait");

    // A different declaration may now take that very port, because the reserved
    // set only counts live entries.
    let store = RecordStore::open(&dir).expect("store");
    let free = moco_job::port::is_available(&store, port).expect("scan");
    assert!(
        free,
        "a terminal job must release its port with no cooldown"
    );

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&dir);
}
