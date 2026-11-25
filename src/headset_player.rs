//! The media player the Bluetooth headsets see. It tells them what plays here
//! and carries out their buttons. Without a registered player BlueZ answers
//! "Stopped", so AirPods send Play for every squeeze and pausing depends on
//! what the desktop binds to that key.

use std::time::{Duration, Instant};

use dbus::Message;
use dbus::arg::{PropMap, Variant};
use dbus::blocking::Connection;
use dbus::blocking::stdintf::org_freedesktop_dbus::{Properties, PropertiesPropertiesChanged};
use dbus::channel::{MatchingReceiver, Sender};
use dbus::message::{MatchRule, SignalArgs};
use log::{info, warn};

use crate::media_controller::MediaController;

/// Our player's object on the system bus.
const PATH: &str = "/org/podbridge/player";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
/// How often the players here are read when no button arrives.
const POLL: Duration = Duration::from_millis(500);
/// BlueZ forgets the player when it restarts or the adapter is replugged.
const REGISTER_EVERY: Duration = Duration::from_secs(10);

/// The state reported to headsets, as MPRIS spells it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Playback {
    Playing,
    Paused,
    Stopped,
}

/// A media button on a headset.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Button {
    Play,
    Pause,
    Next,
    Previous,
}

impl Playback {
    fn from_mpris(status: &str) -> Self {
        match status {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }

    fn as_mpris(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }
}

/// One MPRIS player on this computer.
struct Player {
    service: String,
    status: Playback,
}

/// What headsets hear: playing if anything plays, else paused if anything is
/// paused.
fn playback(players: &[Player]) -> Playback {
    let any = |status| players.iter().any(|player| player.status == status);
    if any(Playback::Playing) {
        Playback::Playing
    } else if any(Playback::Paused) {
        Playback::Paused
    } else {
        Playback::Stopped
    }
}

/// playerctld's name for the most recently active player.
const LATEST_PLAYER: &str = "org.mpris.MediaPlayer2.playerctld";

/// The players that play. playerctld only mirrors one of them, and can lag.
fn playing(players: &[Player]) -> impl Iterator<Item = &Player> {
    players
        .iter()
        .filter(|p| p.status == Playback::Playing && p.service != LATEST_PLAYER)
}

/// What the buttons remember between presses.
#[derive(Default)]
struct Buttons {
    /// The players the last Pause stopped.
    paused: Vec<String>,
    /// The player last heard playing, for desktops without playerctld.
    last_heard: Option<String>,
}

impl Buttons {
    /// Forget the paused players once something else plays: Play then belongs
    /// to what the user chose since.
    fn observe(&mut self, players: &[Player]) {
        if playing(players).any(|p| !self.paused.contains(&p.service)) {
            self.paused.clear();
        }
        if let Some(player) = playing(players).next() {
            self.last_heard = Some(player.service.clone());
        }
    }

    /// The MPRIS calls a button stands for, as (service, method).
    fn press(&mut self, button: Button, players: &[Player]) -> Vec<(String, &'static str)> {
        let named = |service: &str| players.iter().find(|p| p.service == service);
        // Where a button goes when nothing plays.
        let latest = named(LATEST_PLAYER).or(self.last_heard.as_deref().and_then(named));
        let one = |player: Option<&Player>, method| {
            player
                .map(|player| (player.service.clone(), method))
                .into_iter()
                .collect()
        };
        match button {
            Button::Pause => {
                let stopping: Vec<String> = playing(players).map(|p| p.service.clone()).collect();
                if !stopping.is_empty() {
                    self.paused = stopping.clone();
                }
                stopping.into_iter().map(|s| (s, "Pause")).collect()
            }
            Button::Play if playing(players).next().is_some() => Vec::new(),
            Button::Play => {
                let resume: Vec<_> = std::mem::take(&mut self.paused)
                    .into_iter()
                    .filter(|service| players.iter().any(|p| &p.service == service))
                    .map(|service| (service, "Play"))
                    .collect();
                if !resume.is_empty() {
                    return resume;
                }
                let paused = players.iter().find(|p| p.status == Playback::Paused);
                one(latest.or(paused).or(players.first()), "Play")
            }
            Button::Next | Button::Previous => {
                let method = if button == Button::Next {
                    "Next"
                } else {
                    "Previous"
                };
                let target = playing(players).next().or(latest).or(players.first());
                one(target, method)
            }
        }
    }
}

/// The button BlueZ forwards with this call to our player.
fn button(message: &Message) -> Option<Button> {
    if message.path().as_deref() != Some(PATH)
        || message.interface().as_deref() != Some(PLAYER_INTERFACE)
    {
        return None;
    }
    match message.member().as_deref()? {
        "Play" => Some(Button::Play),
        "Pause" | "Stop" => Some(Button::Pause),
        "Next" => Some(Button::Next),
        "Previous" => Some(Button::Previous),
        _ => None,
    }
}

/// Show the headsets on `adapter` a media player for as long as we run.
pub fn spawn(adapter: &str) {
    let adapter = adapter.to_string();
    std::thread::spawn(move || {
        if let Err(e) = run(&adapter) {
            warn!("Headset media buttons stay with the desktop: {e}");
        }
    });
}

fn run(adapter: &str) -> Result<(), dbus::Error> {
    let system = Connection::new_system()?;
    let session = Connection::new_session()?;

    let (pressed_tx, pressed) = std::sync::mpsc::channel();
    system.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |message, connection| {
            let reply = match button(&message) {
                Some(button) => {
                    // The receiver lives as long as this connection.
                    let _ = pressed_tx.send(button);
                    message.method_return()
                }
                None => message.error(
                    &"org.freedesktop.DBus.Error.UnknownMethod".into(),
                    c"podbridge does not offer this",
                ),
            };
            // A refused reply only matters to a caller that waits for one.
            if !message.get_no_reply() {
                let _ = connection.send(reply);
            }
            true
        }),
    );

    let mut buttons = Buttons::default();
    let mut reported = None;
    let mut tried: Option<Instant> = None;
    let mut refused = false;
    loop {
        let players = read_players(&session);
        buttons.observe(&players);
        let state = playback(&players);

        if tried.is_none_or(|at| at.elapsed() >= REGISTER_EVERY) {
            tried = Some(Instant::now());
            match register(&system, adapter, state) {
                Ok(()) => {
                    info!("Headsets on {adapter} now see podbridge's media player");
                    refused = false;
                    reported = Some(state);
                }
                Err(e) if e.name() == Some("org.bluez.Error.AlreadyExists") => {}
                Err(e) => {
                    if !refused {
                        warn!("BlueZ did not take the media player: {e}");
                    }
                    refused = true;
                }
            }
        }
        if reported != Some(state) {
            announce(&system, state);
            reported = Some(state);
        }

        system.process(POLL)?;
        for button in pressed.try_iter() {
            // Read again: the state may have moved since the last poll.
            let players = read_players(&session);
            let calls = buttons.press(button, &players);
            info!("Headset pressed {button:?}: {calls:?}");
            for (service, method) in calls {
                let player = session.with_proxy(&service, "/org/mpris/MediaPlayer2", POLL);
                if let Err(e) =
                    player.method_call::<(), _, &str, &str>(PLAYER_INTERFACE, method, ())
                {
                    warn!("{method} failed for {service}: {e}");
                }
            }
        }
    }
}

fn read_players(session: &Connection) -> Vec<Player> {
    MediaController::list_mpris_services(session)
        .into_iter()
        .filter_map(|service| {
            let status: String = session
                .with_proxy(&service, "/org/mpris/MediaPlayer2", POLL)
                .get(PLAYER_INTERFACE, "PlaybackStatus")
                .ok()?;
            Some(Player {
                service,
                status: Playback::from_mpris(&status),
            })
        })
        .collect()
}

/// BlueZ takes the properties from this call and then follows our signals.
fn register(system: &Connection, adapter: &str, state: Playback) -> Result<(), dbus::Error> {
    let mut properties = status_property(state);
    // BlueZ drops the buttons of a player that does not claim them.
    for flag in [
        "CanControl",
        "CanPlay",
        "CanPause",
        "CanGoNext",
        "CanGoPrevious",
    ] {
        properties.insert(flag.to_string(), Variant(Box::new(true)));
    }
    properties.insert(
        "Identity".to_string(),
        Variant(Box::new("podbridge".to_string())),
    );
    system
        .with_proxy(
            "org.bluez",
            format!("/org/bluez/{adapter}"),
            Duration::from_secs(5),
        )
        .method_call(
            "org.bluez.Media1",
            "RegisterPlayer",
            (dbus::Path::from(PATH), properties),
        )
}

fn status_property(state: Playback) -> PropMap {
    let mut properties = PropMap::new();
    properties.insert(
        "PlaybackStatus".to_string(),
        Variant(Box::new(state.as_mpris().to_string())),
    );
    properties
}

/// Tell BlueZ, and through it the headsets, that the state changed.
fn announce(system: &Connection, state: Playback) {
    let changed = PropertiesPropertiesChanged {
        interface_name: PLAYER_INTERFACE.to_string(),
        changed_properties: status_property(state),
        invalidated_properties: Vec::new(),
    };
    if system.send(changed.to_emit_message(&PATH.into())).is_err() {
        warn!("Could not report the playback state to BlueZ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MUSIC: &str = "org.mpris.MediaPlayer2.music";
    const VIDEO: &str = "org.mpris.MediaPlayer2.video";
    const LATEST: &str = "org.mpris.MediaPlayer2.playerctld";

    fn players(list: &[(&str, Playback)]) -> Vec<Player> {
        list.iter()
            .map(|&(service, status)| Player {
                service: service.to_string(),
                status,
            })
            .collect()
    }

    fn calls(list: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
        list.iter()
            .map(|&(service, method)| (service.to_string(), method))
            .collect()
    }

    #[test]
    fn headsets_hear_playing_if_anything_plays() {
        use Playback::*;
        assert_eq!(
            playback(&players(&[(MUSIC, Paused), (VIDEO, Playing)])),
            Playing
        );
        assert_eq!(
            playback(&players(&[(MUSIC, Stopped), (VIDEO, Paused)])),
            Paused
        );
        assert_eq!(playback(&players(&[(MUSIC, Stopped)])), Stopped);
        assert_eq!(playback(&[]), Stopped);
    }

    #[test]
    fn pause_stops_everything_that_plays() {
        use Playback::*;
        // playerctld only mirrors another player: pausing that one is enough.
        let now = players(&[(MUSIC, Playing), (VIDEO, Playing), (LATEST, Playing)]);
        assert_eq!(
            Buttons::default().press(Button::Pause, &now),
            calls(&[(MUSIC, "Pause"), (VIDEO, "Pause")])
        );
    }

    #[test]
    fn a_mirror_slow_to_report_the_pause_keeps_the_memory() {
        use Playback::*;
        let mut buttons = Buttons::default();
        buttons.press(
            Button::Pause,
            &players(&[(VIDEO, Playing), (LATEST, Playing)]),
        );
        buttons.observe(&players(&[(VIDEO, Paused), (LATEST, Playing)]));
        let quiet = players(&[(MUSIC, Paused), (VIDEO, Paused), (LATEST, Paused)]);
        assert_eq!(
            buttons.press(Button::Play, &quiet),
            calls(&[(VIDEO, "Play")])
        );
    }

    #[test]
    fn without_playerctld_buttons_go_to_the_player_heard_last() {
        use Playback::*;
        let mut buttons = Buttons::default();
        // Paused elsewhere: from the desktop, or by a bud coming out.
        buttons.observe(&players(&[(VIDEO, Paused), (MUSIC, Playing)]));
        let quiet = players(&[(VIDEO, Paused), (MUSIC, Paused)]);
        buttons.observe(&quiet);
        assert_eq!(
            buttons.press(Button::Play, &quiet),
            calls(&[(MUSIC, "Play")])
        );
        assert_eq!(
            buttons.press(Button::Next, &quiet),
            calls(&[(MUSIC, "Next")])
        );
        // Gone since: fall back to what is left.
        let left = players(&[(VIDEO, Paused)]);
        assert_eq!(
            buttons.press(Button::Play, &left),
            calls(&[(VIDEO, "Play")])
        );
    }

    #[test]
    fn play_resumes_what_pause_stopped() {
        use Playback::*;
        let mut buttons = Buttons::default();
        buttons.press(
            Button::Pause,
            &players(&[(MUSIC, Paused), (VIDEO, Playing)]),
        );
        let after = players(&[(MUSIC, Paused), (VIDEO, Paused), (LATEST, Paused)]);
        assert_eq!(
            buttons.press(Button::Play, &after),
            calls(&[(VIDEO, "Play")])
        );
    }

    #[test]
    fn a_second_pause_keeps_the_memory_of_the_first() {
        use Playback::*;
        let mut buttons = Buttons::default();
        buttons.press(Button::Pause, &players(&[(VIDEO, Playing)]));
        let quiet = players(&[(MUSIC, Paused), (VIDEO, Paused)]);
        assert_eq!(buttons.press(Button::Pause, &quiet), calls(&[]));
        assert_eq!(
            buttons.press(Button::Play, &quiet),
            calls(&[(VIDEO, "Play")])
        );
    }

    #[test]
    fn play_never_pauses() {
        use Playback::*;
        let now = players(&[(MUSIC, Playing), (VIDEO, Paused)]);
        assert_eq!(Buttons::default().press(Button::Play, &now), calls(&[]));
    }

    #[test]
    fn play_without_a_remembered_pause_starts_one_player() {
        use Playback::*;
        let mut buttons = Buttons::default();
        // The most recently active player, when playerctld names it.
        let with_latest = players(&[(MUSIC, Paused), (LATEST, Paused)]);
        assert_eq!(
            buttons.press(Button::Play, &with_latest),
            calls(&[(LATEST, "Play")])
        );
        // Else a paused one before a stopped one.
        let without = players(&[(MUSIC, Stopped), (VIDEO, Paused)]);
        assert_eq!(
            buttons.press(Button::Play, &without),
            calls(&[(VIDEO, "Play")])
        );
        let stopped = players(&[(MUSIC, Stopped)]);
        assert_eq!(
            buttons.press(Button::Play, &stopped),
            calls(&[(MUSIC, "Play")])
        );
        assert_eq!(buttons.press(Button::Play, &[]), calls(&[]));
    }

    #[test]
    fn a_remembered_player_that_quit_is_skipped() {
        use Playback::*;
        let mut buttons = Buttons::default();
        buttons.press(Button::Pause, &players(&[(VIDEO, Playing)]));
        assert_eq!(
            buttons.press(Button::Play, &players(&[(MUSIC, Paused)])),
            calls(&[(MUSIC, "Play")])
        );
    }

    #[test]
    fn playing_something_else_forgets_the_pause() {
        use Playback::*;
        let mut buttons = Buttons::default();
        buttons.press(Button::Pause, &players(&[(VIDEO, Playing)]));
        buttons.observe(&players(&[(MUSIC, Playing), (VIDEO, Paused)]));
        let quiet = players(&[(MUSIC, Paused), (VIDEO, Paused)]);
        assert_eq!(
            buttons.press(Button::Play, &quiet),
            calls(&[(MUSIC, "Play")])
        );
    }

    #[test]
    fn a_player_slow_to_report_its_pause_stays_remembered() {
        use Playback::*;
        let mut buttons = Buttons::default();
        buttons.press(Button::Pause, &players(&[(VIDEO, Playing)]));
        buttons.observe(&players(&[(MUSIC, Paused), (VIDEO, Playing)]));
        let quiet = players(&[(MUSIC, Paused), (VIDEO, Paused)]);
        assert_eq!(
            buttons.press(Button::Play, &quiet),
            calls(&[(VIDEO, "Play")])
        );
    }

    #[test]
    fn bluez_calls_on_our_player_are_buttons() {
        let call = |path: &str, interface: &str, member: &str| {
            let message =
                Message::new_method_call("org.podbridge", path, interface, member).unwrap();
            button(&message)
        };
        let ours = |member| call(PATH, PLAYER_INTERFACE, member);
        assert_eq!(ours("Play"), Some(Button::Play));
        assert_eq!(ours("Pause"), Some(Button::Pause));
        assert_eq!(ours("Stop"), Some(Button::Pause));
        assert_eq!(ours("Next"), Some(Button::Next));
        assert_eq!(ours("Previous"), Some(Button::Previous));
        assert_eq!(ours("Seek"), None);
        assert_eq!(call("/other", PLAYER_INTERFACE, "Play"), None);
        assert_eq!(call(PATH, "org.freedesktop.DBus.Peer", "Play"), None);
    }

    #[test]
    fn skipping_goes_to_one_player() {
        use Playback::*;
        let mut buttons = Buttons::default();
        let playing = players(&[(MUSIC, Paused), (VIDEO, Playing), (LATEST, Playing)]);
        assert_eq!(
            buttons.press(Button::Next, &playing),
            calls(&[(VIDEO, "Next")])
        );
        let quiet = players(&[(MUSIC, Paused), (LATEST, Paused)]);
        assert_eq!(
            buttons.press(Button::Previous, &quiet),
            calls(&[(LATEST, "Previous")])
        );
        assert_eq!(
            buttons.press(Button::Previous, &players(&[(MUSIC, Paused)])),
            calls(&[(MUSIC, "Previous")])
        );
    }
}
