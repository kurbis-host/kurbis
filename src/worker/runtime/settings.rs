use std::net::{IpAddr, SocketAddr};

use serde_json::Value;

use crate::{
    model::DEFAULT_SERVER_MOTD,
    runtime_api_proxy::{MINECRAFT_SERVICES_PREFIX, SERVICE_BASE_URL},
};

/// Where a server asks Mojang and Microsoft about joining accounts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccountLookups {
    /// The server has its own route to the internet.
    Direct,
    /// The sandbox has no route out; the server's API proxy carries the
    /// lookups to the worker.
    ThroughApiProxy,
}

pub(crate) fn render_pumpkin_config(
    settings: &Value,
    account_lookups: AccountLookups,
    java_port: u16,
    bedrock_port: u16,
    signal_address: SocketAddr,
    external_ip: IpAddr,
    external_port: u16,
) -> String {
    let motd = setting_str(settings, "motd", DEFAULT_SERVER_MOTD);
    let difficulty = setting_str(settings, "difficulty", "Normal");
    let gamemode = setting_str(settings, "gamemode", "Survival");
    let view_distance = setting_u64(settings, "view_distance", 12).clamp(2, 32);
    let simulation_distance = setting_u64(settings, "simulation_distance", 8).clamp(2, 32);
    let hardcore = setting_bool(settings, "hardcore", false);
    let whitelist = setting_bool(settings, "whitelist", false);
    let services = format!("{SERVICE_BASE_URL}{MINECRAFT_SERVICES_PREFIX}");
    let (java_authentication, bedrock_authentication) = match account_lookups {
        AccountLookups::Direct => (String::new(), String::new()),
        AccountLookups::ThroughApiProxy => (
            format!(
                "[networking.java.authentication]\n\
                 url = \"{services}session/hasJoined?username={{username}}&serverId={{server_hash}}\"\n\
                 services_url = \"{services}services\"\n\
                 profile_by_name_url = \"{services}profile/name/{{username}}\"\n\
                 profile_by_uuid_url = \"{services}profile/uuid/{{uuid}}\"\n"
            ),
            format!("url = \"{services}bedrock/discovery\"\n"),
        ),
    };
    format!(
        "default_difficulty = {difficulty}\n\
         default_gamemode = {gamemode}\n\
         hardcore = {hardcore}\n\
         white_list = {whitelist}\n\
         enforce_whitelist = {whitelist}\n\
         accepts_transfers = true\n\
         [networking.query]\n\
         enabled = false\n\
         [networking.rcon]\n\
         enabled = false\n\
         [networking.java]\n\
         enabled = true\n\
         online_mode = true\n\
         encryption = true\n\
         address = \"0.0.0.0:{java_port}\"\n\
         max_players = 0\n\
         motd = {motd}\n\
         view_distance = {view_distance}\n\
         simulation_distance = {simulation_distance}\n\
         {java_authentication}\
         [networking.bedrock]\n\
         enabled = true\n\
         online_mode = true\n\
         max_players = 0\n\
         motd = {motd}\n\
         view_distance = {view_distance}\n\
         simulation_distance = {simulation_distance}\n\
         [networking.bedrock.authentication]\n\
         enabled = true\n\
         {bedrock_authentication}\
         [networking.bedrock.nethernet]\n\
         enabled = true\n\
         address = \"0.0.0.0:{bedrock_port}\"\n\
         signal_address = \"{signal_address}\"\n\
         external_ip = \"{external_ip}\"\n\
         external_port = {external_port}\n\
         identity_key = \"nethernet-key.der\"\n\
         stun_servers = []\n\
         [plugins]\n\
         allow_unsigned = false\n\
         [plugins.overrides.kurbis-control]\n\
         enabled = true\n\
         allow_unsigned = true\n\
         allowed_permissions = [\"fs.read.data\", \"http.outbound\"]\n",
        difficulty = serde_json::to_string(difficulty).expect("string serialization cannot fail"),
        gamemode = serde_json::to_string(gamemode).expect("string serialization cannot fail"),
        motd = serde_json::to_string(motd).expect("string serialization cannot fail"),
    )
}

fn setting_str<'a>(settings: &'a Value, key: &str, default: &'a str) -> &'a str {
    settings.get(key).and_then(Value::as_str).unwrap_or(default)
}

fn setting_u64(settings: &Value, key: &str, default: u64) -> u64 {
    settings.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn setting_bool(settings: &Value, key: &str, default: bool) -> bool {
    settings
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_runtime_requires_online_java_and_full_bedrock_authentication() {
        let config = render_pumpkin_config(
            &Value::Null,
            AccountLookups::ThroughApiProxy,
            25565,
            19132,
            "0.0.0.0:19132".parse().unwrap(),
            "192.0.2.20".parse().unwrap(),
            19132,
        );
        assert!(
            config.contains(
                "[networking.java]\nenabled = true\nonline_mode = true\nencryption = true"
            )
        );
        assert!(config.contains("[networking.bedrock]\nenabled = true\nonline_mode = true"));
        assert!(config.contains("[networking.bedrock.authentication]\nenabled = true"));
        assert!(config.contains(
            "[networking.java.authentication]\nurl = \"http://kurbis-runtime-api-proxy:8080/minecraft/session/hasJoined?username={username}&serverId={server_hash}\""
        ));
        assert!(config.contains(
            "url = \"http://kurbis-runtime-api-proxy:8080/minecraft/bedrock/discovery\""
        ));
        assert!(config.contains(
            "[networking.bedrock.nethernet]\nenabled = true\naddress = \"0.0.0.0:19132\"\nsignal_address = \"0.0.0.0:19132\"\nexternal_ip = \"192.0.2.20\"\nexternal_port = 19132\nidentity_key = \"nethernet-key.der\"\nstun_servers = []"
        ));
        assert!(!config.contains("[networking.bedrock]\nencryption ="));
    }

    #[test]
    fn hosted_runtime_grants_only_the_managed_plugin_permissions() {
        let config = render_pumpkin_config(
            &Value::Null,
            AccountLookups::ThroughApiProxy,
            25565,
            19132,
            "0.0.0.0:19132".parse().unwrap(),
            "192.0.2.20".parse().unwrap(),
            19132,
        );
        assert!(config.contains("[plugins]\nallow_unsigned = false"));
        assert!(config.contains(
            "[plugins.overrides.kurbis-control]\nenabled = true\nallow_unsigned = true\nallowed_permissions = [\"fs.read.data\", \"http.outbound\"]"
        ));
        assert!(!config.contains("ask_permission_confirmation = false"));
    }
}
