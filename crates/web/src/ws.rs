//! Bulletproof WebSocket connection manager.
//!
//! A single long-lived connection per session. It survives anything the network
//! throws at it: drops, server restarts, sleeping tabs, flaky mobile signal. The
//! rules are simple and robust:
//!   * every (re)connect re-sends Join/Resume; the server replies with a full
//!     snapshot, so we always resync — no fragile incremental state,
//!   * reconnect with capped exponential backoff + jitter, forever,
//!   * a watchdog reconnects if frames stop arriving (dead-but-open sockets),
//!   * reconnect immediately when the browser comes back online / the tab is
//!     refocused,
//!   * give up only when the server says the session is fatally gone.

use std::cell::RefCell;
use std::rc::Rc;

use herdcore_protocol::v1::{client_frame, server_frame};
use herdcore_protocol::{decode_frame, encode_frame, v1};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use crate::api::{self, Session};
use crate::state::{AppAction, AppHandle};

const MIN_BACKOFF_MS: u32 = 500;
const MAX_BACKOFF_MS: u32 = 8_000;
/// If no frame (not even the server's ~20s heartbeat) arrives in this window,
/// the socket is treated as dead and force-reconnected.
const WATCHDOG_STALE_MS: f64 = 32_000.0;

/// What the connection should announce when a socket opens.
#[derive(Clone)]
enum Intent {
    Join {
        lobby_name: String,
        display_name: String,
    },
    Resume(Session),
}

struct Inner {
    url: String,
    app: AppHandle,
    socket: Option<WebSocket>,
    intent: Option<Intent>,
    session: Option<Session>,
    /// True only after this socket's Join/Resume has received Welcome.
    established: bool,
    /// Commands issued while reconnecting. They are flushed after Welcome, so
    /// the server never receives them before the session is authenticated.
    pending: Vec<v1::ClientFrame>,
    stopped: bool,
    backoff_ms: u32,
    generation: u64,
    reconnect_scheduled_for: Option<u64>,
    last_frame_ms: f64,
    // Closures must outlive the socket that references them.
    keepalive: Vec<Closure<dyn FnMut(JsEvent)>>,
}

// Helper alias so the closure vec can hold differently-typed event handlers.
type JsEvent = wasm_bindgen::JsValue;

#[derive(Clone)]
pub struct Connection {
    inner: Rc<RefCell<Inner>>,
}

impl PartialEq for Connection {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Connection {
    pub fn new(app: AppHandle) -> Connection {
        let connection = Connection {
            inner: Rc::new(RefCell::new(Inner {
                url: api::ws_url(),
                app,
                socket: None,
                intent: None,
                session: None,
                established: false,
                pending: Vec::new(),
                stopped: true,
                backoff_ms: MIN_BACKOFF_MS,
                generation: 0,
                reconnect_scheduled_for: None,
                last_frame_ms: now(),
                keepalive: Vec::new(),
            })),
        };
        connection.install_global_listeners();
        connection.start_watchdog();
        connection
    }

    /// Join or create a lobby by word as a fresh participant.
    pub fn join(&self, lobby_name: String, display_name: String) {
        let mut inner = self.inner.borrow_mut();
        inner.intent = Some(Intent::Join {
            lobby_name,
            display_name,
        });
        inner.session = None;
        inner.established = false;
        inner.pending.clear();
        drop(inner);
        self.start();
    }

    /// Re-attach to an existing session (page load with a stored session).
    pub fn resume(&self, session: Session) {
        let mut inner = self.inner.borrow_mut();
        inner.session = Some(session.clone());
        inner.intent = Some(Intent::Resume(session));
        inner.established = false;
        drop(inner);
        self.start();
    }

    /// Send a command on an authenticated socket, or retain it until reconnect
    /// completes. This prevents UI actions from disappearing during a brief
    /// disconnect while the last authoritative lobby snapshot is still shown.
    pub fn send(&self, frame: v1::ClientFrame) {
        let mut inner = self.inner.borrow_mut();
        let sent = inner.established
            && inner.socket.as_ref().is_some_and(|socket| {
                socket.ready_state() == WebSocket::OPEN
                    && socket
                        .send_with_u8_array(&encode_frame(&frame))
                        .is_ok()
            });
        if !sent {
            enqueue(&mut inner.pending, frame);
        }
        let app = (!sent && !inner.stopped).then(|| inner.app.clone());
        drop(inner);
        if let Some(app) = app {
            app.dispatch(AppAction::Status(
                "Reconnecting — your action will be sent automatically".into(),
            ));
        }
    }

    /// Leave the lobby and stop reconnecting.
    pub fn leave(&self) {
        self.send(client(client_frame::Body::Leave(v1::LeaveCommand {})));
        self.shutdown();
        api::clear_session();
        self.inner.borrow().app.dispatch(AppAction::Cleared);
    }

    fn start(&self) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.stopped = false;
            inner.backoff_ms = MIN_BACKOFF_MS;
        }
        self.connect();
    }

    fn shutdown(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.stopped = true;
        inner.intent = None;
        inner.session = None;
        inner.established = false;
        inner.pending.clear();
        inner.reconnect_scheduled_for = None;
        if let Some(socket) = inner.socket.take() {
            clear_handlers(&socket);
            let _ = socket.close();
        }
    }

    fn connect(&self) {
        let (url, generation) = {
            let mut inner = self.inner.borrow_mut();
            if inner.stopped {
                return;
            }
            if let Some(socket) = inner.socket.take() {
                clear_handlers(&socket);
                let _ = socket.close();
            }
            inner.generation += 1;
            inner.established = false;
            inner.reconnect_scheduled_for = None;
            inner.last_frame_ms = now();
            (inner.url.clone(), inner.generation)
        };

        let socket = match WebSocket::new(&url) {
            Ok(socket) => socket,
            Err(_) => {
                self.schedule_reconnect();
                return;
            }
        };
        socket.set_binary_type(BinaryType::Arraybuffer);

        let on_open = {
            let this = self.clone();
            Closure::<dyn FnMut(JsEvent)>::new(move |_| this.on_open(generation))
        };
        let on_message = {
            let this = self.clone();
            Closure::<dyn FnMut(JsEvent)>::new(move |event: JsEvent| {
                if let Ok(event) = event.dyn_into::<MessageEvent>() {
                    this.on_message(event);
                }
            })
        };
        let on_close = {
            let this = self.clone();
            Closure::<dyn FnMut(JsEvent)>::new(move |event: JsEvent| {
                let _ = event.dyn_into::<CloseEvent>();
                this.on_disconnect(generation);
            })
        };
        let on_error = {
            let this = self.clone();
            Closure::<dyn FnMut(JsEvent)>::new(move |event: JsEvent| {
                let _ = event.dyn_into::<Event>();
                this.on_disconnect(generation);
            })
        };
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let mut inner = self.inner.borrow_mut();
        inner.socket = Some(socket);
        inner.keepalive = vec![on_open, on_message, on_close, on_error];
    }

    fn on_open(&self, generation: u64) {
        let frame = {
            let mut inner = self.inner.borrow_mut();
            if inner.stopped || inner.generation != generation {
                return;
            }
            inner.backoff_ms = MIN_BACKOFF_MS;
            inner.last_frame_ms = now();
            match &inner.intent {
                Some(Intent::Join {
                    lobby_name,
                    display_name,
                }) => client(client_frame::Body::Join(v1::Join {
                    lobby_name: lobby_name.clone(),
                    display_name: display_name.clone(),
                })),
                Some(Intent::Resume(session)) => client(client_frame::Body::Resume(v1::Resume {
                    lobby_id: session.lobby_id.clone(),
                    player_id: session.player_id.clone(),
                    session_token: session.token.clone(),
                    after_version: 0,
                })),
                None => return,
            }
        };
        // Opening frames must bypass the authenticated-command outbox: Welcome
        // is what marks this socket established.
        let failed = {
            let inner = self.inner.borrow();
            inner.generation != generation
                || inner.socket.as_ref().is_none_or(|socket| {
                    socket.ready_state() != WebSocket::OPEN
                        || socket
                            .send_with_u8_array(&encode_frame(&frame))
                            .is_err()
                })
        };
        if failed {
            self.on_disconnect(generation);
        }
    }

    fn on_message(&self, event: MessageEvent) {
        let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
        self.inner.borrow_mut().last_frame_ms = now();
        let Ok(frame) = decode_frame::<v1::ServerFrame>(&bytes) else {
            return;
        };
        let app = self.inner.borrow().app.clone();
        match frame.body {
            Some(server_frame::Body::Welcome(welcome)) => {
                if let Some(lobby) = welcome.lobby {
                    let session = Session {
                        lobby_id: lobby.lobby_id.clone(),
                        player_id: welcome.player_id,
                        token: welcome.session_token,
                        word: lobby.lobby_code.clone(),
                    };
                    api::save_session(&session);
                    {
                        let mut inner = self.inner.borrow_mut();
                        inner.session = Some(session.clone());
                        inner.intent = Some(Intent::Resume(session.clone()));
                        inner.established = true;
                    }
                    app.dispatch(AppAction::Joined { session, lobby });
                    app.dispatch(AppAction::SetCatalogue(welcome.catalogue));
                    self.flush_pending();
                    // Refresh the games list on (re)connect.
                    self.send(client(client_frame::Body::ListGames(v1::ListGamesCommand {})));
                }
            }
            Some(server_frame::Body::Update(update)) => {
                if let Some(lobby) = update.lobby {
                    let reset = update.kind == v1::LobbyEventKind::GameStarted as i32
                        || update.kind == v1::LobbyEventKind::TurnResolved as i32;
                    app.dispatch(AppAction::ApplyEvent {
                        lobby,
                        reset_submitted: reset,
                    });
                }
            }
            Some(server_frame::Body::Games(list)) => {
                app.dispatch(AppAction::SetGames(list.games));
            }
            Some(server_frame::Body::Moved(moved)) => {
                app.dispatch(AppAction::Moved {
                    game_id: moved.game_id,
                    turn: moved.turn,
                    seat: moved.seat,
                });
            }
            Some(server_frame::Body::Error(error)) => {
                if error.fatal {
                    self.shutdown();
                    api::clear_session();
                    app.dispatch(AppAction::Cleared);
                } else {
                    app.dispatch(AppAction::Status(error.message));
                }
            }
            Some(server_frame::Body::Bye(_)) => {
                self.shutdown();
                api::clear_session();
                app.dispatch(AppAction::Cleared);
            }
            // Assign is only for bot providers; a browser never receives it.
            Some(server_frame::Body::Pong(_))
            | Some(server_frame::Body::Assign(_))
            | None => {}
        }
    }

    fn on_disconnect(&self, generation: u64) {
        {
            let mut inner = self.inner.borrow_mut();
            if inner.stopped || inner.generation != generation {
                return;
            }
            inner.established = false;
        }
        self.schedule_reconnect();
    }

    fn schedule_reconnect(&self) {
        let delay = {
            let mut inner = self.inner.borrow_mut();
            if inner.stopped {
                return;
            }
            let generation = inner.generation;
            if inner.reconnect_scheduled_for == Some(generation) {
                return;
            }
            inner.reconnect_scheduled_for = Some(generation);
            let base = inner.backoff_ms;
            inner.backoff_ms = (base.saturating_mul(2)).min(MAX_BACKOFF_MS);
            // jitter: 50%..100% of base, so reconnects don't thunder together.
            let jitter = (js_sys::Math::random() * 0.5 + 0.5) * f64::from(base);
            (jitter as u32, generation)
        };
        let (delay, generation) = delay;
        let this = self.clone();
        gloo_timers::callback::Timeout::new(delay, move || {
            let should_reconnect = {
                let mut inner = this.inner.borrow_mut();
                let should = !inner.stopped
                    && inner.generation == generation
                    && inner.reconnect_scheduled_for == Some(generation);
                if should {
                    inner.reconnect_scheduled_for = None;
                }
                should
            };
            if should_reconnect {
                this.connect();
            }
        })
        .forget();
    }

    fn flush_pending(&self) {
        let pending = std::mem::take(&mut self.inner.borrow_mut().pending);
        for frame in pending {
            self.send(frame);
        }
    }

    fn start_watchdog(&self) {
        let this = self.clone();
        gloo_timers::callback::Interval::new(8_000, move || {
            let stale = {
                let inner = this.inner.borrow();
                !inner.stopped
                    && inner.socket.is_some()
                    && now() - inner.last_frame_ms > WATCHDOG_STALE_MS
            };
            if stale {
                // Force a reconnect: drop the dead socket and dial again.
                this.connect();
            }
        })
        .forget();
    }

    fn install_global_listeners(&self) {
        let Some(window) = web_sys::window() else {
            return;
        };
        // Reconnect promptly when connectivity or focus returns.
        let reconnect = {
            let this = self.clone();
            Closure::<dyn FnMut(JsEvent)>::new(move |_| {
                let should = {
                    let inner = this.inner.borrow();
                    !inner.stopped
                        && inner
                            .socket
                            .as_ref()
                            .map(|s| s.ready_state() != WebSocket::OPEN)
                            .unwrap_or(true)
                };
                if should {
                    this.connect();
                }
            })
        };
        let target: &web_sys::EventTarget = window.as_ref();
        let _ = target.add_event_listener_with_callback("online", reconnect.as_ref().unchecked_ref());
        let _ = target
            .add_event_listener_with_callback("visibilitychange", reconnect.as_ref().unchecked_ref());
        reconnect.forget();
    }
}

/// Coalesce snapshot requests and Start clicks while disconnected. Other
/// commands retain their order; move commands already carry request IDs.
fn enqueue(pending: &mut Vec<v1::ClientFrame>, frame: v1::ClientFrame) {
    let singleton = matches!(
        &frame.body,
        Some(client_frame::Body::Start(_)) | Some(client_frame::Body::ListGames(_))
    );
    if singleton
        && pending.iter().any(|queued| {
            matches!(
                (&queued.body, &frame.body),
                (
                    Some(client_frame::Body::Start(_)),
                    Some(client_frame::Body::Start(_))
                ) | (
                    Some(client_frame::Body::ListGames(_)),
                    Some(client_frame::Body::ListGames(_))
                )
            )
        })
    {
        return;
    }
    pending.push(frame);
}

fn client(body: client_frame::Body) -> v1::ClientFrame {
    v1::ClientFrame { body: Some(body) }
}

fn clear_handlers(socket: &WebSocket) {
    socket.set_onopen(None);
    socket.set_onmessage(None);
    socket.set_onclose(None);
    socket.set_onerror(None);
}

fn now() -> f64 {
    js_sys::Date::now()
}

#[cfg(test)]
mod tests {
    use super::{client, enqueue};
    use herdcore_protocol::v1::{self, client_frame};

    #[test]
    fn disconnected_start_and_list_requests_are_coalesced() {
        let mut pending = Vec::new();
        enqueue(
            &mut pending,
            client(client_frame::Body::Start(v1::StartCommand {})),
        );
        enqueue(
            &mut pending,
            client(client_frame::Body::Start(v1::StartCommand {})),
        );
        enqueue(
            &mut pending,
            client(client_frame::Body::ListGames(v1::ListGamesCommand {})),
        );
        enqueue(
            &mut pending,
            client(client_frame::Body::ListGames(v1::ListGamesCommand {})),
        );

        assert_eq!(pending.len(), 2);
        assert!(matches!(
            &pending[0].body,
            Some(client_frame::Body::Start(_))
        ));
        assert!(matches!(
            &pending[1].body,
            Some(client_frame::Body::ListGames(_))
        ));
    }
}
