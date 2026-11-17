//! Transport: identify, login, Session behaviour on bad frames, acknowledgements, large replies.
mod common;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};
use taphome_cli::client::{self, Options, Session, Timeouts};
use taphome_cli::core;

/// Logs in through the library and opens a Session with the right DbHash.
async fn session(core: &FakeCore, opts: Options) -> Session {
    let ctx = core.ctx();
    let stored = core::login(&ctx).await.unwrap().session;
    let hash = core.with_state(|s| s.db_hash.clone());
    let device_id = taphome_cli::config::device_id(&ctx.env).unwrap();
    let s = Session::connect(
        &stored.host,
        &stored.location_id,
        &stored.user,
        &stored.session_id,
        Some(&hash),
        &device_id,
        opts,
    )
    .await
    .unwrap();
    assert_eq!(s.reconnect["ReconnectResultId"], 1);
    s
}

fn fast_acks() -> Options {
    Options {
        ack_interval: Duration::from_millis(100),
        ..Options::standard()
    }
}

#[test]
fn identify_reads_the_location() {
    let core = FakeCore::start();
    let info = block(client::identify(&core.host(), &Timeouts::standard())).unwrap();
    assert_eq!(info["LocationId"], LOCATION_ID);
    assert_eq!(info["Name"], "Test home");
}

#[test]
fn garbage_frame_closes_the_session_at_once() {
    let core = FakeCore::start();
    block(async {
        let s = session(&core, Options::standard()).await;
        core.garbage_on_next_request();
        let started = Instant::now();
        let limit = Duration::from_secs(30);
        let empty = json!({});
        let (a, b) = tokio::join!(
            s.request("MessageGetBasicCorePropertiesRequest", &empty, limit),
            s.request("MessageGetBasicCorePropertiesRequest", &empty, limit)
        );
        // Both the request that got garbage and the one pending next to it fail without waiting for the limit.
        assert!(a.is_err() && b.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(s.is_closed());
        // Later requests fail immediately with the same error.
        let later = Instant::now();
        let err = s
            .request("MessageGetBasicCorePropertiesRequest", &empty, limit)
            .await
            .unwrap_err();
        assert!(later.elapsed() < Duration::from_millis(500));
        assert_eq!(err.code, "protocol_error");
        assert_eq!(err.exit_code, 4);
    });
}

#[test]
fn server_close_fails_pending_and_later_requests() {
    let core = FakeCore::start();
    block(async {
        let s = session(&core, Options::standard()).await;
        core.silent_next_request();
        let limit = Duration::from_secs(30);
        let empty = json!({});
        let pending = s.request("MessageGetBasicCorePropertiesRequest", &empty, limit);
        let closer = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            core.push_raw(&[0xff; 12]);
        };
        let started = Instant::now();
        let (result, ()) = tokio::join!(pending, closer);
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        let err = s
            .request("MessageGetBasicCorePropertiesRequest", &empty, limit)
            .await
            .unwrap_err();
        assert_eq!(err.exit_code, 4);
    });
}

#[test]
fn acknowledgements_are_sent_every_interval_and_exempt_acks() {
    let core = FakeCore::start();
    block(async {
        let s = session(&core, fast_acks()).await;
        let response = s
            .request(
                "MessageGetBasicCorePropertiesRequest",
                &json!({}),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(response["$type"], "MessageGetBasicCorePropertiesResponse");
        s.wait(Duration::from_millis(500)).await;
        let acks = core.acks();
        let stamp = |kind: i64, id: i64| (kind << 32) | id;
        assert!(
            acks.contains(&stamp(2, 1)),
            "reconnect response (Response, id 1) acknowledged: {acks:?}"
        );
        assert!(
            acks.contains(&stamp(2, 2)),
            "second response acknowledged: {acks:?}"
        );
        assert!(
            acks.iter().all(|a| a >> 32 != 5),
            "acknowledgements are not acknowledged: {acks:?}"
        );
        s.close().await;
    });
}

#[test]
fn close_flushes_pending_acknowledgements() {
    let core = FakeCore::start();
    block(async {
        let s = session(&core, Options::standard()).await;
        assert!(core.acks().is_empty(), "the 4 s timer has not fired");
        s.close().await;
        for _ in 0..40 {
            if !core.acks().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(core.acks().contains(&((2i64 << 32) | 1)));
    });
}

#[test]
fn broadcasts_are_collected() {
    let core = FakeCore::start();
    block(async {
        let s = session(&core, Options::standard()).await;
        core.push_broadcast(
            "MessageDeviceValueChangedBroadcast",
            json!({"DeviceId": 10, "ValueTypeId": 48, "Value": 1.0}),
        );
        s.wait(Duration::from_millis(300)).await;
        let b = s.broadcasts();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0]["$type"], "MessageDeviceValueChangedBroadcast");
        assert_eq!(b[0]["DeviceId"], 10);
    });
}

#[test]
fn large_reconnect_response_is_accepted() {
    let core = FakeCore::start();
    core.with_state(|s| {
        s.devices[0]["DeviceProperties"]["Blob"] = json!("x".repeat(3 * 1024 * 1024))
    });
    block(async {
        let s = session(&core, Options::standard()).await;
        let blob = &s.reconnect["Devices"][0]["DeviceProperties"]["Blob"];
        assert_eq!(blob.as_str().unwrap().len(), 3 * 1024 * 1024);
    });
}

#[test]
fn login_result_is_returned_to_the_caller() {
    let core = FakeCore::start();
    let timeouts = Timeouts::standard();
    let bad = block(client::login(
        &core.host(),
        LOCATION_ID,
        USER,
        "not-the-hash",
        "0123456789abcdef",
        &timeouts,
    ))
    .unwrap();
    assert_eq!(bad["LoginResultId"], 2);
    assert!(bad.get("SessionId").is_none());
}
