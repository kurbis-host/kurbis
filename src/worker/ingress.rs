use std::{
    net::{IpAddr, SocketAddr},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use uuid::Uuid;

const NFT_TABLE: &str = "kurbis_workload_ingress";
const NFT_CHAIN: &str = "gateway_only";
const NFT_TIMEOUT: Duration = Duration::from_secs(10);

mod relay;
pub use relay::{cleanup_ingress_through, ensure_runtime_ingress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishedWorkloadPorts {
    pub java_tcp: SocketAddr,
    pub bedrock_udp: SocketAddr,
    pub bedrock_signal_tcp: SocketAddr,
}

#[derive(Debug, Clone, Copy)]
pub struct GatewayIngressPolicy {
    gateway_address: IpAddr,
}

impl GatewayIngressPolicy {
    pub fn new(gateway_address: IpAddr) -> Result<Self> {
        if gateway_address.is_unspecified() {
            bail!("Gateway ingress source address must be concrete");
        }
        Ok(Self { gateway_address })
    }

    /// Install an allow-from-Gateway rule followed by a drop rule for each
    /// published host port. Rules live in a dedicated nftables table and never
    /// replace or flush other host firewall policy.
    pub async fn install(
        &self,
        server_id: Uuid,
        generation: i64,
        ports: PublishedWorkloadPorts,
    ) -> Result<()> {
        if generation <= 0 {
            bail!("Gateway ingress policy requires a positive generation");
        }
        validate_published_ports(self.gateway_address, ports)?;
        self.ensure_chain().await?;
        if self
            .verify_rules(server_id, generation, ports)
            .await
            .is_ok()
        {
            return Ok(());
        }
        self.remove_generation(server_id, generation).await?;
        let mut script = String::new();
        for (protocol, socket, allowed_source) in ingress_bindings(self.gateway_address, ports) {
            let prefix = rule_prefix(server_id, generation, protocol, socket);
            let allow = self.rule_from(protocol, socket, &prefix, allowed_source, true);
            let drop = self.rule_from(protocol, socket, &prefix, allowed_source, false);
            script.push_str(&format!("add rule inet {NFT_TABLE} {NFT_CHAIN} {allow}\nadd rule inet {NFT_TABLE} {NFT_CHAIN} {drop}\n"));
        }
        self.nft_batch(&script).await?;
        if let Err(error) = self.verify_rules(server_id, generation, ports).await {
            self.remove_generation(server_id, generation).await?;
            return Err(error).context("verify installed Gateway-only ingress rules");
        }
        Ok(())
    }

    /// Remove generation-tagged host admission rules without allowing a stale
    /// worker to erase a newer generation's policy.
    pub async fn remove_generations_through(
        &self,
        server_id: Uuid,
        max_generation: i64,
    ) -> Result<()> {
        if max_generation <= 0 {
            bail!("ingress policy cleanup requires a positive generation");
        }
        self.ensure_chain().await?;
        let rules = self.list_rules().await?;
        let server_prefix = format!("kurbis:{server_id}:");
        let mut handles = Vec::new();
        for rule in &rules {
            let Some(comment) = rule.get("comment").and_then(Value::as_str) else {
                continue;
            };
            let Some(suffix) = comment.strip_prefix(&server_prefix) else {
                continue;
            };
            let Some(generation) = suffix
                .split(':')
                .next()
                .and_then(|value| value.parse::<i64>().ok())
            else {
                bail!("refusing to remove malformed Gateway ingress rule {comment}");
            };
            if generation > max_generation {
                bail!("refusing stale Gateway ingress cleanup of newer generation {generation}");
            }
            if generation > 0
                && let Some(handle) = rule.get("handle").and_then(Value::as_u64)
            {
                handles.push(handle);
            }
        }
        handles.sort_unstable_by(|left, right| right.cmp(left));
        for handle in handles {
            self.nft([
                "delete",
                "rule",
                "inet",
                NFT_TABLE,
                NFT_CHAIN,
                "handle",
                &handle.to_string(),
            ])
            .await?;
        }
        Ok(())
    }

    pub async fn remove_generation(&self, server_id: Uuid, generation: i64) -> Result<()> {
        if generation <= 0 {
            bail!("ingress policy cleanup requires a positive generation");
        }
        self.ensure_chain().await?;
        let rules = self.list_rules().await?;
        let prefix = format!("kurbis:{server_id}:{generation}:");
        let mut handles = rules
            .iter()
            .filter(|rule| {
                rule.get("comment")
                    .and_then(Value::as_str)
                    .is_some_and(|comment| comment.starts_with(&prefix))
            })
            .filter_map(|rule| rule.get("handle").and_then(Value::as_u64))
            .collect::<Vec<_>>();
        handles.sort_unstable_by(|left, right| right.cmp(left));
        for handle in handles {
            self.nft([
                "delete",
                "rule",
                "inet",
                NFT_TABLE,
                NFT_CHAIN,
                "handle",
                &handle.to_string(),
            ])
            .await?;
        }
        Ok(())
    }

    /// Confirm the worker can read and mutate only the dedicated firewall chain.
    /// This is run before the worker accepts any public workload assignments.
    pub async fn verify_host_policy(&self) -> Result<()> {
        self.ensure_chain().await?;
        let prefix = format!("kurbis:preflight:{}", Uuid::new_v4());
        let socket = SocketAddr::new(self.gateway_address, 1);
        let allow = self.rule("tcp", socket, &prefix, true);
        let drop = self.rule("tcp", socket, &prefix, false);
        self.nft_batch(&format!(
            "add rule inet {NFT_TABLE} {NFT_CHAIN} {allow}\nadd rule inet {NFT_TABLE} {NFT_CHAIN} {drop}\n"
        ))
        .await?;
        let handles = self
            .list_rules()
            .await?
            .into_iter()
            .filter(|rule| {
                rule.get("comment")
                    .and_then(Value::as_str)
                    .is_some_and(|comment| comment.starts_with(&prefix))
            })
            .filter_map(|rule| rule.get("handle").and_then(Value::as_u64))
            .collect::<Vec<_>>();
        if handles.len() != 2 {
            bail!("nftables preflight could not verify allow/drop rule insertion");
        }
        for handle in handles.into_iter().rev() {
            self.nft([
                "delete",
                "rule",
                "inet",
                NFT_TABLE,
                NFT_CHAIN,
                "handle",
                &handle.to_string(),
            ])
            .await?;
        }
        Ok(())
    }

    fn rule(&self, protocol: &str, socket: SocketAddr, prefix: &str, allow: bool) -> String {
        self.rule_from(protocol, socket, prefix, self.gateway_address, allow)
    }

    fn rule_from(
        &self,
        protocol: &str,
        socket: SocketAddr,
        prefix: &str,
        allowed_source: IpAddr,
        allow: bool,
    ) -> String {
        let (source_field, destination_field) = if socket.is_ipv4() {
            ("ip saddr", "ip daddr")
        } else {
            ("ip6 saddr", "ip6 daddr")
        };
        let verdict = if allow { "accept" } else { "drop" };
        let label = if allow { "allow" } else { "drop" };
        let source = if allow {
            if socket.is_ipv4() != allowed_source.is_ipv4() {
                return String::new();
            }
            format!("{source_field} {} ", allowed_source)
        } else {
            String::new()
        };
        format!(
            "{source}{destination_field} {} {protocol} dport {} {verdict} comment \"{prefix}:{label}\"",
            socket.ip(),
            socket.port()
        )
    }

    async fn ensure_chain(&self) -> Result<()> {
        let table = self.nft_output(["list", "table", "inet", NFT_TABLE]).await;
        if let Err(error) = table {
            let message = format!("{error:#}");
            if !message.contains("No such file or directory") {
                return Err(error).context("inspect workload ingress nftables table");
            }
            self.nft(["add", "table", "inet", NFT_TABLE]).await?;
        }
        let chain = self
            .nft_output(["list", "chain", "inet", NFT_TABLE, NFT_CHAIN])
            .await;
        if let Err(error) = chain {
            let message = format!("{error:#}");
            if !message.contains("No such file or directory") {
                return Err(error).context("inspect workload ingress nftables chain");
            }
            self.nft_batch(&format!(
                "add chain inet {NFT_TABLE} {NFT_CHAIN} {{ type filter hook input priority -5; policy accept; }}\n"
            ))
            .await?;
        } else {
            let chain = chain?;
            if !valid_input_chain_declaration(&chain) {
                bail!(
                    "existing workload ingress chain has an unexpected hook, priority, or policy"
                );
            }
        }
        Ok(())
    }

    async fn verify_rules(
        &self,
        server_id: Uuid,
        generation: i64,
        ports: PublishedWorkloadPorts,
    ) -> Result<()> {
        let rules = self.list_rules().await?;
        for (protocol, socket, allowed_source) in ingress_bindings(self.gateway_address, ports) {
            let prefix = rule_prefix(server_id, generation, protocol, socket);
            let allow = find_verified_rule(
                &rules,
                &format!("{prefix}:allow"),
                allowed_source,
                protocol,
                socket,
                true,
            )
            .context(format!(
                "configured source allow rule {prefix} is missing or has been altered"
            ))?;
            let drop = find_verified_rule(
                &rules,
                &format!("{prefix}:drop"),
                allowed_source,
                protocol,
                socket,
                false,
            )
            .context(format!(
                "non-allowed source drop rule {prefix} is missing or has been altered"
            ))?;
            if allow >= drop {
                bail!("Gateway ingress allow rule must precede the port drop rule for {prefix}");
            }
        }
        Ok(())
    }

    async fn list_rules(&self) -> Result<Vec<Value>> {
        let json = self
            .nft_output(["-j", "-a", "list", "chain", "inet", NFT_TABLE, NFT_CHAIN])
            .await?;
        let value: Value = serde_json::from_str(&json).context("nft returned invalid JSON")?;
        Ok(value
            .get("nftables")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("rule").cloned())
            .collect())
    }

    async fn nft<const N: usize>(&self, args: [&str; N]) -> Result<()> {
        self.nft_output(args).await.map(|_| ())
    }

    async fn nft_output<const N: usize>(&self, args: [&str; N]) -> Result<String> {
        let output = timeout(
            NFT_TIMEOUT,
            Command::new("nft")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("nftables command timed out")??;
        if !output.status.success() {
            bail!(
                "nftables command failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn nft_batch(&self, script: &str) -> Result<()> {
        let mut child = Command::new("nft")
            .arg("-f")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().context("nft stdin is unavailable")?;
        stdin.write_all(script.as_bytes()).await?;
        drop(stdin);
        let output = timeout(NFT_TIMEOUT, child.wait_with_output())
            .await
            .context("nftables batch timed out")??;
        if !output.status.success() {
            bail!(
                "nftables policy update failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

fn valid_input_chain_declaration(chain: &str) -> bool {
    let priority_is_minus_five =
        chain.contains("priority -5") || chain.contains("priority filter - 5");
    chain.contains("type filter")
        && chain.contains("hook input")
        && priority_is_minus_five
        && chain.contains("policy accept")
}

fn validate_published_ports(gateway_address: IpAddr, ports: PublishedWorkloadPorts) -> Result<()> {
    let bindings = [
        ("tcp", ports.java_tcp),
        ("udp", ports.bedrock_udp),
        ("tcp", ports.bedrock_signal_tcp),
    ];
    for (_, socket) in bindings {
        if socket.port() <= 1024
            || socket.ip().is_unspecified()
            || socket.ip().is_ipv4() != gateway_address.is_ipv4()
        {
            bail!(
                "Gateway-only workload ports require concrete worker addresses and unprivileged ports"
            );
        }
    }
    if ports.java_tcp == ports.bedrock_signal_tcp {
        bail!("Java and Bedrock signaling cannot share a TCP worker port");
    }
    Ok(())
}

fn find_verified_rule(
    rules: &[Value],
    comment: &str,
    gateway_address: IpAddr,
    protocol: &str,
    socket: SocketAddr,
    allow: bool,
) -> Option<u64> {
    rules.iter().find_map(|rule| {
        if rule.get("comment").and_then(Value::as_str) != Some(comment) {
            return None;
        }
        let expressions = rule.get("expr")?.as_array()?;
        if expressions.len() != if allow { 4 } else { 3 } {
            return None;
        }
        let mut destination_matches = false;
        let mut source_matches = !allow;
        let mut unexpected_source_match = false;
        let mut port_matches = false;
        let mut verdict_matches = false;
        for expression in expressions {
            let Some(expression) = expression.as_object() else {
                continue;
            };
            if (allow && expression.contains_key("accept"))
                || (!allow && expression.contains_key("drop"))
            {
                verdict_matches = true;
            }
            let Some(condition) = expression.get("match") else {
                continue;
            };
            if condition.get("op").and_then(Value::as_str) != Some("==") {
                continue;
            }
            let Some(left) = condition.get("left") else {
                continue;
            };
            let Some(payload) = left.get("payload") else {
                continue;
            };
            let Some(field) = payload.get("field").and_then(Value::as_str) else {
                continue;
            };
            if !allow && field == "saddr" {
                unexpected_source_match = true;
            }
            let payload_protocol = payload.get("protocol").and_then(Value::as_str)?;
            let right = condition.get("right")?;
            match field {
                "saddr" if allow => {
                    let expected_protocol = if gateway_address.is_ipv4() {
                        "ip"
                    } else {
                        "ip6"
                    };
                    source_matches |= payload_protocol == expected_protocol
                        && right.as_str() == Some(&gateway_address.to_string());
                }
                "daddr" => {
                    let expected_protocol = if socket.is_ipv4() { "ip" } else { "ip6" };
                    destination_matches |= payload_protocol == expected_protocol
                        && right.as_str() == Some(&socket.ip().to_string());
                }
                "dport" => {
                    port_matches |= payload_protocol == protocol
                        && right.as_u64() == Some(u64::from(socket.port()));
                }
                _ => {}
            }
        }
        (destination_matches
            && source_matches
            && !unexpected_source_match
            && port_matches
            && verdict_matches)
            .then(|| rule.get("handle").and_then(Value::as_u64))
            .flatten()
    })
}

pub fn docker_publish_binding(
    bind_address: IpAddr,
    host_port: u16,
    container_port: u16,
    protocol: &str,
) -> Result<String> {
    if host_port <= 1024 || container_port == 0 || !matches!(protocol, "tcp" | "udp") {
        bail!("invalid Gateway-only workload port binding");
    }
    let address = match bind_address {
        IpAddr::V4(address) if !address.is_unspecified() => address.to_string(),
        IpAddr::V6(address) if !address.is_unspecified() => format!("[{address}]"),
        _ => bail!("workload ports require a concrete worker address"),
    };
    Ok(format!("{address}:{host_port}:{container_port}/{protocol}"))
}

fn ingress_bindings(
    gateway_address: IpAddr,
    ports: PublishedWorkloadPorts,
) -> Vec<(&'static str, SocketAddr, IpAddr)> {
    let mut bindings = vec![
        ("tcp", ports.java_tcp, gateway_address),
        ("udp", ports.bedrock_udp, gateway_address),
        ("tcp", ports.bedrock_signal_tcp, gateway_address),
    ];
    let loopback = if ports.java_tcp.is_ipv4() {
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else {
        IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
    };
    if ports.java_tcp.ip() != loopback {
        bindings.push((
            "tcp",
            SocketAddr::new(loopback, ports.java_tcp.port()),
            loopback,
        ));
    }
    bindings
}

fn rule_prefix(server_id: Uuid, generation: i64, protocol: &str, socket: SocketAddr) -> String {
    format!(
        "kurbis:{server_id}:{generation}:{protocol}:{}:{}",
        socket.ip(),
        socket.port()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_hook_accepts_equivalent_nft_priority_renderings_only() {
        assert!(valid_input_chain_declaration(
            "chain gateway_only { type filter hook input priority filter - 5; policy accept; }"
        ));
        assert!(valid_input_chain_declaration(
            "chain gateway_only { type filter hook input priority -5; policy accept; }"
        ));
        assert!(!valid_input_chain_declaration(
            "chain gateway_only { type filter hook input priority filter; policy accept; }"
        ));
        assert!(!valid_input_chain_declaration(
            "chain gateway_only { type filter hook output priority filter - 5; policy accept; }"
        ));
        assert!(!valid_input_chain_declaration(
            "chain gateway_only { type filter hook input priority filter - 5; policy drop; }"
        ));
    }

    #[test]
    fn verified_ingress_rejects_a_tampered_rule_even_when_comment_remains() {
        let gateway: IpAddr = "10.9.0.2".parse().unwrap();
        let socket: SocketAddr = "10.9.0.4:23123".parse().unwrap();
        let allow = serde_json::json!({"comment":"kurbis:test:7:tcp:23123:allow", "handle": 2, "expr":[
            {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"10.9.0.2"}},
            {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"10.9.0.4"}},
            {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":23123}},
            {"accept":null}
        ]});
        let drop = serde_json::json!({"comment":"kurbis:test:7:tcp:23123:drop", "handle": 3, "expr":[
            {"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"daddr"}},"right":"10.9.0.4"}},
            {"match":{"op":"==","left":{"payload":{"protocol":"tcp","field":"dport"}},"right":23123}},
            {"drop":null}
        ]});
        assert!(
            find_verified_rule(
                &[allow.clone(), drop.clone()],
                "kurbis:test:7:tcp:23123:allow",
                gateway,
                "tcp",
                socket,
                true
            )
            .is_some()
        );
        assert!(
            find_verified_rule(
                &[allow.clone(), drop.clone()],
                "kurbis:test:7:tcp:23123:drop",
                gateway,
                "tcp",
                socket,
                false
            )
            .is_some()
        );
        let mut weakened_drop = drop.clone();
        weakened_drop["expr"].as_array_mut().unwrap().insert(
            0,
            serde_json::json!({"match":{"op":"==","left":{"payload":{"protocol":"ip","field":"saddr"}},"right":"192.0.2.77"}}),
        );
        assert!(
            find_verified_rule(
                &[weakened_drop],
                "kurbis:test:7:tcp:23123:drop",
                gateway,
                "tcp",
                socket,
                false
            )
            .is_none()
        );
        let mut tampered = allow;
        tampered["expr"][0]["match"]["right"] = Value::String("192.0.2.77".into());
        assert!(
            find_verified_rule(
                &[tampered],
                "kurbis:test:7:tcp:23123:allow",
                gateway,
                "tcp",
                socket,
                true
            )
            .is_none()
        );
    }

    #[test]
    fn firewall_rules_allow_only_gateway_source_and_drop_other_sources() {
        let policy = GatewayIngressPolicy::new("10.9.0.2".parse().unwrap()).unwrap();
        let id = Uuid::new_v4();
        let socket = "10.9.0.4:23123".parse().unwrap();
        let allow = policy.rule("tcp", socket, &rule_prefix(id, 7, "tcp", socket), true);
        let drop = policy.rule("tcp", socket, &rule_prefix(id, 7, "tcp", socket), false);
        assert!(allow.contains("ip saddr 10.9.0.2 ip daddr 10.9.0.4 tcp dport 23123 accept"));
        assert!(drop.contains("ip daddr 10.9.0.4 tcp dport 23123 drop"));
        assert!(!drop.contains("ip saddr"));
        assert!(rule_prefix(id, 7, "tcp", socket).starts_with(&format!("kurbis:{id}:7:")));
        assert_eq!(
            docker_publish_binding("10.9.0.4".parse().unwrap(), 23123, 25565, "tcp").unwrap(),
            "10.9.0.4:23123:25565/tcp"
        );
        assert_eq!(
            docker_publish_binding("fd00::4".parse().unwrap(), 23123, 19132, "udp").unwrap(),
            "[fd00::4]:23123:19132/udp"
        );
        assert!(docker_publish_binding("0.0.0.0".parse().unwrap(), 23123, 25565, "tcp").is_err());
    }
}
