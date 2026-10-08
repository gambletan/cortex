//! Independent black-box privacy checks using the parent HTTP/stdio harness.
//! Expectations come from the Muse design and public guide, not implementation code.
use super::*;

#[cfg(unix)]
fn assert_disclosure_fence_blocks_revocation(delete: bool) {
    use std::os::fd::AsRawFd;
    let tmp = TempDir::new("privacy-revocation-fence");
    let cloud = Cloud::start(tmp.path());
    let (_, _, device) = new_tenant(&cloud);
    let text = "Rigel sensitive flight plan";
    device.push_export(next_version(), &items(&[text])).unwrap();
    let (link, _) = device.enroll().unwrap();
    let pid = rid_of(&link);
    let muse = Muse::new(&cloud, &pid);
    let token = muse.sign_in();
    assert_eq!(muse.recall(&token, text), vec![text.to_string()]);
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(tenant_dir_of(&tmp.path().join("data"), &pid).join("gateway-state.lock")).unwrap();
    // The design exposes this OS lock as the in-flight disclosure boundary.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let operation = scope.spawn(|| {
            started_tx.send(()).unwrap();
            let result = if delete { device.delete() }
                else { device.push_export(next_version(), &[]).map(|_| ()) };
            done_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let early = done_rx.recv_timeout(Duration::from_millis(400));
        // Release before assertions so a failing test cannot strand its worker.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
        assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "revocation returned while disclosure lock was held: {early:?}");
        done_rx.recv_timeout(Duration::from_secs(15)).unwrap().unwrap();
        operation.join().unwrap();
    });
    if delete {
        let response = muse.call(&token, "recall_memory", json!({"query": text}));
        assert!(matches!(response.status, 401 | 404), "{}", response.dump());
    } else {
        assert!(muse.recall(&token, text).is_empty());
    }
}

#[cfg(unix)]
#[test]
fn export_revocation_waits_for_in_flight_disclosure_boundary() {
    assert_disclosure_fence_blocks_revocation(false);
}

#[cfg(unix)]
#[test]
fn tenant_deletion_waits_for_in_flight_disclosure_boundary() {
    assert_disclosure_fence_blocks_revocation(true);
}

#[cfg(unix)]
fn assert_oauth_replay_waits_for_disclosure(code_replay: bool) {
    use std::os::fd::AsRawFd;
    let tmp = TempDir::new("privacy-oauth-replay-fence");
    let cloud = Cloud::start(tmp.path());
    let (_, _, device) = new_tenant(&cloud);
    let text = "Pollux protected reservation";
    device.push_export(next_version(), &items(&[text])).unwrap();
    let (link, _) = device.enroll().unwrap();
    let pid = rid_of(&link);
    let muse = Muse::new(&cloud, &pid);
    let client = muse.register();
    let verifier = rand_b64url(32);
    let challenge = URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
    let consent = muse.authorize(&client, &challenge, "privacy-replay-state");
    assert_eq!(consent.status, 200, "{}", consent.resp.dump());
    let approved = muse.approve(consent.req.as_deref().unwrap(), consent.csrf.as_deref().unwrap(),
        consent.cookie.as_deref(), Some(BASE));
    assert_eq!(approved.status, 303, "{}", approved.dump());
    let wait = muse.follow(approved.location().unwrap());
    assert_eq!(wait.status, 302, "{}", wait.dump());
    let (_, params) = split_location(wait.location().unwrap());
    let code = params.get("code").unwrap();
    let resource = muse.resource();
    let exchange = [("grant_type", "authorization_code"), ("code", code.as_str()),
        ("redirect_uri", MUSE_CB), ("client_id", client.as_str()),
        ("code_verifier", verifier.as_str()), ("resource", resource.as_str())];
    let issued = muse.token(&exchange);
    assert_eq!(issued.status, 200, "{}", issued.dump());
    let issued = issued.json();
    let original_access = issued["access_token"].as_str().unwrap();
    let refresh = issued["refresh_token"].as_str().unwrap();
    let refresh_args = [("grant_type", "refresh_token"), ("refresh_token", refresh),
        ("client_id", client.as_str()), ("resource", resource.as_str())];
    let rotated_access = if code_replay { original_access.to_string() } else {
        let rotated = muse.token(&refresh_args);
        assert_eq!(rotated.status, 200, "{}", rotated.dump());
        rotated.json()["access_token"].as_str().unwrap().to_string()
    };
    assert_eq!(muse.recall(&rotated_access, text), vec![text.to_string()]);
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(tenant_dir_of(&tmp.path().join("data"), &pid).join("gateway-state.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            started_tx.send(()).unwrap();
            done_tx.send(muse.token(if code_replay { &exchange } else { &refresh_args })).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let early = done_rx.recv_timeout(Duration::from_millis(400));
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
        assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "OAuth replay revocation completed while disclosure was in flight");
        let refused = done_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert_eq!(refused.status, 400, "{}", refused.dump());
        worker.join().unwrap();
    });
    for access in [original_access, rotated_access.as_str()] {
        let denied = muse.call(access, "recall_memory", json!({"query": text}));
        assert_eq!(denied.status, 401, "replay must revoke the grant: {}", denied.dump());
        assert!(!denied.body.contains(text), "{}", denied.dump());
    }
}

#[cfg(unix)]
#[test]
fn refresh_token_reuse_revocation_waits_for_disclosure_boundary() {
    assert_oauth_replay_waits_for_disclosure(false);
}

#[cfg(unix)]
#[test]
fn authorization_code_replay_revocation_waits_for_disclosure_boundary() {
    assert_oauth_replay_waits_for_disclosure(true);
}

fn read_count(status: &Value, key: &str, text: &str) -> u64 {
    status[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing public read list {key}: {status}"))
        .iter()
        .filter(|r| r["text"] == json!(text))
        .map(|r| r["times"].as_u64().expect("read count"))
        .sum()
}

#[test]
fn unchanged_full_snapshot_preserves_reads_today() {
    let tmp = TempDir::new("privacy-refresh-audit");
    let cloud = Cloud::start(tmp.path());
    let (_, _, device) = new_tenant(&cloud);
    let text = "Orion prefers morning trains";
    let snapshot = items(&[text]);
    device.push_export(next_version(), &snapshot).unwrap();
    let (link, _) = device.enroll().unwrap();
    let muse = Muse::new(&cloud, &rid_of(&link));
    let token = muse.sign_in();
    assert_eq!(muse.recall(&token, text), vec![text.to_string()]);
    let before = device.status().unwrap();
    assert_eq!(read_count(&before, "read_today", text), 1, "{before}");
    device.push_export(next_version(), &snapshot).unwrap();
    let refreshed = device.status().unwrap();
    assert_eq!(read_count(&refreshed, "read_today", text), 1,
        "a snapshot refresh must not erase an actual disclosure: {refreshed}");
    assert_eq!(muse.recall(&token, text), vec![text.to_string()]);
    let after = device.status().unwrap();
    assert_eq!(read_count(&after, "read_today", text), 2, "{after}");
}

#[test]
fn adding_a_share_keeps_prior_disclosures_visible_and_failures_do_not_add_reads() {
    let tmp = TempDir::new("privacy-local-audit");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let text = "Pegasus enjoys green tea";
    let pid = rid_of(dev.connect_texts(&[text])["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &pid);
    let token = muse.sign_in();
    assert_eq!(muse.recall(&token, text), vec![text.to_string()]);
    let share = dev.call_confirmed("muse_share", json!({"texts": ["Lyra takes evening walks"]}));
    assert!(!share.is_error, "{}", share.text);
    let before = dev.status();
    assert_eq!(read_count(&before, "muse_read_today", text), 1, "{before}");
    assert_eq!(read_count(&before, "muse_read_today", "Lyra takes evening walks"), 0, "{before}");
    let denied = muse.call("invalid-token", "recall_memory", json!({"query": text}));
    assert_eq!(denied.status, 401, "{}", denied.dump());
    let malformed = muse.call(&token, "recall_memory", json!({"query": 123}));
    assert!(!mcp_ok(&malformed), "{}", malformed.dump());
    let after = dev.status();
    assert_eq!(read_count(&after, "muse_read_today", text), 1, "failed requests disclosed nothing: {after}");
    assert_eq!(read_count(&after, "muse_read_today", "Lyra takes evening walks"), 0, "{after}");
}

#[test]
fn stale_snapshot_after_restart_cannot_restore_unshared_text() {
    let tmp = TempDir::new("privacy-stale-restart");
    let cloud = Cloud::start(tmp.path());
    let (secret, mid, device) = new_tenant(&cloud);
    let old_version = next_version();
    let old_snapshot = items(&["Vega private address", "Altair public hobby"]);
    device.push_export(old_version, &old_snapshot).unwrap();
    let (link, _) = device.enroll().unwrap();
    let pid = rid_of(&link);
    let muse = Muse::new(&cloud, &pid);
    let token = muse.sign_in();
    assert!(muse.recall(&token, "Vega private address").contains(&"Vega private address".to_string()));
    device.push_export(next_version(), &items(&["Altair public hobby"])).unwrap();
    assert!(muse.recall(&token, "Vega private address").iter().all(|t| !t.contains("Vega")));
    cloud.stop();
    let restarted = Cloud::start(tmp.path());
    let device = Device::new(secret, Some(mid), restarted.url());
    let err = device.push_export(old_version, &old_snapshot).expect_err("stale snapshot refused after restart");
    assert!(err.contains("409"), "{err}");
    let muse = Muse::new(&restarted, &pid);
    assert!(muse.recall(&token, "Vega private address").iter().all(|t| !t.contains("Vega")));
    assert_eq!(muse.recall(&token, "Altair public hobby"), vec!["Altair public hobby".to_string()]);
}

#[test]
fn delete_cannot_be_undone_by_a_late_device_snapshot_after_restart() {
    let tmp = TempDir::new("privacy-delete-restart");
    let cloud = Cloud::start(tmp.path());
    let (secret, mid, device) = new_tenant(&cloud);
    let snapshot = items(&["Sirius sensitive fact"]);
    device.push_export(next_version(), &snapshot).unwrap();
    let (link, _) = device.enroll().unwrap();
    let pid = rid_of(&link);
    let token = Muse::new(&cloud, &pid).sign_in();
    device.delete().unwrap();
    cloud.stop();
    let restarted = Cloud::start(tmp.path());
    let device = Device::new(secret, Some(mid), restarted.url());
    assert!(device.push_export(next_version(), &snapshot).is_err(), "a deleted identity cannot recreate its export");
    let response = Muse::new(&restarted, &pid).call(&token, "recall_memory", json!({"query": "Sirius sensitive fact"}));
    assert!(matches!(response.status, 401 | 404), "{}", response.dump());
    assert!(!response.body.contains("Sirius sensitive fact"), "{}", response.dump());
    assert_eq!(get(restarted.port, &format!("/.well-known/oauth-authorization-server/t/{pid}")).status, 404);
}

#[test]
fn unavailable_audit_is_reported_and_prevents_unrecorded_disclosure() {
    let tmp = TempDir::new("privacy-audit-unavailable");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let text = "Deneb confidential itinerary";
    let pid = rid_of(dev.connect_texts(&[text])["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &pid);
    let token = muse.sign_in();
    assert_eq!(muse.recall(&token, text), vec![text.to_string()]);
    // Fault injection touches only the documented audit artifact, never the DB.
    let audit = tenant_dir_of(&tmp.path().join("data"), &pid).join("gateway-audit.jsonl");
    std::fs::remove_file(&audit).unwrap();
    std::fs::create_dir(&audit).unwrap();
    let failed = muse.call(&token, "recall_memory", json!({"query": text}));
    assert!(!mcp_ok(&failed), "no disclosure without an audit record: {}", failed.dump());
    assert!(!failed.body.contains(text), "{}", failed.dump());
    let status = dev.status();
    assert!(status["cloud_error"].is_string() || status["audit_error"].is_string()
        || status["muse_read_today_error"].is_string(),
        "unreadable audit must be explicitly unavailable, not trustworthy empty: {status}");
}
