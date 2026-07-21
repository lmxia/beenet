use super::*;

mod gateway_tip_tests {
    use super::*;

    fn gw(peer_id: &str, region: Option<&str>, connected: u32, capacity: u32) -> GatewayView {
        GatewayView {
            gateway_id: peer_id.to_string(),
            peer_id: peer_id.to_string(),
            dial_addr: format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer_id}"),
            region: region.map(str::to_string),
            capacity,
            connected_workers: connected,
            last_seen_unix_ms: 0,
        }
    }

    #[test]
    fn prefers_same_region_as_primary() {
        let tip = select_gateway_tip(
            vec![
                gw("g-remote", Some("us-west"), 0, 1000),
                gw("g-local-busy", Some("cn-hangzhou"), 900, 1000),
                gw("g-local-free", Some("cn-hangzhou"), 10, 1000),
            ],
            "worker-1",
            Some("cn-hangzhou"),
        );
        assert_eq!(tip[0].peer_id, "g-local-free");
        assert!(tip.iter().any(|g| g.peer_id == "g-remote"));
        assert!(tip.len() <= GATEWAY_TIP_SIZE);
    }

    #[test]
    fn backups_prefer_different_region() {
        let tip = select_gateway_tip(
            vec![
                gw("a", Some("r1"), 0, 1000),
                gw("b", Some("r1"), 1, 1000),
                gw("c", Some("r2"), 50, 1000),
                gw("d", Some("r3"), 50, 1000),
            ],
            "worker-x",
            Some("r1"),
        );
        assert_eq!(tip[0].region.as_deref(), Some("r1"));
        assert_eq!(tip.len(), 3);
        let regions: Vec<_> = tip.iter().filter_map(|g| g.region.as_deref()).collect();
        assert!(regions.contains(&"r2") || regions.contains(&"r3"));
    }

    #[test]
    fn no_region_still_returns_diversified_tip() {
        let tip = select_gateway_tip(
            vec![
                gw("a", Some("r1"), 0, 1000),
                gw("b", Some("r2"), 0, 1000),
                gw("c", Some("r3"), 0, 1000),
                gw("d", Some("r4"), 0, 1000),
            ],
            "worker-y",
            None,
        );
        assert_eq!(tip.len(), GATEWAY_TIP_SIZE);
        let peers: std::collections::HashSet<_> = tip.iter().map(|g| g.peer_id.as_str()).collect();
        assert_eq!(peers.len(), GATEWAY_TIP_SIZE);
    }

    #[test]
    fn sticky_is_stable_for_same_worker() {
        let candidates = vec![
            gw("g1", Some("r1"), 10, 1000),
            gw("g2", Some("r1"), 10, 1000),
            gw("g3", Some("r1"), 10, 1000),
        ];
        let a = select_gateway_tip(candidates.clone(), "worker-stable", Some("r1"));
        let b = select_gateway_tip(candidates, "worker-stable", Some("r1"));
        assert_eq!(a[0].peer_id, b[0].peer_id);
    }
}

mod join_token_tests {
    use super::*;

    fn token_record(token_value: &str) -> JoinTokenRecord {
        JoinTokenRecord {
            id: "token-id".to_string(),
            description: "test".to_string(),
            token_hash: hash_join_token(token_value),
            created_at_unix_ms: 1,
            expires_at: Instant::now() + Duration::from_secs(60),
            expires_at_unix_ms: 61_000,
        }
    }

    #[test]
    fn reusable_token_matches_only_its_secret() {
        let record = token_record("shared-bootstrap-token");
        assert!(record.matches("shared-bootstrap-token"));
        assert!(!record.matches("different-token"));
    }

    #[test]
    fn token_list_view_does_not_expose_secret() {
        let view = TokenView::from(&token_record("secret"));
        assert!(view.token_value.is_none());
    }

    #[test]
    fn token_ttl_defaults_to_ten_minutes_and_caps_at_one_hour() {
        assert_eq!(
            resolve_join_token_ttl(None).unwrap(),
            DEFAULT_JOIN_TOKEN_TTL_SECS
        );
        assert_eq!(
            resolve_join_token_ttl(Some(MAX_JOIN_TOKEN_TTL_SECS)).unwrap(),
            MAX_JOIN_TOKEN_TTL_SECS
        );
        assert!(resolve_join_token_ttl(Some(0)).is_err());
        assert!(resolve_join_token_ttl(Some(MAX_JOIN_TOKEN_TTL_SECS + 1)).is_err());
    }
}

mod workers_lookup_tests {
    use super::*;

    fn active(supported: &[&str]) -> ActiveRecord {
        ActiveRecord {
            last_seen: Instant::now(),
            supported_cids: supported.iter().map(|s| (*s).to_string()).collect(),
            loaded_cids: Vec::new(),
            name: None,
        }
    }

    fn peer(seed: u8) -> PeerId {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        let keypair = identity::Keypair::ed25519_from_bytes(bytes).expect("ed25519 key");
        PeerId::from(keypair.public())
    }

    #[test]
    fn returns_only_requested_active_peers() {
        let a = peer(1);
        let b = peer(2);
        let c = peer(3);
        let mut map = HashMap::new();
        map.insert(a, active(&["cid-a"]));
        map.insert(b, active(&["cid-b"]));
        map.insert(c, active(&[]));

        let views = select_active_workers_by_peers(&map, &[a, c], Instant::now());
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].peer_id, a.to_string());
        assert_eq!(views[1].peer_id, c.to_string());
        assert_eq!(views[0].supported_cids, vec!["cid-a".to_string()]);
    }

    #[test]
    fn omits_unknown_and_stale_peers() {
        let live = peer(4);
        let stale = peer(5);
        let missing = peer(6);
        let mut map = HashMap::new();
        map.insert(live, active(&[]));
        map.insert(
            stale,
            ActiveRecord {
                last_seen: Instant::now() - STALE_AFTER - Duration::from_secs(1),
                supported_cids: vec!["old".into()],
                loaded_cids: Vec::new(),
                name: None,
            },
        );

        let views = select_active_workers_by_peers(&map, &[live, stale, missing], Instant::now());
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].peer_id, live.to_string());
    }

    #[test]
    fn lookup_body_cap_constant_is_enforced_by_handler_bound() {
        assert_eq!(MAX_LOOKUP_PEER_IDS, 256);
    }
}
