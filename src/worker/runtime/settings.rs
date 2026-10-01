use serde_json::Value;

pub(crate) fn render_pumpkin_config(settings: &Value, java_port: u16, bedrock_port: u16) -> String {
    let motd = setting_str(settings, "motd", "Hosted by kurbis");
    let difficulty = setting_str(settings, "difficulty", "Normal");
    let gamemode = setting_str(settings, "gamemode", "Survival");
    let view_distance = setting_u64(settings, "view_distance", 12).clamp(2, 32);
    let simulation_distance = setting_u64(settings, "simulation_distance", 8).clamp(2, 32);
    let hardcore = setting_bool(settings, "hardcore", false);
    let whitelist = setting_bool(settings, "whitelist", false);
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
         [networking.bedrock]\n\
         enabled = true\n\
         online_mode = true\n\
         encryption = true\n\
         address = \"0.0.0.0:{bedrock_port}\"\n\
         max_players = 0\n\
         motd = {motd}\n\
         view_distance = {view_distance}\n\
         simulation_distance = {simulation_distance}\n\
         [networking.bedrock.authentication]\n\
         enabled = true\n",
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
        let config = render_pumpkin_config(&Value::Null, 25565, 19132);
        assert!(
            config.contains(
                "[networking.java]\nenabled = true\nonline_mode = true\nencryption = true"
            )
        );
        assert!(config.contains(
            "[networking.bedrock]\nenabled = true\nonline_mode = true\nencryption = true"
        ));
        assert!(config.contains("[networking.bedrock.authentication]\nenabled = true"));
        assert!(config.contains(
            "[networking.bedrock]\nenabled = true\nonline_mode = true\nencryption = true\naddress = \"0.0.0.0:19132\""
        ));
    }
}
