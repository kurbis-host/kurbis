use std::{env, fs, path::PathBuf};
fn main() {
    let path = "../runtime/pumpkin/assets/packets.json";
    println!("cargo:rerun-if-changed={path}");
    let packets: serde_json::Value =
        serde_json::from_slice(&fs::read(path).expect("release SDK packets"))
            .expect("packet table");
    let click = packets["serverbound"]["play"]["container_click"]
        .as_i64()
        .expect("container click ID");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("packet_ids.rs"),
        format!("const CONTAINER_CLICK: i32 = {click};\n"),
    )
    .unwrap();
}
