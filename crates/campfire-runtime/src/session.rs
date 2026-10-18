use crate::{
    Config, ConfigError, GameContext, GameModule,
    metrics::{Metrics, MetricsSnapshot, inc},
    output::Output,
    protocol::{Command, Error, Request},
    rate::Bucket,
    room::{self, Admission, Control, Input, RoomHandle, RoomRef},
};
use axum::{
    Router,
    routing::{get, post},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, Semaphore, mpsc, oneshot, watch},
    time::Instant,
};

pub(crate) type TokenHash = [u8; 32];
pub(crate) struct Session {
    pub id: String,
    pub game_id: String,
    pub deadline: Instant,
    pub expires_at: u64,
    pub generation: u64,
    pub output: Option<Arc<Output>>,
    pub room: Option<RoomRef>,
    pub reservation: Option<Instant>,
}
pub(crate) struct RateKey {
    pub bucket: Bucket,
    pub last_seen: Instant,
}
pub(crate) struct State {
    pub sessions: HashMap<TokenHash, Session>,
    pub rooms: BTreeMap<(String, String), RoomHandle>,
    pub rate_keys: HashMap<String, RateKey>,
    pub global_rate: Bucket,
}
pub(crate) struct Inner {
    pub config: Config,
    pub modules: RwLock<HashMap<String, Arc<dyn GameModule>>>,
    pub state: Mutex<State>,
    pub metrics: Arc<Metrics>,
    pub connections: Arc<Semaphore>,
    pub unauthenticated: Arc<Semaphore>,
    pub shutdown: watch::Sender<bool>,
    started: AtomicBool,
    clock_origin: Instant,
    unix_origin: u64,
}
#[derive(Clone)]
pub struct Campfire {
    pub(crate) inner: Arc<Inner>,
}
impl Campfire {
    pub fn new(config: Config) -> Result<Self, ConfigError> {
        config.validate()?;
        let (shutdown, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(Inner {
                connections: Arc::new(Semaphore::new(config.max_connections)),
                unauthenticated: Arc::new(Semaphore::new(config.max_unauthenticated_connections)),
                state: Mutex::new(State {
                    sessions: HashMap::new(),
                    rooms: BTreeMap::new(),
                    rate_keys: HashMap::new(),
                    global_rate: Bucket::new(config.global_guest_burst),
                }),
                config,
                modules: RwLock::new(HashMap::new()),
                metrics: Arc::new(Metrics::default()),
                shutdown,
                started: AtomicBool::new(false),
                clock_origin: Instant::now(),
                unix_origin: unix_millis(),
            }),
        })
    }
    pub fn register_game(
        &self,
        game_id: impl Into<String>,
        module: impl GameModule,
    ) -> Result<(), ConfigError> {
        let game_id = game_id.into();
        if game_id.is_empty()
            || game_id.len() > 64
            || !game_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(ConfigError(
                "game_id must contain 1..=64 ASCII letters, digits, '-' or '_'".into(),
            ));
        }
        let mut modules = self
            .inner
            .modules
            .write()
            .map_err(|_| ConfigError("game registry lock is poisoned".into()))?;
        if modules.contains_key(&game_id) {
            return Err(ConfigError("game_id already registered".into()));
        }
        modules.insert(game_id, Arc::new(module));
        Ok(())
    }
    pub fn router(&self) -> Router {
        self.start();
        Router::new()
            .route("/health", get(crate::transport::health))
            .route("/v1/games/{game_id}/guests", post(crate::transport::guest))
            .route("/v1/games/{game_id}/ws", get(crate::transport::upgrade))
            .with_state(self.clone())
    }
    fn start(&self) {
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let mut stop = self.inner.shutdown.subscribe();
        let period = self.inner.config.cleanup_interval;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {biased;
                    _=stop.changed()=>break,
                    _=interval.tick()=> {let Some(inner)=weak.upgrade() else {break;};cleanup(&inner).await;}
                }
            }
        });
    }
    pub fn metrics(&self) -> MetricsSnapshot {
        let mut snapshot = self.inner.metrics.snapshot();
        snapshot.active_connections =
            (self.inner.config.max_connections - self.inner.connections.available_permits()) as u64;
        snapshot.unauthenticated_connections = (self.inner.config.max_unauthenticated_connections
            - self.inner.unauthenticated.available_permits())
            as u64;
        snapshot
    }
    pub async fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
        let mut state = self.inner.state.lock().await;
        for session in state.sessions.values() {
            if let Some(output) = &session.output {
                output.close();
            }
        }
        let rooms: Vec<_> = state.rooms.values().cloned().collect();
        state.sessions.clear();
        state.rate_keys.clear();
        self.inner
            .metrics
            .active_sessions
            .store(0, Ordering::Relaxed);
        self.inner.metrics.rate_keys.store(0, Ordering::Relaxed);
        drop(state);
        for handle in rooms {
            let _ = handle.control.send(Control::Stop).await;
            handle.control.closed().await;
        }
        // Acknowledged directory cleanup is performed by the supervisor of each room.
        while self.inner.metrics.active_rooms.load(Ordering::Acquire) > 0 {
            tokio::task::yield_now().await;
        }
        // Transport tasks observe the shutdown signal and release both admission permits.
        if let Ok(permits) = self
            .inner
            .connections
            .clone()
            .acquire_many_owned(self.inner.config.max_connections as u32)
            .await
        {
            drop(permits);
        }
    }
}
pub(crate) fn random_id(prefix: &str) -> Result<String, Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::new("SERVER_BUSY", "OS entropy unavailable"))?;
    let encoded: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{prefix}{encoded}"))
}
pub(crate) fn token_hash(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Inner {
    fn deadline_millis(&self, deadline: Instant) -> u64 {
        self.unix_origin.saturating_add(
            deadline
                .saturating_duration_since(self.clock_origin)
                .as_millis() as u64,
        )
    }
    pub fn has_game(&self, id: &str) -> bool {
        self.modules
            .read()
            .is_ok_and(|modules| modules.contains_key(id))
    }
    pub async fn issue(&self, game: &str, source: String) -> Result<Value, Error> {
        if *self.shutdown.borrow() {
            return Err(Error::busy());
        }
        if !self.has_game(game) {
            return Err(Error::new("GAME_NOT_FOUND", "game_id is not registered"));
        }
        let now = Instant::now();
        let mut state = self.state.lock().await;
        state.rate_keys.retain(|_, key| {
            now.saturating_duration_since(key.last_seen) < self.config.rate_key_ttl
        });
        self.metrics
            .rate_keys
            .store(state.rate_keys.len() as u64, Ordering::Relaxed);
        if state.sessions.len() >= self.config.max_sessions {
            inc(&self.metrics.admission_rejections);
            return Err(Error::busy());
        }
        if !state.rate_keys.contains_key(&source)
            && state.rate_keys.len() >= self.config.max_rate_keys
        {
            inc(&self.metrics.admission_rejections);
            return Err(Error::busy());
        }
        let key = state.rate_keys.entry(source).or_insert_with(|| RateKey {
            bucket: Bucket::new(self.config.guest_burst),
            last_seen: now,
        });
        key.last_seen = now;
        if !key.bucket.take(
            self.config.guest_rate_per_second,
            self.config.guest_burst,
            now,
        ) {
            return Err(Error::new(
                "RATE_LIMITED",
                "guest issuance rate exceeded for source",
            ));
        }
        if !state.global_rate.take(
            self.config.global_guest_rate_per_second,
            self.config.global_guest_burst,
            now,
        ) {
            return Err(Error::new(
                "RATE_LIMITED",
                "global guest issuance rate exceeded",
            ));
        }
        let id = random_id("g_")?;
        let token = random_id("")?;
        let deadline = now + self.config.guest_ttl;
        let expires_at = self.deadline_millis(deadline);
        state.sessions.insert(
            token_hash(&token),
            Session {
                id: id.clone(),
                game_id: game.into(),
                deadline,
                expires_at,
                generation: 0,
                output: None,
                room: None,
                reservation: None,
            },
        );
        self.metrics
            .active_sessions
            .store(state.sessions.len() as u64, Ordering::Relaxed);
        self.metrics
            .rate_keys
            .store(state.rate_keys.len() as u64, Ordering::Relaxed);
        inc(&self.metrics.guests_issued);
        Ok(json!({"guest_id":id,"token":token,"expires_at":expires_at,"game_id":game}))
    }
    pub async fn authenticate(
        &self,
        game: &str,
        token: &str,
        request: &str,
        output: Arc<Output>,
    ) -> Result<(TokenHash, u64, Instant, Value), Error> {
        if token.len() > 512 {
            return Err(Error::new("UNAUTHENTICATED", "invalid or expired token"));
        }
        let key = token_hash(token);
        let mut state = self.state.lock().await;
        let session = state
            .sessions
            .get_mut(&key)
            .ok_or_else(|| Error::new("UNAUTHENTICATED", "invalid or expired token"))?;
        if session.deadline <= Instant::now() {
            return Err(Error::new("UNAUTHENTICATED", "invalid or expired token"));
        }
        if session.game_id != game {
            return Err(Error::new("GAME_MISMATCH", "token belongs to another game"));
        }
        if !output.is_alive() {
            return Err(Error::new(
                "CANCELLED",
                "connection closed during authentication",
            ));
        }
        session.generation = session.generation.checked_add(1).ok_or_else(Error::busy)?;
        let generation = session.generation;
        if let Some(previous) = session.output.take() {
            previous.close();
        }
        let room_ref = session.room.clone();
        let player = session.id.clone();
        if let Some(reference) = room_ref {
            let reservation = if let Some(handle) = state
                .rooms
                .get(&(reference.game_id.clone(), reference.id.clone()))
                .filter(|h| h.reference == reference)
            {
                let (tx, rx) = oneshot::channel();
                if handle
                    .control
                    .send(Control::Replace {
                        player,
                        generation,
                        reply: tx,
                    })
                    .await
                    .is_ok()
                {
                    rx.await.ok().flatten()
                } else {
                    None
                }
            } else {
                None
            };
            let session = state.sessions.get_mut(&key).expect("session is serialized");
            session.reservation = reservation;
            if reservation.is_none() {
                session.room = None;
            }
        }
        let session = state.sessions.get_mut(&key).expect("session is serialized");
        session.output = Some(output);
        let response = json!({"v":1,"type":"session.authenticated","request_id":request,"guest_id":session.id,
            "expires_at":session.expires_at,"game_id":game,"room_id":session.room.as_ref().map(|r|&r.id),
            "room_instance_id":session.room.as_ref().map(|r|&r.instance),"can_resume":session.reservation.is_some_and(|d|d>Instant::now()),
            "reservation_expires_at":session.reservation.map(|deadline|self.deadline_millis(deadline))});
        Ok((key, generation, session.deadline, response))
    }
    pub async fn disconnect(&self, key: TokenHash, generation: u64) {
        let mut state = self.state.lock().await;
        let Some(session) = state
            .sessions
            .get_mut(&key)
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        session.output = None;
        let player = session.id.clone();
        let reference = session.room.clone();
        if let Some(reference) = reference {
            if let Some(handle) = state
                .rooms
                .get(&(reference.game_id.clone(), reference.id.clone()))
                .filter(|h| h.reference == reference)
            {
                let (tx, rx) = oneshot::channel();
                let _ = handle
                    .control
                    .send(Control::Disconnect {
                        player,
                        generation,
                        reply: tx,
                    })
                    .await;
                let _ = rx.await;
                let session = state.sessions.get_mut(&key).expect("session is serialized");
                session.reservation.get_or_insert(
                    (Instant::now() + self.config.reconnect_grace).min(session.deadline),
                );
            } else if let Some(session) = state.sessions.get_mut(&key) {
                session.room = None;
                session.reservation = None;
            }
        }
    }
    pub async fn command(
        self: &Arc<Self>,
        key: TokenHash,
        generation: u64,
        game: &str,
        output: Arc<Output>,
        request: &Request,
    ) -> Result<(Value, bool), Error> {
        if request.game_id.as_ref().is_some_and(|id| id != game) {
            return Err(Error::new(
                "GAME_MISMATCH",
                "command game_id differs from authenticated game",
            ));
        }
        let mut state = self.state.lock().await;
        let session = state
            .sessions
            .get(&key)
            .ok_or_else(|| Error::new("UNAUTHENTICATED", "session is no longer active"))?;
        if session.generation != generation
            || session.deadline <= Instant::now()
            || !output.is_alive()
        {
            return Err(Error::new(
                "UNAUTHENTICATED",
                "connection is no longer authorized",
            ));
        }
        if session.game_id != game {
            return Err(Error::new(
                "GAME_MISMATCH",
                "session belongs to another game",
            ));
        }
        let player = session.id.clone();
        let deadline = session.deadline;
        let membership = session.room.clone();
        match &request.command {
            Command::Authenticate { .. } => Err(Error::new(
                "INVALID_MESSAGE",
                "connection is already authenticated",
            )),
            Command::Logout => {
                if let Some(reference) = membership {
                    leave(&state, &reference, &player, None).await;
                }
                state.sessions.remove(&key);
                self.metrics
                    .active_sessions
                    .store(state.sessions.len() as u64, Ordering::Relaxed);
                Ok((
                    json!({"v":1,"type":"session.logged_out","request_id":request.id}),
                    false,
                ))
            }
            Command::List { cursor, limit } => {
                if *limit == 0 || *limit > 100 {
                    return Err(Error::new("INVALID_MESSAGE", "limit must be 1..=100"));
                }
                let mut rooms = Vec::new();
                let mut next_cursor = None;
                for handle in state.rooms.values().filter(|h| {
                    h.reference.game_id == game
                        && h.public
                        && h.visible
                        && cursor
                            .as_ref()
                            .is_none_or(|cursor| h.reference.id > *cursor)
                }) {
                    if rooms.len() == *limit as usize {
                        next_cursor = rooms
                            .last()
                            .and_then(|v: &Value| v["room_id"].as_str())
                            .map(str::to_owned);
                        break;
                    }
                    let (tx, rx) = oneshot::channel();
                    if handle
                        .control
                        .send(Control::List { reply: tx })
                        .await
                        .is_err()
                    {
                        continue;
                    }
                    if let Ok(members) = rx.await {
                        rooms.push(json!({"game_id":game,"room_id":handle.reference.id,"room_instance_id":handle.reference.instance,"members":members,"capacity":handle.capacity}));
                    }
                }
                Ok((
                    json!({"v":1,"type":"room.listed","request_id":request.id,"rooms":rooms,"next_cursor":next_cursor}),
                    false,
                ))
            }
            Command::Create { settings } => {
                if membership.is_some() {
                    return Err(Error::new(
                        "ALREADY_IN_ROOM",
                        "leave the current room before creating another",
                    ));
                }
                if state.rooms.len() >= self.config.max_rooms {
                    inc(&self.metrics.admission_rejections);
                    return Err(Error::busy());
                }
                let mut settings = settings
                    .as_object()
                    .cloned()
                    .ok_or_else(|| Error::new("INVALID_SETTINGS", "settings must be an object"))?;
                let capacity = match settings.remove("capacity") {
                    None => self.config.room_capacity,
                    Some(value) => value
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n > 0 && *n <= self.config.room_capacity)
                        .ok_or_else(|| {
                            Error::new(
                                "INVALID_SETTINGS",
                                "capacity must be within server room capacity",
                            )
                        })?,
                };
                let public = match settings.remove("public") {
                    None => true,
                    Some(Value::Bool(value)) => value,
                    _ => return Err(Error::new("INVALID_SETTINGS", "public must be a boolean")),
                };
                let module = self
                    .modules
                    .read()
                    .map_err(|_| Error::busy())?
                    .get(game)
                    .cloned()
                    .ok_or_else(|| Error::new("GAME_NOT_FOUND", "game is not registered"))?;
                let context = GameContext {
                    capacity,
                    tick_interval: self.config.tick_interval(),
                };
                let game_state = catch_unwind(AssertUnwindSafe(|| {
                    module.create(&Value::Object(settings), &context)
                }))
                .map_err(|_| Error::new("GAME_FAILED", "game factory panicked"))?
                .map_err(Error::from)?;
                let reference = RoomRef {
                    game_id: game.into(),
                    id: random_id("r_")?,
                    instance: random_id("i_")?,
                };
                let (control, controls) = mpsc::channel(64);
                let (inputs, input_rx) = mpsc::channel(self.config.room_input_capacity);
                let handle = RoomHandle {
                    reference: reference.clone(),
                    control,
                    inputs,
                    public,
                    visible: false,
                    capacity,
                };
                state.rooms.insert(
                    (reference.game_id.clone(), reference.id.clone()),
                    handle.clone(),
                );
                inc(&self.metrics.active_rooms);
                let room_config = self.config.clone();
                let metrics = self.metrics.clone();
                let room_ref = reference.clone();
                let task = tokio::spawn(room::run(
                    room_ref,
                    room_config,
                    capacity,
                    game_state,
                    metrics,
                    controls,
                    input_rx,
                ));
                let weak = Arc::downgrade(self);
                let supervised = reference.clone();
                tokio::spawn(async move {
                    let result = task.await;
                    if let Some(inner) = weak.upgrade() {
                        inner.closed(supervised, result).await;
                    }
                });
                let result = admit(
                    &handle, &player, generation, deadline, output, request, false,
                )
                .await;
                match result {
                    Ok(response) => {
                        if let Some(handle) = state
                            .rooms
                            .get_mut(&(reference.game_id.clone(), reference.id.clone()))
                        {
                            handle.visible = true;
                        }
                        let session = state.sessions.get_mut(&key).expect("session is serialized");
                        session.room = Some(reference);
                        session.reservation = None;
                        Ok((response, true))
                    }
                    Err(error) => {
                        let _ = handle.control.send(Control::Stop).await;
                        handle.control.closed().await;
                        state
                            .rooms
                            .remove(&(reference.game_id.clone(), reference.id.clone()));
                        Err(error)
                    }
                }
            }
            Command::Join {
                room_id,
                room_instance_id,
            }
            | Command::Resume {
                room_id,
                room_instance_id,
            } => {
                let resume = matches!(request.command, Command::Resume { .. });
                let handle = lookup(&state, game, room_id, room_instance_id)?;
                if resume {
                    if membership.as_ref() != Some(&handle.reference) {
                        return Err(Error::new(
                            "RESUME_EXPIRED",
                            "session has no reservation for this room instance",
                        ));
                    }
                } else if membership.is_some() {
                    return Err(Error::new(
                        "ALREADY_IN_ROOM",
                        "leave the current room before joining another",
                    ));
                }
                let reference = handle.reference.clone();
                let response = admit(
                    handle, &player, generation, deadline, output, request, resume,
                )
                .await?;
                let session = state.sessions.get_mut(&key).expect("session is serialized");
                session.room = Some(reference);
                session.reservation = None;
                Ok((response, true))
            }
            Command::Leave {
                room_id,
                room_instance_id,
            } => {
                if let Some(reference) = membership {
                    if reference.id != *room_id {
                        return Err(Error::new(
                            "ROOM_NOT_FOUND",
                            "session is not a member of this room",
                        ));
                    }
                    if reference.instance != *room_instance_id {
                        return Err(Error::new(
                            "STALE_INSTANCE",
                            "room instance does not match membership",
                        ));
                    }
                    leave(&state, &reference, &player, Some(generation)).await;
                    let session = state.sessions.get_mut(&key).expect("session is serialized");
                    session.room = None;
                    session.reservation = None;
                    output.latest.send_replace(None);
                }
                Ok((
                    json!({"v":1,"type":"room.left","request_id":request.id,"room_id":room_id,"room_instance_id":room_instance_id}),
                    false,
                ))
            }
            Command::Input {
                room_id,
                room_instance_id,
                input_seq,
                payload,
            } => {
                let handle = lookup(&state, game, room_id, room_instance_id)?;
                if membership.as_ref() != Some(&handle.reference) {
                    return Err(Error::new(
                        "ROOM_NOT_FOUND",
                        "session is not a member of this room",
                    ));
                }
                let (tx, rx) = oneshot::channel();
                let input = Input {
                    player,
                    generation,
                    sequence: *input_seq,
                    payload: payload.clone(),
                    received: Instant::now(),
                    reply: tx,
                };
                match handle.inputs.try_send(input) {
                    Ok(()) => {
                        crate::metrics::max(
                            &self.metrics.max_input_queue_depth,
                            (handle.inputs.max_capacity() - handle.inputs.capacity()) as u64,
                        );
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        inc(&self.metrics.shared_queue_overloads);
                        return Err(Error::busy());
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Err(Error::new("ROOM_NOT_FOUND", "room task has ended"));
                    }
                }
                drop(state);
                rx.await
                    .map_err(|_| Error::new("ROOM_NOT_FOUND", "room task has ended"))??;
                Ok((
                    json!({"v":1,"type":"game.input.accepted","request_id":request.id,"input_seq":input_seq}),
                    false,
                ))
            }
        }
    }
    async fn closed(
        &self,
        reference: RoomRef,
        result: Result<&'static str, tokio::task::JoinError>,
    ) {
        let reason = match result {
            Ok(reason) => reason,
            Err(error) => {
                if error.is_panic() {
                    inc(&self.metrics.room_panics);
                }
                "game_panic"
            }
        };
        let mut state = self.state.lock().await;
        state
            .rooms
            .remove(&(reference.game_id.clone(), reference.id.clone()));
        for session in state
            .sessions
            .values_mut()
            .filter(|s| s.room.as_ref() == Some(&reference))
        {
            session.room = None;
            session.reservation = None;
            if let Some(output) = &session.output {
                output.latest.send_replace(None);
                output.send(json!({"v":1,"type":"room.closed","room_id":reference.id,"room_instance_id":reference.instance,"reason":reason}));
            }
        }
        self.metrics.active_rooms.fetch_sub(1, Ordering::Release);
        tracing::info!(game_id=%reference.game_id,room_id=%reference.id,room_instance_id=%reference.instance,reason,"room closed");
    }
}
fn lookup<'a>(
    state: &'a State,
    game: &str,
    id: &str,
    instance: &str,
) -> Result<&'a RoomHandle, Error> {
    let handle = state
        .rooms
        .get(&(game.to_owned(), id.to_owned()))
        .ok_or_else(|| {
            if state
                .rooms
                .values()
                .any(|handle| handle.reference.id == id && handle.reference.game_id != game)
            {
                Error::new("GAME_MISMATCH", "room belongs to another game")
            } else {
                Error::new("ROOM_NOT_FOUND", "room does not exist")
            }
        })?;
    if handle.reference.game_id != game {
        return Err(Error::new("GAME_MISMATCH", "room belongs to another game"));
    }
    if handle.reference.instance != instance {
        return Err(Error::new("STALE_INSTANCE", "room instance has changed"));
    }
    if !handle.visible {
        return Err(Error::new(
            "ROOM_NOT_FOUND",
            "room admission has not committed",
        ));
    }
    Ok(handle)
}
async fn admit(
    handle: &RoomHandle,
    player: &str,
    generation: u64,
    deadline: Instant,
    output: Arc<Output>,
    request: &Request,
    resume: bool,
) -> Result<Value, Error> {
    let (tx, rx) = oneshot::channel();
    handle
        .control
        .send(Control::Enter(Admission {
            player: player.into(),
            generation,
            token_deadline: deadline,
            output,
            request_id: request.id.clone(),
            resume,
            reply: tx,
        }))
        .await
        .map_err(|_| Error::new("ROOM_NOT_FOUND", "room task has ended"))?;
    rx.await
        .map_err(|_| Error::new("ROOM_NOT_FOUND", "room task has ended"))?
}
async fn leave(state: &State, reference: &RoomRef, player: &str, generation: Option<u64>) {
    if let Some(handle) = state
        .rooms
        .get(&(reference.game_id.clone(), reference.id.clone()))
        .filter(|h| h.reference == *reference)
    {
        let (tx, rx) = oneshot::channel();
        let _ = handle
            .control
            .send(Control::Leave {
                player: player.into(),
                generation,
                reply: tx,
            })
            .await;
        let _ = rx.await;
    }
}
async fn cleanup(inner: &Inner) {
    let mut state = inner.state.lock().await;
    let now = Instant::now();
    let expired: Vec<_> = state
        .sessions
        .iter()
        .filter(|(_, s)| s.deadline <= now)
        .map(|(key, _)| *key)
        .collect();
    for key in expired {
        if let Some(session) = state.sessions.remove(&key) {
            if let Some(output) = session.output {
                output.close();
            }
            if let Some(reference) = session.room {
                leave(&state, &reference, &session.id, None).await;
            }
        }
    }
    let reservations: Vec<_> = state
        .sessions
        .iter()
        .filter(|(_, s)| s.reservation.is_some_and(|d| d <= now))
        .map(|(key, _)| *key)
        .collect();
    for key in reservations {
        let session = state.sessions.get_mut(&key).expect("session is serialized");
        let reference = session.room.take();
        let player = session.id.clone();
        session.reservation = None;
        if let Some(reference) = reference {
            leave(&state, &reference, &player, None).await;
        }
    }
    state
        .rate_keys
        .retain(|_, key| now.saturating_duration_since(key.last_seen) < inner.config.rate_key_ttl);
    inner
        .metrics
        .active_sessions
        .store(state.sessions.len() as u64, Ordering::Relaxed);
    inner
        .metrics
        .rate_keys
        .store(state.rate_keys.len() as u64, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Game;
    use std::{collections::BTreeSet, time::Duration};
    struct TestModule {
        close_on_create: Option<Arc<Output>>,
    }
    impl GameModule for TestModule {
        fn create(&self, _: &Value, _: &GameContext) -> Result<Box<dyn Game>, crate::GameError> {
            if let Some(output) = &self.close_on_create {
                output.close();
            }
            Ok(Box::<TestGame>::default())
        }
    }
    #[derive(Default)]
    struct TestGame {
        players: BTreeSet<String>,
    }
    impl Game for TestGame {
        fn player_joined(&mut self, id: &str) {
            self.players.insert(id.into());
        }
        fn player_left(&mut self, id: &str) {
            self.players.remove(id);
        }
        fn player_connection_changed(&mut self, _: &str, _: bool) {}
        fn validate_input(&self, _: &str, _: &Value) -> Result<(), crate::GameError> {
            Ok(())
        }
        fn apply_input(&mut self, _: &str, _: &Value) {}
        fn tick(&mut self, _: Duration) {}
        fn snapshot(&self) -> Value {
            json!({"players":self.players})
        }
    }
    fn output(capacity: usize) -> (Arc<Output>, mpsc::Receiver<Value>) {
        let (reliable, rx) = mpsc::channel(capacity);
        let (latest, _) = watch::channel(None);
        let (stop, _) = watch::channel(false);
        (
            Arc::new(Output {
                reliable,
                latest,
                stop,
                alive: AtomicBool::new(true),
                metrics: Arc::new(Metrics::default()),
            }),
            rx,
        )
    }
    #[tokio::test(start_paused = true)]
    async fn manual_clock_reclaims_session_rate_keys_room_and_reservation_before_reissue() {
        let config = Config {
            max_sessions: 1,
            max_rate_keys: 1,
            guest_ttl: Duration::from_secs(1),
            rate_key_ttl: Duration::from_secs(2),
            ..Config::default()
        };
        let runtime = Campfire::new(config).unwrap();
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        let guest = runtime.inner.issue("a", "source-a".into()).await.unwrap();
        assert_eq!(
            runtime
                .inner
                .issue("a", "source-a".into())
                .await
                .unwrap_err()
                .code,
            "SERVER_BUSY"
        );
        let (out, _receiver) = output(8);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let create = crate::protocol::parse(
            r#"{"v":1,"type":"room.create","request_id":"create","settings":{}}"#,
        )
        .unwrap();
        runtime
            .inner
            .command(key, generation, "a", out.clone(), &create)
            .await
            .unwrap();
        assert_eq!(runtime.metrics().active_rooms, 1);
        assert!(
            runtime.inner.state.lock().await.sessions[&key]
                .room
                .is_some()
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        cleanup(&runtime.inner).await;
        assert!(!out.is_alive());
        assert_eq!(runtime.metrics().active_sessions, 0);
        assert_eq!(runtime.metrics().rate_keys, 1);
        assert_eq!(
            runtime
                .inner
                .issue("a", "source-b".into())
                .await
                .unwrap_err()
                .code,
            "SERVER_BUSY"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        cleanup(&runtime.inner).await;
        assert_eq!(runtime.metrics().rate_keys, 0);
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert_eq!(runtime.metrics().active_rooms, 0);
        assert!(runtime.inner.state.lock().await.rooms.is_empty());
        let replacement = runtime.inner.issue("a", "source-b".into()).await.unwrap();
        assert_ne!(guest["token"], replacement["token"]);
        assert_eq!(runtime.metrics().active_sessions, 1);
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn cancelled_create_rolls_back_temporary_room_and_refunds_room_capacity() {
        let runtime = Campfire::new(Config {
            max_rooms: 1,
            ..Config::default()
        })
        .unwrap();
        let (out, _receiver) = output(8);
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: Some(out.clone()),
                },
            )
            .unwrap();
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let create = crate::protocol::parse(
            r#"{"v":1,"type":"room.create","request_id":"create","settings":{}}"#,
        )
        .unwrap();
        assert_eq!(
            runtime
                .inner
                .command(key, generation, "a", out, &create)
                .await
                .unwrap_err()
                .code,
            "CANCELLED"
        );
        assert!(runtime.inner.state.lock().await.rooms.is_empty());
        assert!(
            runtime.inner.state.lock().await.sessions[&key]
                .room
                .is_none()
        );
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(runtime.metrics().active_rooms, 0);
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn shutdown_delivers_stop_even_when_control_queue_is_full() {
        let runtime = Campfire::new(Config::default()).unwrap();
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let (out, _receiver) = output(8);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let create = crate::protocol::parse(
            r#"{"v":1,"type":"room.create","request_id":"create","settings":{}}"#,
        )
        .unwrap();
        runtime
            .inner
            .command(key, generation, "a", out, &create)
            .await
            .unwrap();
        let handle = runtime
            .inner
            .state
            .lock()
            .await
            .rooms
            .values()
            .next()
            .unwrap()
            .clone();
        for _ in 0..handle.control.max_capacity() {
            let (tx, _) = oneshot::channel();
            handle
                .control
                .try_send(Control::List { reply: tx })
                .unwrap();
        }
        assert_eq!(handle.control.capacity(), 0);
        tokio::time::timeout(Duration::from_secs(1), runtime.shutdown())
            .await
            .unwrap();
        assert!(handle.control.is_closed());
        assert_eq!(runtime.metrics().active_rooms, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn reservation_expiry_timestamp_is_stable_across_manual_clock_reauthentication() {
        let runtime = Campfire::new(Config::default()).unwrap();
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let token = guest["token"].as_str().unwrap();
        let (out, _receiver) = output(8);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", token, "auth", out.clone())
            .await
            .unwrap();
        let create = crate::protocol::parse(
            r#"{"v":1,"type":"room.create","request_id":"create","settings":{}}"#,
        )
        .unwrap();
        runtime
            .inner
            .command(key, generation, "a", out, &create)
            .await
            .unwrap();
        let (out, _receiver2) = output(8);
        let (_, _, _, first) = runtime
            .inner
            .authenticate("a", token, "replace1", out)
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(10)).await;
        let (out, _receiver3) = output(8);
        let (_, _, _, second) = runtime
            .inner
            .authenticate("a", token, "replace2", out)
            .await
            .unwrap();
        assert_eq!(
            first["reservation_expires_at"],
            second["reservation_expires_at"]
        );
        runtime.shutdown().await;
    }
}
