//! Two overlay nodes on loopback, in one process: a desktop that publishes a hub, and a phone that pairs with it by invite
//! and reaches the hub through the overlay. No relay and no LAN address are involved; the phone dials the desktop's
//! direct listener on 127.0.0.1, which is the same path a USB tunnel gives the phone.

use std::net::{SocketAddr, TcpListener as StdListener};
use std::path::PathBuf;

use mm_overlay::{Overlay, Settings, ALLOW_PAIRED, HUB_SERVICE};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A free loopback port, found by binding and letting go. The window is small; the test fails loudly if it is lost.
fn free_loopback_port() -> SocketAddr {
    let probe = StdListener::bind("127.0.0.1:0").expect("bind a loopback port");
    probe.local_addr().expect("local address")
}

/// A stand-in for the hub: answers every connection with a fixed body.
async fn fake_hub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the fake hub");
    let addr = listener.local_addr().expect("local address");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = vec![0u8; 4096];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn get_health(addr: SocketAddr) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect to the overlay endpoint");
    stream
        .write_all(b"GET /v1/health HTTP/1.1\r\nHost: hub\r\nConnection: close\r\n\r\n")
        .await
        .expect("send the request");
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.expect("read the reply");
    String::from_utf8_lossy(&reply).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_phone_reaches_the_hub_through_the_overlay() {
    let hub = fake_hub().await;
    let dir = scratch("paired");
    let desk_listen = free_loopback_port();

    let desk = Overlay::start(
        &dir.join("desk"),
        Settings {
            name: "desk".into(),
            listen: Some(desk_listen),
            ..Settings::default()
        },
    )
    .await
    .expect("start the desktop node");
    desk.expose(HUB_SERVICE, hub, &[ALLOW_PAIRED], "hub")
        .expect("publish the hub");
    let invite = desk.invite(600);

    let phone = Overlay::start(
        &dir.join("phone"),
        Settings {
            name: "phone".into(),
            ..Settings::default()
        },
    )
    .await
    .expect("start the phone node");
    let paired = phone.accept(&invite, Some("desk".into())).expect("pair with the desktop");
    assert_eq!(paired.node_id, desk.node_id());
    assert!(
        paired.direct.contains(&desk_listen.to_string()),
        "the invite carries the desktop's direct address: {:?}",
        paired.direct
    );

    let local = phone
        .connect(&paired.node_id, HUB_SERVICE)
        .await
        .expect("reach the desktop's hub through the overlay");
    assert!(local.ip().is_loopback(), "the endpoint is on the phone's loopback: {local}");
    let reply = get_health(local).await;
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(reply.ends_with("ok"), "{reply}");

    // The same name gives the same endpoint, so the app can ask again without changing its address.
    assert_eq!(phone.connect(&paired.node_id, HUB_SERVICE).await.expect("connect again"), local);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_that_never_paired_cannot_reach_the_hub() {
    let hub = fake_hub().await;
    let dir = scratch("stranger");
    let desk_listen = free_loopback_port();

    let desk = Overlay::start(
        &dir.join("desk"),
        Settings {
            name: "desk".into(),
            listen: Some(desk_listen),
            ..Settings::default()
        },
    )
    .await
    .expect("start the desktop node");
    desk.expose(HUB_SERVICE, hub, &[ALLOW_PAIRED], "hub")
        .expect("publish the hub");

    // This node has no invite, so it has no address for the desktop and no pairing with it.
    let stranger = Overlay::start(
        &dir.join("stranger"),
        Settings {
            name: "stranger".into(),
            ..Settings::default()
        },
    )
    .await
    .expect("start the other node");
    let refused = stranger.connect(&desk.node_id(), HUB_SERVICE).await;
    assert!(refused.is_err(), "an unpaired node got an endpoint: {refused:?}");
}
