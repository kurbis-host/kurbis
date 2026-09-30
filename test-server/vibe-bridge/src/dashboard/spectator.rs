//! Stock Pumpkin skips inventory events for spectators and exposes clicks as
//! raw packet events. Consume workshop clicks without changing their gamemode.
use super::*;
use pumpkin_plugin_api::{common::GameMode, events::PacketReceivedEvent};

include!(concat!(env!("OUT_DIR"), "/packet_ids.rs"));

fn varint(bytes: &mut &[u8]) -> Option<i32> {
    let mut value = 0_u32;
    for index in 0..5 {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        if index == 4 && byte > 7 {
            return None;
        }
        value |= u32::from(byte & 127) << (index * 7);
        if byte & 128 == 0 {
            return i32::try_from(value).ok();
        }
    }
    None
}
fn click_header(mut bytes: &[u8]) -> Option<(i32, i16, i32)> {
    let window = varint(&mut bytes)?;
    let _revision = varint(&mut bytes)?;
    let slot = i16::from_be_bytes(bytes.get(..2)?.try_into().ok()?);
    let _button = *bytes.get(2)?;
    bytes = &bytes[3..];
    let mode = varint(&mut bytes)?;
    if mode > 6 || bytes.len() < 2 {
        return None;
    }
    Some((window, slot, mode))
}

pub struct Received;
impl EventHandler<PacketReceivedEvent> for Received {
    fn handle(
        &self,
        _server: Server,
        mut event: EventData<PacketReceivedEvent>,
    ) -> EventData<PacketReceivedEvent> {
        if event.cancelled || event.packet_id != CONTAINER_CLICK {
            return event;
        }
        if event.player.as_java().is_none() {
            return event;
        }
        let Some((window_id, slot, mode)) = click_header(&event.raw_payload) else {
            return event;
        };
        let id = key(&event.player);
        let action = {
            let mut state = state().lock().unwrap();
            if !state.menus.contains_key(&id) || !(1..=100).contains(&window_id) {
                return event;
            }
            // Bind the first click. Each plugin-owned open then increments the
            // same 1..=100 counter as this pinned release's screen handler.
            let window = state.windows.entry(id.clone()).or_default();
            if let Some(expected) = *window {
                if expected != window_id {
                    return event;
                }
            } else {
                *window = Some(window_id);
            }
            state
                .menus
                .get(&id)
                .and_then(|actions| actions.get(&slot))
                .cloned()
        };
        if event.player.get_gamemode() != GameMode::Spectator {
            return event;
        }
        event.cancelled = true;
        if !event.player.has_permission(PERMISSION) || mode == 5 {
            return event;
        }
        if let Some(action) = action
            && let Err(error) = act(&event.player, action)
        {
            tell(&event.player, &format!("Vibe: {error}"));
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_header_handles_wire_bounds_and_rejects_incomplete_packets() {
        let packet = [3, 0, 0, 33, 0, 0, 0, 0];
        assert_eq!(click_header(&packet), Some((3, 33, 0)));
        for length in 0..packet.len() {
            assert_eq!(click_header(&packet[..length]), None);
        }
        let mut drag = packet;
        drag[5] = 5;
        assert_eq!(click_header(&drag), Some((3, 33, 5)));
        drag[5] = 7;
        assert_eq!(click_header(&drag), None);
        let mut bad = &[255, 255, 255, 255, 127][..];
        assert_eq!(varint(&mut bad), None);
        let mut largest = &[255, 255, 255, 255, 7][..];
        assert_eq!(varint(&mut largest), Some(i32::MAX));
    }
}
