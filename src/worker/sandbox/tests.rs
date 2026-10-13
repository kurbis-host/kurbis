use super::*;

#[test]
fn isolated_network_policy_rejects_egress_ipv6_icc_and_unexpected_peers() {
    let valid = serde_json::json!({
        "Driver": "bridge",
        "Internal": true,
        "EnableIPv6": false,
        "Options": {
            "com.docker.network.bridge.enable_icc": "true",
            "com.docker.network.bridge.enable_ip_masquerade": "false"
        },
        "Containers": {"id": {"Name": "server"}}
    });
    assert!(verify_isolated_network_metadata(&valid, &["server".into()]).is_ok());
    for invalid in [
        serde_json::json!({"Driver":"bridge", "Internal":false, "EnableIPv6":false, "Options":{"com.docker.network.bridge.enable_icc":"true", "com.docker.network.bridge.enable_ip_masquerade":"false"}, "Containers":{"id":{"Name":"server"}}}),
        serde_json::json!({"Driver":"bridge", "Internal":true, "EnableIPv6":true, "Options":{"com.docker.network.bridge.enable_icc":"true", "com.docker.network.bridge.enable_ip_masquerade":"false"}, "Containers":{"id":{"Name":"server"}}}),
        serde_json::json!({"Driver":"bridge", "Internal":true, "EnableIPv6":false, "Options":{"com.docker.network.bridge.enable_icc":"false", "com.docker.network.bridge.enable_ip_masquerade":"false"}, "Containers":{"id":{"Name":"server"}}}),
        serde_json::json!({"Driver":"bridge", "Internal":true, "EnableIPv6":false, "Options":{"com.docker.network.bridge.enable_icc":"true", "com.docker.network.bridge.enable_ip_masquerade":"true"}, "Containers":{"id":{"Name":"server"}}}),
        serde_json::json!({"Driver":"bridge", "Internal":true, "EnableIPv6":false, "Options":{"com.docker.network.bridge.enable_icc":"true", "com.docker.network.bridge.enable_ip_masquerade":"false"}, "Containers":{"id":{"Name":"server"}, "other":{"Name":"sibling"}}}),
    ] {
        assert!(verify_isolated_network_metadata(&invalid, &["server".into()]).is_err());
    }
}

#[test]
fn runtime_capabilities_are_deterministic_and_bound_to_server_generation() {
    let key = "worker-root-key-that-is-long-enough-for-testing";
    let server = Uuid::new_v4();
    let token = scoped_runtime_token(key, server, 7).unwrap();
    assert_eq!(token, scoped_runtime_token(key, server, 7).unwrap());
    assert_ne!(token, scoped_runtime_token(key, server, 8).unwrap());
    assert_ne!(token, scoped_runtime_token(key, Uuid::new_v4(), 7).unwrap());
    assert_ne!(token, key);
    assert_eq!(token.len(), 64);
}

#[test]
fn network_identity_is_per_server_and_generation() {
    let id = Uuid::new_v4();
    assert_eq!(workload_network_name(id, 3), workload_network_name(id, 3));
    assert_ne!(workload_network_name(id, 3), workload_network_name(id, 4));
    assert_ne!(
        workload_network_name(id, 3),
        workload_network_name(Uuid::new_v4(), 3)
    );
}

#[test]
fn docker_policy_sets_isolation_and_hard_limits_without_host_mounts() {
    let args = DockerSandbox
        .run_security_args(
            WorkloadLimits::server(2 * 1024 * 1024 * 1024, SERVER_STORAGE_BYTES as u64, 8000),
            "2613:2613",
            "workload-net",
            true,
        )
        .unwrap();
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let memory_limit = args.iter().position(|arg| *arg == "--memory").unwrap();
    let swap_limit = args.iter().position(|arg| *arg == "--memory-swap").unwrap();
    assert_eq!(args[memory_limit + 1], args[swap_limit + 1]);
    for required in [
        "--network",
        "workload-net",
        "--memory",
        "--memory-swap",
        "--cpu-period",
        "--cpu-quota",
        "--pids-limit",
        "--read-only",
        "--cgroupns=private",
        "--read-only",
        "no-new-privileges:true",
        "--cap-drop",
        "ALL",
        "--log-driver",
        "local",
        "max-size=10m",
        "max-file=3",
    ] {
        assert!(
            args.contains(&required),
            "missing sandbox option {required}"
        );
    }
    assert!(!args.contains(&"--privileged"));
    assert!(!args.contains(&"/var/run/docker.sock"));
    // A server may burst to the worker's whole CPU budget: eight CPUs here.
    let cpu_quota = args.iter().position(|arg| *arg == "--cpu-quota").unwrap();
    assert_eq!(args[cpu_quota + 1], "800000");
}

#[test]
fn quota_mount_requires_an_exact_dedicated_loopback_ext4_root() {
    let target = Path::new("/var/lib/kurbis/servers/abc");
    let mountinfo = "42 30 7:2 / /var/lib/kurbis/servers/abc rw,relatime - ext4 /dev/loop4 rw";
    assert!(verify_dedicated_loop_mount(mountinfo, target).is_ok());
    assert!(
        verify_dedicated_loop_mount(
            "42 30 8:1 / /var/lib/kurbis/servers/abc rw - ext4 /dev/sda1 rw",
            target
        )
        .is_err()
    );
    assert!(
        verify_dedicated_loop_mount(
            "42 30 7:2 /world /var/lib/kurbis/servers/abc rw - ext4 /dev/loop4 rw",
            target
        )
        .is_err()
    );
    assert!(verify_dedicated_loop_mount(
            "42 30 7:2 / /var/lib/kurbis/servers/abc rw - ext4 /dev/loop4 rw\n43 30 7:2 / /mnt/duplicate rw - ext4 /dev/loop4 rw",
            target
        )
        .is_err());
}

#[test]
fn workload_limits_reject_any_unbounded_resource() {
    for limits in [
        WorkloadLimits {
            memory_bytes: 0,
            cpu_millis: 1,
            pid_limit: 1,
            disk_bytes: 1,
        },
        WorkloadLimits {
            memory_bytes: 1,
            cpu_millis: 0,
            pid_limit: 1,
            disk_bytes: 1,
        },
        WorkloadLimits {
            memory_bytes: 1,
            cpu_millis: 1,
            pid_limit: 0,
            disk_bytes: 1,
        },
        WorkloadLimits {
            memory_bytes: 1,
            cpu_millis: 1,
            pid_limit: 1,
            disk_bytes: 0,
        },
    ] {
        assert!(limits.validate().is_err());
    }
}

#[test]
fn runtime_environment_contains_only_the_scoped_capability() {
    let key = "worker-root-key-that-is-long-enough-for-testing";
    let server = Uuid::new_v4();
    let values = scoped_runtime_environment(key, server, 4).unwrap();
    assert_eq!(
        values[0],
        "PUMPKIN_HOST_CONTROL_SOCKET=/pumpkin/run/host-control.sock"
    );
    assert!(values[1].starts_with("PUMPKIN_HOST_CONTROL_TOKEN="));
    assert!(!values[1].contains(key));
    let docker_args = scoped_runtime_environment_args(key, server, 4).unwrap();
    assert_eq!(docker_args[0], "--env");
    assert_eq!(docker_args[1], values[0]);
    assert_eq!(docker_args[2], "--env");
    assert_eq!(docker_args[3], values[1]);
    assert!(!docker_args.iter().any(|value| value.contains("env-file")));
    let database_variable_name = concat!("DATABASE", "_URL");
    assert!(
        !docker_args
            .iter()
            .any(|value| value.contains(database_variable_name))
    );
    assert!(
        !docker_args
            .iter()
            .any(|value| value.contains("DOCKER_HOST"))
    );
}

#[test]
fn capability_derivation_rejects_weak_keys_and_invalid_generations() {
    let server = Uuid::new_v4();
    assert!(scoped_runtime_token("weak", server, 1).is_err());
    assert!(scoped_runtime_token(&"x".repeat(32), server, 0).is_err());
}

#[test]
fn workload_storage_owner_follows_the_rootless_daemons_real_id_map() {
    // /proc/<dockerd>/uid_map of the production rootless daemon: container
    // root is the daemon account and ids from 1 use the subordinate range.
    let id_map = "         0       1800          1\n         1     100000      65536\n     65537     180000      65536\n";
    assert_eq!(host_id_for(id_map, 0), Some(1800));
    assert_eq!(host_id_for(id_map, 1), Some(100_000));
    // Pumpkin's account. "Range start + id" would give 102613, which is the
    // host id of container uid 2614, and Pumpkin could not write its storage.
    assert_eq!(host_id_for(id_map, PUMPKIN_UID), Some(102_612));
    assert_eq!(host_id_for(id_map, 65_537), Some(180_000));
    assert_eq!(host_id_for(id_map, 200_000), None);
    assert_eq!(host_id_for("garbage", 1), None);
}

#[test]
fn daemon_start_time_is_read_past_a_command_name_with_spaces_and_parentheses() {
    // /proc/<pid>/stat of a rootless daemon; field 22 is its start time.
    let stat = "2059296 (dockerd) S 2059201 2059296 2059201 0 -1 4194560 31337 0 12 0 \
                 4821 1933 0 0 20 0 27 0 88213377 2412412928 19874 18446744073709551615";
    assert_eq!(process_start_ticks(stat), Some(88_213_377));
    let renamed = stat.replace("(dockerd)", "(my (odd) name)");
    assert_eq!(process_start_ticks(&renamed), Some(88_213_377));
    assert_eq!(process_start_ticks("2059296 (dockerd) S 1 2"), None);
    assert_eq!(process_start_ticks("garbage"), None);
}
