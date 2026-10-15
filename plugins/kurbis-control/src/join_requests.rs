//! Players the whitelist turns away, and `/letin`, which lets one of them in.
//!
//! Every player turned away is reported to the panel, which lists them so the
//! owner can let them in even when nobody who could is online.
//! When the whitelist stops someone joining, every operator online is told in
//! chat, and so is every player on the whitelist while the owner lets members
//! add players (the `whitelist_by_members` setting). The message links to
//! `/letin <name>`, which asks for confirmation before
//! `/letin <name> confirm` adds them. The Kurbis API checks the adder again
//! and records who added them in the whitelist and the server's activity.
//!
//! Pumpkin checks the whitelist for Java players only, after the pre-login
//! event; Bedrock players are stopped here, in the login event.

use std::{
    sync::{Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use pumpkin_plugin_api::{
    Context, Player, Server,
    command::{
        Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs,
        StringType,
    },
    commands::CommandHandler,
    common::NamedColor,
    events::{AsyncPlayerPreLoginEvent, EventData, EventHandler, PlayerLoginEvent},
    logging::{LogLevel, log},
    scheduler::SchedulerExt,
    text::TextComponent,
    uuid::Uuid as WitUuid,
};
use serde_json::Value;
use uuid::Uuid;

use crate::api;

pub(crate) const PERMISSION: &str = "kurbis-control:let-in";
/// How long `/letin` can still let in someone who was turned away.
const REMEMBER_FOR: Duration = Duration::from_secs(15 * 60);
/// A player retrying to join is announced again at most this often.
const ANNOUNCE_AGAIN_AFTER: Duration = Duration::from_secs(60);
const MAX_REMEMBERED: usize = 32;
const ANNOUNCE_TICKS: u64 = 20;

static STATE: OnceLock<Mutex<Vec<TurnedAway>>> = OnceLock::new();

#[derive(Clone, Debug)]
struct TurnedAway {
    id: Uuid,
    name: String,
    at: Instant,
    announced_at: Option<Instant>,
}

fn state() -> MutexGuard<'static, Vec<TurnedAway>> {
    STATE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn start(context: &Context) {
    context.schedule_repeating_task(ANNOUNCE_TICKS, ANNOUNCE_TICKS, announce_task);
}

pub(crate) fn command() -> Command {
    Command::new(
        &["letin".into()],
        "Let in a player the whitelist just turned away.",
    )
    .then(
        CommandNode::argument("name", &ArgumentType::String(StringType::SingleWord))
            .execute(Handler { confirmed: false })
            .then(CommandNode::literal("confirm").execute(Handler { confirmed: true })),
    )
}

pub(crate) struct PreLogin;
impl EventHandler<AsyncPlayerPreLoginEvent> for PreLogin {
    fn handle(
        &self,
        server: Server,
        mut event: EventData<AsyncPlayerPreLoginEvent>,
    ) -> EventData<AsyncPlayerPreLoginEvent> {
        let Ok(id) = Uuid::parse_str(&event.player_uuid) else {
            return event;
        };
        if !event.cancelled && is_turned_away(&server, id) {
            event.cancelled = true;
            event.kick_message = kick_message();
            remember(&mut state(), id, &event.player_name, Instant::now());
        }
        event
    }
}

pub(crate) struct Login;
impl EventHandler<PlayerLoginEvent> for Login {
    fn handle(
        &self,
        server: Server,
        mut event: EventData<PlayerLoginEvent>,
    ) -> EventData<PlayerLoginEvent> {
        let id = from_wit(event.player.get_id());
        if !event.cancelled && is_turned_away(&server, id) {
            event.cancelled = true;
            event.kick_message = kick_message();
            remember(&mut state(), id, &event.player.get_name(), Instant::now());
        }
        event
    }
}

/// Mirrors Pumpkin's own check: operators join even when not on the list.
fn is_turned_away(server: &Server, id: Uuid) -> bool {
    let whitelist = server.get_whitelist_manager();
    whitelist.is_enabled()
        && !whitelist.is_whitelisted(to_wit(id))
        && !server.get_op_manager().is_op(to_wit(id))
}

fn kick_message() -> TextComponent {
    TextComponent::text(
        "You are not on this server's whitelist yet.\n\
         The owner and anyone online who can let you in have been told you want to join.",
    )
}

fn remember(turned_away: &mut Vec<TurnedAway>, id: Uuid, name: &str, now: Instant) {
    turned_away.retain(|entry| now.duration_since(entry.at) < REMEMBER_FOR);
    if let Some(entry) = turned_away.iter_mut().find(|entry| entry.id == id) {
        entry.name = name.to_owned();
        entry.at = now;
        return;
    }
    if turned_away.len() >= MAX_REMEMBERED {
        turned_away.remove(0);
    }
    turned_away.push(TurnedAway {
        id,
        name: name.to_owned(),
        at: now,
        announced_at: None,
    });
}

/// The players to announce now, marked as announced.
fn due_announcements(turned_away: &mut [TurnedAway], now: Instant) -> Vec<TurnedAway> {
    turned_away
        .iter_mut()
        .filter(|entry| {
            entry
                .announced_at
                .is_none_or(|at| now.duration_since(at) >= ANNOUNCE_AGAIN_AFTER)
                && entry.announced_at.is_none_or(|at| entry.at > at)
        })
        .map(|entry| {
            entry.announced_at = Some(now);
            entry.clone()
        })
        .collect()
}

fn find(turned_away: &[TurnedAway], name: &str, now: Instant) -> Option<TurnedAway> {
    turned_away
        .iter()
        .rev()
        .find(|entry| {
            entry.name.eq_ignore_ascii_case(name) && now.duration_since(entry.at) < REMEMBER_FOR
        })
        .cloned()
}

fn forget(id: Uuid) {
    state().retain(|entry| entry.id != id);
}

fn announce_task(server: Server) {
    let due = due_announcements(&mut state(), Instant::now());
    if due.is_empty() {
        return;
    }
    for entry in &due {
        if let Err(error) = api::report_join_request(entry.id, &entry.name) {
            log(
                LogLevel::Warn,
                &format!("could not report {} to the panel: {error:#}", entry.name),
            );
        }
    }
    // Operators hear about it even when the setting cannot be read.
    let members_may_add = members_may_add().unwrap_or(false);
    for entry in due {
        for player in server.get_all_players() {
            if may_let_in(&server, &player, members_may_add) {
                player.send_system_message(announcement(&entry.name), false);
            }
        }
    }
}

fn announcement(name: &str) -> TextComponent {
    let command = format!("/letin {name}");
    TextComponent::text("§6[Kurbis] §f")
        .add_text(&format!(
            "{name} tried to join but is not on the whitelist. "
        ))
        .add_child(
            TextComponent::text("[Let them in]")
                .click_run_command(&command)
                .hover_show_text(TextComponent::text(&format!(
                    "Asks you to confirm before adding {name} to the whitelist"
                )))
                .underlined(true)
                .color_named(NamedColor::Green),
        )
        .add_text(&format!(" §7or type {command}"))
}

fn members_may_add() -> Result<bool> {
    let server = api::get(&api::server_path("")?)?;
    Ok(server
        .get("settings")
        .and_then(|settings| settings.get("whitelist_by_members"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn may_let_in(server: &Server, player: &Player, members_may_add: bool) -> bool {
    let id = player.get_id();
    server.get_op_manager().is_op(id)
        || members_may_add && server.get_whitelist_manager().is_whitelisted(id)
}

struct Handler {
    confirmed: bool,
}

impl CommandHandler for Handler {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let run = || -> Result<()> {
            let player = sender
                .as_player()
                .context("Only players can use /letin; operators can use /kurbis whitelist add.")?;
            crate::menu::forget_text_input(&player);
            let name = match args.get_value("name") {
                Arg::Simple(value) => value,
                _ => bail!("missing name"),
            };
            let whitelist = server.get_whitelist_manager();
            if !server.get_op_manager().is_op(player.get_id()) {
                if !whitelist.is_whitelisted(player.get_id()) {
                    bail!("Only players on the whitelist can let others in.");
                }
                if !members_may_add()? {
                    bail!("On this server only operators can let players in.");
                }
            }
            let Some(entry) = find(&state(), &name, Instant::now()) else {
                bail!("Nobody named {name} was turned away in the last 15 minutes.");
            };
            if whitelist.is_whitelisted(to_wit(entry.id)) {
                forget(entry.id);
                tell(
                    &player,
                    &format!("{} is already on the whitelist.", entry.name),
                );
                return Ok(());
            }
            if !self.confirmed {
                ask_to_confirm(&player, &entry.name);
                return Ok(());
            }
            api::add_whitelist_entry(
                &entry.name,
                Some(entry.id),
                Some((player_uuid(&player), &player.get_name())),
            )?;
            // The worker applies the same list shortly; adding it here lets
            // them join straight away.
            whitelist.add_player(&entry.name, to_wit(entry.id));
            forget(entry.id);
            let members_may_add = members_may_add().unwrap_or(false);
            let notice = format!(
                "§6[Kurbis] §f{} let {} in. They can join now.",
                player.get_name(),
                entry.name
            );
            for online in server.get_all_players() {
                if player_uuid(&online) == player_uuid(&player)
                    || may_let_in(&server, &online, members_may_add)
                {
                    tell(&online, &notice);
                }
            }
            Ok(())
        };
        run().map(|()| 1).map_err(|error| {
            CommandError::CommandFailed(TextComponent::text(&format!("Kurbis: {error:#}")))
        })
    }
}

fn ask_to_confirm(player: &Player, name: &str) {
    let command = format!("/letin {name} confirm");
    let message = TextComponent::text(&format!(
        "§6[Kurbis] §fAdd {name} to the whitelist? They can join right away and stay on it until an operator removes them. "
    ));
    let message = if player.as_bedrock().is_some() {
        message.add_text(&format!("§7Type {command} to confirm."))
    } else {
        message.add_child(
            TextComponent::text("[Confirm]")
                .click_run_command(&command)
                .hover_show_text(TextComponent::text(&format!("Add {name} to the whitelist")))
                .underlined(true)
                .color_named(NamedColor::Green),
        )
    };
    player.send_system_message(message, false);
}

fn tell(player: &Player, message: &str) {
    player.send_system_message(TextComponent::text(message), false);
}

pub(crate) fn player_uuid(player: &Player) -> Uuid {
    from_wit(player.get_id())
}

fn to_wit(id: Uuid) -> WitUuid {
    let (high, low) = id.as_u64_pair();
    WitUuid { high, low }
}

fn from_wit(id: WitUuid) -> Uuid {
    Uuid::from_u64_pair(id.high, id.low)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    #[test]
    fn wit_uuids_round_trip_in_their_standard_form() {
        let value = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        let wit = to_wit(value);
        assert_eq!(
            (wit.high, wit.low),
            (0x0011223344556677, 0x8899aabbccddeeff)
        );
        assert_eq!(from_wit(wit), value);
    }

    #[test]
    fn a_retry_is_announced_again_only_after_a_minute() {
        let start = Instant::now();
        let mut turned_away = Vec::new();
        remember(&mut turned_away, id(1), "Alex", start);
        assert_eq!(due_announcements(&mut turned_away, start).len(), 1);

        remember(
            &mut turned_away,
            id(1),
            "Alex",
            start + Duration::from_secs(10),
        );
        assert!(due_announcements(&mut turned_away, start + Duration::from_secs(10)).is_empty());
        assert_eq!(
            due_announcements(&mut turned_away, start + ANNOUNCE_AGAIN_AFTER).len(),
            1
        );
        // Without another attempt there is nothing new to announce.
        assert!(due_announcements(&mut turned_away, start + ANNOUNCE_AGAIN_AFTER * 3).is_empty());
    }

    #[test]
    fn letin_finds_recent_attempts_by_name_regardless_of_case() {
        let start = Instant::now();
        let mut turned_away = Vec::new();
        remember(&mut turned_away, id(1), "Alex", start);
        assert_eq!(
            find(&turned_away, "alex", start).map(|entry| entry.id),
            Some(id(1))
        );
        assert!(find(&turned_away, "Steve", start).is_none());
        assert!(find(&turned_away, "Alex", start + REMEMBER_FOR).is_none());
    }

    #[test]
    fn remembers_a_bounded_number_of_players() {
        let start = Instant::now();
        let mut turned_away = Vec::new();
        for value in 0..(MAX_REMEMBERED as u128 + 5) {
            remember(
                &mut turned_away,
                id(value),
                &format!("player{value}"),
                start,
            );
        }
        assert_eq!(turned_away.len(), MAX_REMEMBERED);
        assert!(find(&turned_away, "player0", start).is_none());
        assert!(find(&turned_away, "player36", start).is_some());
    }
}
