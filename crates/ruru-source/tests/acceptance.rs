//! ruru-source acceptance tests.
//!
//! Each test names the acceptance criterion it covers. Everything here drives the real
//! socket through the real `ruru-client` — no in-process shortcuts — so what passes is
//! the surface `ruru-mcp` will actually consume.

mod common;

use common::{HEIGHT, Harness, WIDTH, base_config, usable_group};
use ruru_client::proto::{self, ErrorCode, Message, Op, Request};
use ruru_client::{ClientError, SourceClient};
use std::collections::HashSet;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::time::Duration;
use tokio::net::UnixStream;

fn cfg() -> ruru_source::config::Config {
    base_config(std::path::Path::new("/tmp/placeholder.sock"))
}

async fn collect_seqs(sub: &mut ruru_client::FrameStream, n: usize) -> HashSet<u64> {
    let mut seqs = HashSet::new();
    for _ in 0..n {
        seqs.insert(sub.next().await.expect("stream ok").expect("frame").seq);
    }
    seqs
}

// ---------------------------------------------------------------------------
// AC: the source creates the socket at the configured path, mode 0660, group `ruru`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn socket_is_created_with_mode_0660() {
    let h = Harness::start(cfg()).await;
    let path = h.socket_path().to_path_buf();

    let meta = std::fs::metadata(&path).expect("socket should exist");
    assert!(meta.file_type().is_socket(), "should be a unix socket");
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o660,
        "socket mode should be 0660, got {:o}",
        meta.permissions().mode() & 0o777
    );

    h.shutdown().await;
    assert!(!path.exists(), "socket should be removed on shutdown");
}

#[tokio::test]
async fn socket_group_is_applied_when_configured() {
    let gid = match usable_group("ruru") {
        Some(gid) => gid,
        None => {
            // On a host that is *supposed* to have the group (CI, or a host after
            // `deploy/install.sh`), a skip here would be an unverified test masquerading as
            // a pass. RURU_REQUIRE_GROUP turns that into a hard failure. Locally, where
            // there is legitimately no `ruru` group, it still skips.
            if std::env::var_os("RURU_REQUIRE_GROUP").is_some() {
                panic!(
                    "RURU_REQUIRE_GROUP is set but there is no usable `ruru` group. \
                     Add this user to it — `sudo usermod -aG ruru $USER` — and start a \
                     new login session, then re-run."
                );
            }
            eprintln!(
                "SKIP socket_group_is_applied_when_configured: no usable `ruru` group on \
                 this host (see CONTRIBUTING.md to run it)"
            );
            return;
        }
    };

    let mut config = cfg();
    config.source.socket_group = Some("ruru".into());
    let h = Harness::start(config).await;

    let meta = std::fs::metadata(h.socket_path()).unwrap();
    assert_eq!(
        std::os::unix::fs::MetadataExt::gid(&meta),
        gid,
        "socket should be owned by group `ruru`"
    );
    h.shutdown().await;
}

/// The socket's only access control is its ownership, so a group we cannot apply must
/// stop the daemon rather than downgrade the boundary silently.
#[tokio::test]
async fn unknown_socket_group_aborts_startup() {
    use ruru_source::RunningSource;
    use ruru_source::camera::mock::MockConfig;
    use ruru_source::camera::{Camera, MockCamera};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("source.sock");
    let mut config = cfg();
    config.source.socket_path = socket.clone();
    config.source.socket_group = Some("ruru-no-such-group-xyz".into());

    let camera = MockCamera::new(MockConfig::default());
    let err = RunningSource::start_with_camera(config, Box::new(camera) as Box<dyn Camera>)
        .await
        .expect_err("startup should abort");

    let chain = format!("{err:#}");
    assert!(
        chain.contains("no such group"),
        "error should name the missing group, got: {chain}"
    );
    assert!(
        !socket.exists(),
        "a half-configured socket must not be left on disk"
    );
}

// ---------------------------------------------------------------------------
// AC: snapshot() returns one framed image + metadata; subscribe() yields a stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn snapshot_returns_one_decodable_frame() {
    let h = Harness::start(cfg()).await;

    let frame = h.client.snapshot().await.expect("snapshot");

    assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
    assert_eq!(frame.fourcc.to_string(), "MJPG");
    assert!(
        frame.looks_like_jpeg(),
        "payload should start with JPEG SOI"
    );
    assert_eq!(
        &frame.data[frame.data.len() - 2..],
        &[0xFF, 0xD9],
        "payload should end with JPEG EOI"
    );
    assert!(frame.data.len() > 500, "suspiciously small JPEG");
    assert!(
        frame.timestamp_us > 1_600_000_000_000_000,
        "plausible epoch us"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn subscribe_yields_a_continuous_stream() {
    let h = Harness::start(cfg()).await;

    let mut sub = h.client.subscribe().await.expect("subscribe");
    let mut seqs = Vec::new();
    for _ in 0..6 {
        let frame = sub.next().await.expect("stream ok").expect("frame");
        assert!(frame.looks_like_jpeg());
        seqs.push(frame.seq);
    }

    assert!(
        seqs.windows(2).all(|w| w[1] > w[0]),
        "seq must strictly increase, got {seqs:?}"
    );

    drop(sub);
    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// AC: two concurrent subscribers both receive frames from a single capture loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_subscribers_share_one_capture_loop() {
    let h = Harness::start(cfg()).await;

    let mut a = h.client.subscribe().await.expect("subscriber a");
    let mut b = h.client.subscribe().await.expect("subscriber b");

    eventually!(
        "both subscribers to register",
        Duration::from_secs(5),
        h.client.status().await.unwrap().subscribers == 2
    );

    // Read concurrently: draining one and then the other would let the idle one fall
    // behind and be skipped forward, which would hide the very overlap being tested.
    let (seen_a, seen_b) = tokio::join!(collect_seqs(&mut a, 8), collect_seqs(&mut b, 8));

    assert!(!seen_a.is_empty() && !seen_b.is_empty());
    assert!(
        !seen_a.is_disjoint(&seen_b),
        "both subscribers must see frames from the same capture: {seen_a:?} vs {seen_b:?}"
    );
    assert_eq!(
        h.stats.opens(),
        1,
        "the device must be opened exactly once for both subscribers"
    );

    drop(a);
    drop(b);
    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// AC: ref-counting opens on first attach and idle-closes after the last detaches;
//     the pin forces always-open
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refcount_opens_on_demand_and_idle_closes() {
    let h = Harness::start(cfg()).await; // idle_grace_secs = 1

    let status = h.client.status().await.unwrap();
    assert!(!status.open, "device should start closed");
    assert!(!status.pinned);
    assert_eq!(h.stats.opens(), 0, "nothing should open the device yet");

    h.client.snapshot().await.expect("snapshot");
    assert_eq!(h.stats.opens(), 1, "first attach should open the device");

    // The snapshot has detached, but the grace period keeps the device warm.
    let status = h.client.status().await.unwrap();
    assert_eq!(status.subscribers, 0, "snapshot should have detached");
    assert!(status.open, "device should stay open during the idle grace");

    eventually!(
        "the device to idle-close",
        Duration::from_secs(6),
        !h.client.status().await.unwrap().open
    );
    assert_eq!(h.stats.closes(), 1);

    // And it comes back on demand.
    h.client.snapshot().await.expect("second snapshot");
    assert_eq!(h.stats.opens(), 2, "a later attach should reopen");

    h.shutdown().await;
}

#[tokio::test]
async fn a_subscriber_holds_the_device_open_past_the_grace() {
    let h = Harness::start(cfg()).await; // grace = 1s

    let mut sub = h.client.subscribe().await.unwrap();
    sub.next().await.unwrap().expect("frame");

    tokio::time::sleep(Duration::from_millis(2500)).await;

    let status = h.client.status().await.unwrap();
    assert!(
        status.open,
        "an attached subscriber must prevent idle-close"
    );
    assert_eq!(status.subscribers, 1);
    assert_eq!(h.stats.closes(), 0);

    drop(sub);
    eventually!(
        "idle-close after the subscriber leaves",
        Duration::from_secs(6),
        !h.client.status().await.unwrap().open
    );

    h.shutdown().await;
}

#[tokio::test]
async fn pin_holds_the_device_open_with_no_clients() {
    let mut config = cfg();
    config.source.pin = true;
    let h = Harness::start(config).await;

    eventually!(
        "the pinned device to open unprompted",
        Duration::from_secs(5),
        h.client.status().await.unwrap().open
    );

    let status = h.client.status().await.unwrap();
    assert!(status.pinned);
    assert_eq!(status.subscribers, 0, "pin holds it open with zero clients");
    assert_eq!(h.stats.opens(), 1);

    // Well past idle_grace_secs = 1.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        h.client.status().await.unwrap().open,
        "pin must survive the idle grace"
    );
    assert_eq!(h.stats.closes(), 0);

    // And it is genuinely capturing, not merely open.
    eventually!(
        "the pinned source to report a capture rate",
        Duration::from_secs(5),
        h.client.status().await.unwrap().fps > 1.0
    );

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// AC: get_control / set_control round-trip through the socket to the mock
// ---------------------------------------------------------------------------

#[tokio::test]
async fn controls_round_trip_through_the_socket() {
    let h = Harness::start(cfg()).await;

    let controls = h.client.list_controls().await.expect("list_controls");
    assert!(
        controls.iter().any(|c| c.name == "brightness"),
        "expected a brightness control, got {:?}",
        controls.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    let before = h.client.get_control("brightness").await.expect("get");
    assert_eq!(before.value, 128);
    assert_eq!((before.min, before.max), (0, 255));

    let set = h
        .client
        .set_control("brightness", 200, false)
        .await
        .expect("set");
    assert_eq!(set.value, 200);
    assert_eq!(
        h.client.get_control("brightness").await.unwrap().value,
        200,
        "the new value must persist for the next reader"
    );

    // V4L2 snaps to the step grid, so callers must trust the returned descriptor.
    // pan_absolute has step 3600.
    let snapped = h
        .client
        .set_control("pan_absolute", 5000, false)
        .await
        .unwrap();
    assert_eq!(
        snapped.value, 3600,
        "step 3600 should snap 5000 down to 3600"
    );

    h.shutdown().await;
}

/// The bug the real Brio exposed, end to end through the socket: a control that is
/// inactive until its auto gate is off does not take if set first, and does take once the
/// gate is applied before it — which is why config order is preserved.
#[tokio::test]
async fn gated_control_takes_only_when_its_gate_is_applied_first() {
    let h = Harness::start(cfg()).await;

    // Autofocus on (default) → focus_absolute is inactive → the write silently no-ops.
    let applied = h
        .client
        .set_control("focus_absolute", 40, false)
        .await
        .unwrap();
    assert_eq!(
        applied.value, 0,
        "focus_absolute must not take while autofocus is on"
    );

    // Apply the gate first, then the same write sticks — the correct config order.
    h.client
        .set_control("focus_automatic_continuous", 0, false)
        .await
        .unwrap();
    let applied = h
        .client
        .set_control("focus_absolute", 40, false)
        .await
        .unwrap();
    assert_eq!(
        applied.value, 40,
        "with autofocus off, focus_absolute takes"
    );

    h.shutdown().await;
}

/// A control set with `persist` is written to the state file and re-applied on the
/// next open, so operator tuning survives a restart of `ruru-source` — the whole point of
/// persisting to the source (which owns device state) rather than to `ruru-mcp`.
#[tokio::test]
async fn persisted_control_overrides_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("source.sock");
    let state = dir.path().join("controls.json");

    let mut config = base_config(&socket);
    config.source.controls_state_path = Some(state.clone());
    config.source.idle_grace_secs = 60; // keep the device open between snapshot and get

    // First run: persist a brightness override (default is 128), then stop the source.
    let h = Harness::start(config.clone()).await;
    let set = h
        .client
        .set_control("brightness", 200, true)
        .await
        .expect("set + persist");
    assert_eq!(set.value, 200);
    h.shutdown().await;
    assert!(
        state.exists(),
        "the override must be written to the state file"
    );

    // Second run: a fresh mock camera comes up at its 128 default. A snapshot drives the
    // control-applying open path, which must re-apply the persisted override.
    let h = Harness::start(config).await;
    h.client.snapshot().await.expect("snapshot");
    let got = h.client.get_control("brightness").await.expect("get");
    assert_eq!(
        got.value, 200,
        "a persisted control override must survive a ruru-source restart"
    );
    h.shutdown().await;
}

/// A correctly ordered lock (gate before gated control) applies cleanly and leaves no
/// error; a misordered one is caught and surfaced in `status.last_error` rather than
/// shipping a silently-unlocked camera.
#[tokio::test]
async fn misordered_locked_controls_are_flagged_in_status() {
    // Correct order: gate first.
    let mut good = cfg();
    good.camera
        .controls
        .insert("focus_automatic_continuous".into(), 0);
    good.camera.controls.insert("focus_absolute".into(), 40);
    let h = Harness::start(good).await;
    h.client.snapshot().await.expect("open");
    assert_eq!(
        h.client.get_control("focus_absolute").await.unwrap().value,
        40
    );
    assert!(
        h.client.status().await.unwrap().control_warnings.is_empty(),
        "a correctly ordered lock should produce no warnings"
    );
    h.shutdown().await;

    // Wrong order: control before its gate. The focus lock cannot take.
    let mut bad = cfg();
    bad.camera.controls.insert("focus_absolute".into(), 40);
    bad.camera
        .controls
        .insert("focus_automatic_continuous".into(), 0);
    let h = Harness::start(bad).await;
    h.client.snapshot().await.expect("open");
    assert_eq!(
        h.client.get_control("focus_absolute").await.unwrap().value,
        0,
        "focus_absolute cannot take when set before autofocus is disabled"
    );
    let warnings = h.client.status().await.unwrap().control_warnings;
    assert!(
        warnings.iter().any(|w| w.contains("focus_absolute")),
        "the misordered lock must be surfaced in control_warnings, got {warnings:?}"
    );

    // And it must survive a successful capture — this is the flash-and-vanish bug the
    // separate field exists to prevent.
    h.client.snapshot().await.expect("another frame");
    assert!(
        h.client
            .status()
            .await
            .unwrap()
            .control_warnings
            .iter()
            .any(|w| w.contains("focus_absolute")),
        "a config warning must persist across successful captures, not clear like a \
         transient error"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn control_errors_are_specific() {
    let h = Harness::start(cfg()).await;

    let err = h.client.get_control("no_such_control").await.unwrap_err();
    assert!(
        matches!(
            err,
            ClientError::Source {
                code: ErrorCode::UnknownControl,
                ..
            }
        ),
        "got {err:?}"
    );

    let err = h
        .client
        .set_control("brightness", 9999, false)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ClientError::Source {
                code: ErrorCode::ControlRange,
                ..
            }
        ),
        "got {err:?}"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn locked_controls_are_applied_on_open() {
    let mut config = cfg();
    config.camera.controls.insert("brightness".into(), 64);
    config
        .camera
        .controls
        .insert("power_line_frequency".into(), 1);
    let h = Harness::start(config).await;

    h.client.snapshot().await.expect("snapshot forces an open");

    assert_eq!(h.client.get_control("brightness").await.unwrap().value, 64);
    assert_eq!(
        h.client
            .get_control("power_line_frequency")
            .await
            .unwrap()
            .value,
        1,
        "50 Hz mains lock should be applied"
    );

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// The master gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabling_refuses_capture_and_closes_the_device() {
    let h = Harness::start(cfg()).await;

    h.client.snapshot().await.expect("works while enabled");
    assert_eq!(h.stats.opens(), 1);

    let status = h.client.set_enabled(false).await.expect("disable");
    assert!(!status.enabled);
    assert!(!status.open, "disabling must close the device immediately");
    assert_eq!(h.stats.closes(), 1);

    assert!(h.client.snapshot().await.unwrap_err().is_disabled());
    assert!(h.client.subscribe().await.unwrap_err().is_disabled());
    assert!(h.client.list_controls().await.unwrap_err().is_disabled());
    assert!(
        h.client
            .get_control("brightness")
            .await
            .unwrap_err()
            .is_disabled(),
        "a disabled source must not touch the device at all"
    );

    // Status stays available — you must be able to see that it is off.
    assert!(!h.client.status().await.unwrap().enabled);

    let status = h.client.set_enabled(true).await.expect("re-enable");
    assert!(status.enabled);
    h.client.snapshot().await.expect("works again");
    assert_eq!(h.stats.opens(), 2);

    h.shutdown().await;
}

#[tokio::test]
async fn disabling_terminates_a_live_subscription() {
    let h = Harness::start(cfg()).await;

    let mut sub = h.client.subscribe().await.unwrap();
    sub.next().await.unwrap().expect("first frame");

    h.client.set_enabled(false).await.expect("disable");

    // Frames already in flight may still arrive; the stream must end with a clear reason
    // rather than just going quiet.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match sub.next().await {
                Ok(Some(_)) => continue,
                Ok(None) => panic!("stream ended silently instead of reporting why"),
                Err(e) => return e,
            }
        }
    })
    .await
    .expect("the subscription should end promptly");

    assert!(ended.is_disabled(), "got {ended:?}");
    h.shutdown().await;
}

#[tokio::test]
async fn starting_disabled_refuses_until_enabled() {
    let mut config = cfg();
    config.source.enabled = false;
    let h = Harness::start(config).await;

    assert!(h.client.snapshot().await.unwrap_err().is_disabled());
    assert_eq!(
        h.stats.opens(),
        0,
        "a disabled source must never open the device"
    );

    h.client.set_enabled(true).await.unwrap();
    h.client.snapshot().await.expect("enabled");

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// Device failure and recovery
// ---------------------------------------------------------------------------

/// Stands in for a replug: the device refuses to open, then starts working. The source
/// must back off and retry rather than fail permanently.
#[tokio::test]
async fn recovers_after_the_device_fails_to_open() {
    let mut config = cfg();
    config.camera.reopen_backoff_secs = 1;
    let h = Harness::start_with(config, |cam| cam.with_open_failures(2)).await;

    let frame = h
        .client
        .snapshot()
        .await
        .expect("should recover and capture");
    assert!(frame.looks_like_jpeg());
    assert_eq!(h.stats.opens(), 1, "one successful open after the failures");

    let status = h.client.status().await.unwrap();
    assert!(status.open);

    h.shutdown().await;
}

// ---------------------------------------------------------------------------
// Protocol hygiene
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wrong_protocol_version_is_rejected() {
    let h = Harness::start(cfg()).await;

    let mut stream = UnixStream::connect(h.socket_path()).await.unwrap();
    let request = Request {
        v: 999,
        op: Op::Snapshot,
    };
    proto::write_json(&mut stream, &request).await.unwrap();

    let response: proto::Response = proto::read_message(&mut stream)
        .await
        .unwrap()
        .unwrap()
        .json_as()
        .unwrap()
        .unwrap();

    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, ErrorCode::ProtoVersion);

    h.shutdown().await;
}

#[tokio::test]
async fn malformed_request_is_rejected() {
    let h = Harness::start(cfg()).await;

    let mut stream = UnixStream::connect(h.socket_path()).await.unwrap();
    let body = {
        let mut b = vec![proto::TAG_JSON];
        b.extend_from_slice(b"{not json at all");
        b
    };
    proto::write_body(&mut stream, &body).await.unwrap();

    let Some(Message::Json(bytes)) = proto::read_message(&mut stream).await.unwrap() else {
        panic!("expected a control message");
    };
    let response: proto::Response = serde_json::from_slice(&bytes).unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, ErrorCode::BadRequest);

    // One bad client must not take the source down.
    h.client.status().await.expect("source still serving");
    h.shutdown().await;
}

/// Two daemons cannot both own one camera, so the second must refuse rather than
/// unlink a live socket out from under the first.
#[tokio::test]
async fn a_second_source_refuses_to_steal_the_socket() {
    use ruru_source::RunningSource;
    use ruru_source::camera::mock::MockConfig;
    use ruru_source::camera::{Camera, MockCamera};

    let h = Harness::start(cfg()).await;

    let mut config = cfg();
    config.source.socket_path = h.socket_path().to_path_buf();
    let camera = MockCamera::new(MockConfig::default());
    let err = RunningSource::start_with_camera(config, Box::new(camera) as Box<dyn Camera>)
        .await
        .expect_err("the second source should refuse to start");

    let chain = format!("{err:#}");
    assert!(
        chain.contains("already being served"),
        "unexpected error: {chain}"
    );

    // The original is untouched.
    h.client
        .snapshot()
        .await
        .expect("first source still serving");
    h.shutdown().await;
}

#[tokio::test]
async fn a_stale_socket_file_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("source.sock");
    std::fs::write(&socket, b"not really a socket").unwrap();

    let mut config = cfg();
    config.source.socket_path = socket.clone();

    // Harness::start rewrites the socket path, so drive the source directly here.
    use ruru_source::RunningSource;
    use ruru_source::camera::mock::MockConfig;
    use ruru_source::camera::{Camera, MockCamera};
    let camera = MockCamera::new(MockConfig {
        width: WIDTH,
        height: HEIGHT,
        jpeg_quality: 60,
    });
    let source = RunningSource::start_with_camera(config, Box::new(camera) as Box<dyn Camera>)
        .await
        .expect("should reclaim the stale path");

    SourceClient::new(&socket)
        .status()
        .await
        .expect("serving on the reclaimed path");
    source.shutdown().await;
}

#[tokio::test]
async fn status_reports_the_backend_and_grace() {
    let h = Harness::start(cfg()).await;
    let status = h.client.status().await.unwrap();
    assert_eq!(status.camera_backend, "mock");
    assert_eq!(status.idle_grace_secs, 1);
    assert!(status.last_error.is_none());
    h.shutdown().await;
}
