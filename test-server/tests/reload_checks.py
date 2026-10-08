"""Exercise build, reload, rollback and failure recovery through the workshop."""
import json
import time


def exercise(op, guest, data, eventually):
    def submit(command, expected):
        existing = set((data / "results").glob("*.json"))
        op.command(command)
        result_path = eventually(lambda: next(iter(set((data / "results").glob("*.json")) - existing), None))
        result = eventually(lambda: (r if r["state"] in ("installed", "completed", "failed") else None)
                            if (r := json.loads(result_path.read_text())) else None, timeout=30)
        assert result["state"] == expected, result
        return result

    def loaded(version):
        start = len(op.packets)
        op.command("reloadprobe")
        eventually(lambda: op.seen("RELOAD-PROBE-" + version, start))
        assert not op.seen("RELOAD-PROBE-" + ("two" if version == "one" else "one"), start)

    fresh = submit("ai code fresh-probe fresh", "failed")
    assert "failed artifact removed" in fresh["message"]
    assert not (data.parents[1] / "fresh-probe.wasm").exists()
    first = submit("ai code reload-probe one", "installed")
    assert "reloaded without restarting" in first["message"]
    loaded("one")
    submit("ai code reload-probe two", "installed")
    loaded("two")
    op.menu("Workshop", lambda: op.command("ai"))
    op.menu("Plugins", lambda: op.click(20))
    op.menu("Plugin controls", lambda: op.click(10))
    before = set((data / "results").glob("*.json"))
    op.click(20)
    path = eventually(lambda: next(iter(set((data / "results").glob("*.json")) - before), None))
    eventually(lambda: json.loads(path.read_text())["state"] == "completed")
    loaded("two")
    submit("ai revert reload-probe", "installed")
    loaded("one")
    before_artifact = (data.parents[1] / "reload-probe.wasm").read_bytes()
    before_index = json.loads((data / "plugins.json").read_text())
    failed = submit("ai code reload-probe broken", "failed")
    assert "previous artifact restored" in failed["message"]
    loaded("one")
    events = (data / "events" / (failed["id"] + ".jsonl")).read_text()
    sequences = [json.loads(line)["sequence"] for line in events.splitlines()]
    assert sequences == list(range(1, len(sequences) + 1)), "Reload logs lost event ordering"
    assert "fixture initialization rejected" in events and "Previous plugin reactivation" in events
    assert (data.parents[1] / "reload-probe.wasm").read_bytes() == before_artifact
    assert json.loads((data / "plugins.json").read_text()) == before_index
    namespace = submit("ai code reload-probe namespace", "failed")
    assert "must use the plugin's namespace" in namespace["message"]
    loaded("one")
    assert (data.parents[1] / "reload-probe.wasm").read_bytes() == before_artifact
    assert not list(data.parents[2].glob(".kurbis-ai-load-*.wasm")), "Temporary load links leaked"
    # Protected bridge and missing plugins must fail without touching the server.
    op.command("ai reload kurbis-ai")
    eventually(lambda: op.seen("kurbis-ai is reserved"))
    submit("ai reload missing-plugin", "failed")
    existing = set((data / "results").glob("*.json"))
    guest.command("ai reload reload-probe")
    time.sleep(0.2)
    assert set((data / "results").glob("*.json")) == existing
    loaded("one")
