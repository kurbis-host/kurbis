"""Build real WASM revisions used to prove that live command behavior changes."""
import pathlib
import subprocess

SOURCE = '''use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, register_plugin,
    command::{Command, CommandSender, CommandError, ConsumedArgs}, commands::CommandHandler,
    text::TextComponent, permission::{Permission, PermissionDefault, PermissionLevel}};
struct Probe;
struct Handler;
impl CommandHandler for Handler {
    fn handle(&self, sender: CommandSender, _: Server, _: ConsumedArgs) -> Result<i32, CommandError> {
        sender.send_message(TextComponent::text("RELOAD-PROBE-VERSION"));
        Ok(1)
    }
}
impl Plugin for Probe {
    fn new() -> Self { Self }
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata { name: "reload-probe".into(), version: "0.1.0".into(), authors: vec![],
            description: "Reload integration fixture".into(), dependencies: vec![], permissions: vec![] }
    }
    fn on_load(&self, context: Context) -> Result<(), String> {
        vibe_plugin_support::register_permission(&context, &Permission {
            node: "reload-probe:command".into(), description: "Fixture command".into(),
            default: PermissionDefault::Op(PermissionLevel::One), children: vec![],
        })?;
        FAIL_LOAD
        context.register_command(Command::new(&["reloadprobe".into()], "Show loaded revision").execute(Handler), "reload-probe:command");
        Ok(())
    }
}
register_plugin!(Probe);
'''


def build(root, directory):
    directory.mkdir()
    (directory / "src").mkdir()
    sdk = root / "runtime/pumpkin/crates/pumpkin-plugin-api"
    (directory / "Cargo.toml").write_text(f'''[package]
name = "reload-probe"
version = "0.1.0"
edition = "2024"
[lib]
crate-type = ["cdylib"]
[dependencies]
pumpkin-plugin-api = {{ path = "{sdk}" }}
vibe-plugin-support = {{ path = "{root / 'plugin-support'}" }}
[workspace]
''')
    target = root / "vibe-bridge/target"
    artifacts = {}
    for version in ("one", "two", "broken", "namespace", "fresh"):
        failure = 'return Err("fixture initialization rejected".into());' if version in ("broken", "fresh") else ""
        source = SOURCE.replace("VERSION", version).replace("FAIL_LOAD", failure)
        if version == "fresh":
            source = source.replace("reload-probe", "fresh-probe")
        if version == "namespace":
            source = source.replace('node: "reload-probe:command"', 'node: "other:command"')
        (directory / "src/lib.rs").write_text(source)
        subprocess.run(["cargo", "build", "--manifest-path", str(directory / "Cargo.toml"),
                        "--release", "--target", "wasm32-wasip2", "--target-dir", str(target)], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        artifact = directory / (version + ".wasm")
        artifact.write_bytes((target / "wasm32-wasip2/release/reload_probe.wasm").read_bytes())
        artifacts[version] = str(artifact)
    return artifacts
