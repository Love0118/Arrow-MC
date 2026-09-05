use arrow_mc::server::{
    access::{AccessError, AccessLimits, LoginAccess},
    login::AuthenticatedProfile,
};
use serde_json::{Value, json};
use std::{
    net::IpAddr,
    time::{Duration, UNIX_EPOCH},
};

fn profile(id: u8) -> AuthenticatedProfile {
    AuthenticatedProfile {
        id: [id; 16],
        name: "same_name".into(),
        properties: Vec::new(),
    }
}

fn ip() -> IpAddr {
    "192.0.2.1".parse().unwrap()
}

fn rejection(key: &str) -> Option<Value> {
    Some(json!({"translate": key}))
}

#[test]
fn default_policy_allows_below_capacity_and_rejects_at_or_above_it() {
    let access = LoginAccess::new(2);
    assert_eq!(access.check(&profile(1), ip(), 0), None);
    assert_eq!(access.check(&profile(1), ip(), 1), None);
    for count in [2, 3, usize::MAX] {
        assert_eq!(
            access.check(&profile(1), ip(), count),
            rejection("multiplayer.disconnect.server_full")
        );
    }
    assert_eq!(
        LoginAccess::new(0).check(&profile(1), ip(), 0),
        rejection("multiplayer.disconnect.server_full")
    );
}

#[test]
fn rejection_precedence_is_profile_whitelist_ip_then_capacity() {
    let mut access = LoginAccess::new(0);
    let p = profile(1);
    access.set_whitelist_enabled(true);
    access.set_profile_ban(p.id, Some("profile"), None).unwrap();
    access.set_ip_ban(ip(), Some("address"), None).unwrap();
    assert_eq!(
        access.check(&p, ip(), 0),
        Some(json!({
            "translate": "multiplayer.disconnect.banned.reason", "with": [{"text": "profile"}]
        }))
    );
    assert!(access.remove_profile_ban(p.id));
    assert_eq!(
        access.check(&p, ip(), 0),
        rejection("multiplayer.disconnect.not_whitelisted")
    );
    access.set_whitelisted(p.id, true).unwrap();
    assert_eq!(
        access.check(&p, ip(), 0),
        Some(json!({
            "translate": "multiplayer.disconnect.banned_ip.reason", "with": [{"text": "address"}]
        }))
    );
    assert!(access.remove_ip_ban(ip()));
    assert_eq!(
        access.check(&p, ip(), 0),
        rejection("multiplayer.disconnect.server_full")
    );
}

#[test]
fn operator_whitelist_and_capacity_bypasses_are_distinct_and_do_not_bypass_bans() {
    let mut access = LoginAccess::new(1);
    let p = profile(1);
    access.set_whitelist_enabled(true);
    access.set_operator(p.id, Some(false)).unwrap();
    assert_eq!(access.check(&p, ip(), 0), None);
    assert_eq!(
        access.check(&p, ip(), 1),
        rejection("multiplayer.disconnect.server_full")
    );
    access.set_operator(p.id, Some(true)).unwrap();
    assert_eq!(access.check(&p, ip(), usize::MAX), None);
    access.set_ip_ban(ip(), None, None).unwrap();
    assert_eq!(
        access.check(&p, ip(), 1).unwrap()["translate"],
        "multiplayer.disconnect.banned_ip.reason"
    );
    access.set_profile_ban(p.id, None, None).unwrap();
    assert_eq!(
        access.check(&p, ip(), 1).unwrap()["translate"],
        "multiplayer.disconnect.banned.reason"
    );
    access.remove_profile_ban(p.id);
    access.remove_ip_ban(ip());
    access.set_operator(p.id, None).unwrap();
    assert_eq!(
        access.check(&p, ip(), 0),
        rejection("multiplayer.disconnect.not_whitelisted")
    );
}

#[test]
fn profile_policies_use_uuid_and_preserve_literal_empty_and_default_reasons() {
    let mut access = LoginAccess::new(2);
    access.set_profile_ban(profile(1).id, None, None).unwrap();
    assert_eq!(access.check(&profile(2), ip(), 0), None);
    let mut renamed = profile(1);
    renamed.name = "new_name".into();
    assert_eq!(
        access.check(&renamed, ip(), 0),
        Some(json!({
            "translate": "multiplayer.disconnect.banned.reason",
            "with": [{"translate": "multiplayer.disconnect.banned.reason.default"}]
        }))
    );
    access.set_profile_ban(renamed.id, Some(""), None).unwrap();
    assert_eq!(
        access.check(&renamed, ip(), 0).unwrap()["with"],
        json!([{"text": ""}])
    );
    access
        .set_profile_ban(
            renamed.id,
            Some("{\"translate\":\"not_a_component\"}\n안녕"),
            None,
        )
        .unwrap();
    assert_eq!(
        access.check(&renamed, ip(), 0).unwrap()["with"],
        json!([{"text": "{\"translate\":\"not_a_component\"}\n안녕"}])
    );
}

#[test]
fn finite_ban_expiration_is_strict_and_uses_millisecond_precision() {
    let mut access = LoginAccess::new(1);
    let p = profile(1);
    let expires = UNIX_EPOCH + Duration::from_secs(1000);
    let display = "1970-01-01 at 00:16:40 UTC";
    access
        .set_profile_ban(p.id, None, Some((expires, display)))
        .unwrap();
    let component = json!({
        "translate": "multiplayer.disconnect.banned.reason",
        "with": [{"translate": "multiplayer.disconnect.banned.reason.default"}],
        "extra": [{"translate": "multiplayer.disconnect.banned.expiration", "with": [display]}]
    });
    for now in [
        expires - Duration::from_millis(1),
        expires,
        expires + Duration::from_micros(999),
    ] {
        assert_eq!(access.check_at(&p, ip(), 0, now), Some(component.clone()));
    }
    assert_eq!(
        access.check_at(&p, ip(), 0, expires + Duration::from_millis(1)),
        None
    );
    assert_eq!(access.purge_expired(expires), 0);
    assert_eq!(access.purge_expired(expires + Duration::from_millis(1)), 1);
}

#[test]
fn expired_profile_ban_falls_through_to_whitelist_and_then_active_ip_ban() {
    let mut access = LoginAccess::new(0);
    let p = profile(1);
    access.set_whitelist_enabled(true);
    access
        .set_profile_ban(
            p.id,
            Some("expired"),
            Some((UNIX_EPOCH, "1970-01-01 at 00:00:00 UTC")),
        )
        .unwrap();
    access.set_ip_ban(ip(), None, None).unwrap();
    let now = UNIX_EPOCH + Duration::from_millis(1);
    assert_eq!(
        access.check_at(&p, ip(), 0, now),
        rejection("multiplayer.disconnect.not_whitelisted")
    );
    access.set_whitelisted(p.id, true).unwrap();
    assert_eq!(
        access.check_at(&p, ip(), 0, now).unwrap()["translate"],
        "multiplayer.disconnect.banned_ip.reason"
    );
}

#[test]
fn ip_expiration_preserves_supplied_timezone_text_and_ignores_expired_entries() {
    let mut access = LoginAccess::new(1);
    let expires = UNIX_EPOCH + Duration::from_secs(1000);
    let display = "1970-01-01 at 09:16:40 KST";
    access
        .set_ip_ban(ip(), Some("reason"), Some((expires, display)))
        .unwrap();
    assert_eq!(
        access.check_at(&profile(1), ip(), 0, expires),
        Some(json!({
            "translate": "multiplayer.disconnect.banned_ip.reason", "with": [{"text": "reason"}],
            "extra": [{"translate": "multiplayer.disconnect.banned_ip.expiration", "with": [display]}]
        }))
    );
    assert_eq!(
        access.check_at(&profile(1), ip(), 0, expires + Duration::from_millis(1)),
        None
    );
    assert_eq!(access.purge_expired(expires + Duration::from_millis(1)), 1);
}

#[test]
fn pre_epoch_and_submillisecond_expirations_follow_signed_java_milliseconds() {
    let mut access = LoginAccess::new(1);
    let expires = UNIX_EPOCH - Duration::from_micros(1);
    access
        .set_profile_ban(
            profile(1).id,
            None,
            Some((expires, "1969-12-31 at 23:59:59 UTC")),
        )
        .unwrap();
    assert!(
        access
            .check_at(&profile(1), ip(), 0, UNIX_EPOCH - Duration::from_millis(1))
            .is_some()
    );
    assert!(access.check_at(&profile(1), ip(), 0, expires).is_some());
    assert_eq!(access.check_at(&profile(1), ip(), 0, UNIX_EPOCH), None);
}

#[test]
fn ipv4_and_ipv6_bans_are_exact_address_matches() {
    let mut access = LoginAccess::new(1);
    let v6: IpAddr = "2001:db8::1".parse().unwrap();
    access.set_ip_ban(v6, None, None).unwrap();
    assert!(
        access
            .check(&profile(1), "2001:db8:0:0:0:0:0:1".parse().unwrap(), 0)
            .is_some()
    );
    for address in [
        ip(),
        "2001:db8::2".parse().unwrap(),
        "::ffff:192.0.2.1".parse().unwrap(),
    ] {
        assert_eq!(access.check(&profile(1), address, 0), None);
    }
    access.set_ip_ban(ip(), None, None).unwrap();
    assert!(access.check(&profile(1), ip(), 0).is_some());
    let mapped = "::ffff:192.0.2.1".parse().unwrap();
    assert!(access.check(&profile(1), mapped, 0).is_some());
    assert!(access.remove_ip_ban(mapped));
    assert_eq!(access.check(&profile(1), ip(), 0), None);
    access.set_ip_ban(mapped, None, None).unwrap();
    assert!(access.check(&profile(1), ip(), 0).is_some());
    assert_eq!(
        access.check(&profile(1), "192.0.2.2".parse().unwrap(), 0),
        None
    );
}

#[test]
fn bounded_lists_allow_replacement_and_removal_without_partial_failed_updates() {
    let mut access = LoginAccess::with_limits(
        2,
        AccessLimits {
            max_entries_per_list: 1,
            max_reason_bytes: 6,
            max_expiration_bytes: 3,
        },
    );
    let p = profile(1);
    let other = profile(2);
    access.set_profile_ban(p.id, Some("안녕"), None).unwrap();
    assert_eq!(
        access.set_profile_ban(other.id, None, None),
        Err(AccessError::EntryLimit)
    );
    assert_eq!(
        access.set_profile_ban(p.id, Some("안녕!"), None),
        Err(AccessError::ReasonLimit)
    );
    assert_eq!(
        access.set_profile_ban(p.id, Some("new"), Some((UNIX_EPOCH, "long"))),
        Err(AccessError::ExpirationLimit)
    );
    assert_eq!(
        access.check(&p, ip(), 0).unwrap()["with"],
        json!([{"text": "안녕"}])
    );
    access.set_profile_ban(p.id, Some("new"), None).unwrap();
    assert!(access.remove_profile_ban(p.id));
    assert!(!access.remove_profile_ban(p.id));
    access.set_profile_ban(other.id, None, None).unwrap();

    access.set_ip_ban(ip(), None, None).unwrap();
    assert_eq!(
        access.set_ip_ban("::1".parse().unwrap(), None, None),
        Err(AccessError::EntryLimit)
    );
    access.set_ip_ban(ip(), Some("new"), None).unwrap();
    access.remove_ip_ban(ip());
    access
        .set_ip_ban("::1".parse().unwrap(), None, None)
        .unwrap();

    access.set_whitelisted(p.id, true).unwrap();
    access.set_whitelisted(p.id, true).unwrap();
    assert_eq!(
        access.set_whitelisted(other.id, true),
        Err(AccessError::EntryLimit)
    );
    access.set_whitelisted(p.id, false).unwrap();
    access.set_whitelisted(other.id, true).unwrap();

    access.set_operator(p.id, Some(false)).unwrap();
    access.set_operator(p.id, Some(true)).unwrap();
    assert_eq!(
        access.set_operator(other.id, Some(true)),
        Err(AccessError::EntryLimit)
    );
    access.set_operator(p.id, None).unwrap();
    access.set_operator(other.id, Some(false)).unwrap();
}

#[test]
fn expired_ban_storage_can_be_reclaimed_without_scanning_during_login() {
    let mut access = LoginAccess::with_limits(
        1,
        AccessLimits {
            max_entries_per_list: 1,
            ..AccessLimits::default()
        },
    );
    access
        .set_profile_ban(
            profile(1).id,
            None,
            Some((UNIX_EPOCH, "1970-01-01 at 00:00:00 UTC")),
        )
        .unwrap();
    let now = UNIX_EPOCH + Duration::from_millis(1);
    assert_eq!(access.check_at(&profile(1), ip(), 0, now), None);
    assert_eq!(
        access.set_profile_ban(profile(2).id, None, None),
        Err(AccessError::EntryLimit)
    );
    assert_eq!(access.purge_expired(now), 1);
    access.set_profile_ban(profile(2).id, None, None).unwrap();
    assert_eq!(access.purge_expired(now), 0);
}

#[test]
fn zero_list_limits_allow_checks_and_removal_but_reject_additions() {
    let mut access = LoginAccess::with_limits(
        1,
        AccessLimits {
            max_entries_per_list: 0,
            ..AccessLimits::default()
        },
    );
    let p = profile(1);
    assert_eq!(access.check(&p, ip(), 0), None);
    assert_eq!(
        access.set_profile_ban(p.id, None, None),
        Err(AccessError::EntryLimit)
    );
    assert_eq!(
        access.set_ip_ban(ip(), None, None),
        Err(AccessError::EntryLimit)
    );
    assert_eq!(
        access.set_whitelisted(p.id, true),
        Err(AccessError::EntryLimit)
    );
    assert_eq!(
        access.set_operator(p.id, Some(true)),
        Err(AccessError::EntryLimit)
    );
    access.set_whitelisted(p.id, false).unwrap();
    access.set_operator(p.id, None).unwrap();
}
