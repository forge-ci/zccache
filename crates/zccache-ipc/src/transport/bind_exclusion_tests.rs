//! A Unix bind must never take a path from a live listener (#2106), while a
//! socket file left behind by a dead one stays reclaimable.

#![cfg(unix)]

use std::io::ErrorKind;
use std::os::unix::net::UnixListener as RawListener;
use std::path::Path;
use std::time::Duration;

use super::*;

const ACCEPT_BUDGET: Duration = Duration::from_secs(5);

fn private_parent(endpoint: &str) {
    std::fs::create_dir_all(Path::new(endpoint).parent().unwrap()).unwrap();
}

/// The listener still owns the path if a new client reaches it through there.
async fn assert_serves_new_clients(listener: &mut IpcListener, endpoint: &str) {
    let _client = connect(endpoint)
        .await
        .expect("the live listener's path must stay connectable");
    tokio::time::timeout(ACCEPT_BUDGET, listener.accept())
        .await
        .expect("the live listener was orphaned: a new client never reached it")
        .expect("accept");
}

#[tokio::test]
async fn second_bind_is_refused_and_does_not_orphan_the_first_listener() {
    let endpoint = unique_test_endpoint();
    let mut first = IpcListener::bind(&endpoint).unwrap();

    let second = IpcListener::bind(&endpoint);

    assert_serves_new_clients(&mut first, &endpoint).await;
    match second {
        Err(IpcError::Io(error)) => assert_eq!(error.kind(), ErrorKind::AddrInUse),
        Err(other) => panic!("expected AddrInUse, got {other}"),
        Ok(_) => panic!("a second listener must not bind a live path"),
    }
}

#[tokio::test]
async fn bind_reclaims_the_socket_a_dead_listener_left_behind() {
    let endpoint = unique_test_endpoint();
    drop(IpcListener::bind(&endpoint).unwrap());
    assert!(
        Path::new(&endpoint).exists(),
        "the fixture needs a leftover socket file"
    );

    let mut next = IpcListener::bind(&endpoint).expect("reclaim the leftover");

    assert_serves_new_clients(&mut next, &endpoint).await;
}

/// A crash from a build with no bind lock: a socket file and no lock file.
#[tokio::test]
async fn bind_reclaims_a_stale_socket_that_has_no_bind_lock() {
    let endpoint = unique_test_endpoint();
    private_parent(&endpoint);
    drop(RawListener::bind(&endpoint).unwrap());

    let mut next = IpcListener::bind(&endpoint).expect("reclaim the leftover");

    assert_serves_new_clients(&mut next, &endpoint).await;
}

/// A daemon from a build with no bind lock holds nothing the lock can see, so
/// only the probe under the lock protects it.
#[tokio::test]
async fn bind_refuses_a_live_listener_that_holds_no_bind_lock() {
    let endpoint = unique_test_endpoint();
    private_parent(&endpoint);
    let _live = RawListener::bind(&endpoint).unwrap();

    let result = IpcListener::bind(&endpoint);

    match result {
        Err(IpcError::Io(error)) => assert_eq!(error.kind(), ErrorKind::AddrInUse),
        Err(other) => panic!("expected AddrInUse, got {other}"),
        Ok(_) => panic!("bind must not take the path from a live listener"),
    }
    let _still_reachable = connect(&endpoint).await.expect("the path was not unlinked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_binds_over_a_stale_socket_elect_one_reachable_listener() {
    let endpoint = unique_test_endpoint();
    drop(IpcListener::bind(&endpoint).unwrap());
    let start = std::sync::Arc::new(std::sync::Barrier::new(8));

    let attempts: Vec<_> = (0..8)
        .map(|_| {
            let endpoint = endpoint.clone();
            let start = std::sync::Arc::clone(&start);
            tokio::task::spawn_blocking(move || {
                start.wait();
                IpcListener::bind(&endpoint)
            })
        })
        .collect();
    let mut winners = Vec::new();
    for attempt in attempts {
        match attempt.await.unwrap() {
            Ok(listener) => winners.push(listener),
            Err(IpcError::Io(error)) => assert_eq!(error.kind(), ErrorKind::AddrInUse),
            Err(other) => panic!("expected AddrInUse, got {other}"),
        }
    }

    assert_eq!(winners.len(), 1, "exactly one bind may win the path");
    assert_serves_new_clients(&mut winners[0], &endpoint).await;
}

/// A client that read a dead PID from an old record retires the endpoint. If a
/// new daemon bound in the meantime, that must not cost it its path.
#[tokio::test]
async fn retire_leaves_a_live_listener_in_place() {
    let endpoint = unique_test_endpoint();
    let mut live = IpcListener::bind(&endpoint).unwrap();

    crate::retire_endpoint(&endpoint).unwrap();

    assert_serves_new_clients(&mut live, &endpoint).await;
}

#[tokio::test]
async fn retire_removes_the_socket_of_a_dead_listener() {
    let endpoint = unique_test_endpoint();
    drop(IpcListener::bind(&endpoint).unwrap());

    crate::retire_endpoint(&endpoint).unwrap();

    assert!(!Path::new(&endpoint).exists());
}

#[tokio::test]
async fn bind_still_refuses_a_path_that_is_not_a_socket() {
    let endpoint = unique_test_endpoint();
    private_parent(&endpoint);
    std::fs::write(&endpoint, b"not a socket").unwrap();

    let Err(IpcError::Io(error)) = IpcListener::bind(&endpoint) else {
        panic!("a regular file must not be replaced");
    };

    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert!(Path::new(&endpoint).is_file());
}
