use crate::{
    AccountAuthenticator, AuthError, Config, ConfigError, GameContext, GameModule, PlayerIdentity,
    metrics::{Metrics, MetricsSnapshot, inc},
    observation::{ObservationDirectory, PublishedRoom, RuntimeObserver},
    output::Output,
    protocol::{Command, Error, Request},
    rate::Bucket,
    room::{self, Admission, Control, GameCommand, Input, RoomHandle, RoomRef},
};
use axum::{
    Router,
    routing::{get, post},
};
use futures_util::FutureExt;
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
struct RotationGuard(Arc<room::Rotation>);
impl Drop for RotationGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(crate) struct Session {
    pub id: String,
    pub game_id: String,
    pub deadline: Instant,
    pub expires_at: u64,
    pub generation: u64,
    pub output: Option<Arc<Output>>,
    pub room: Option<RoomRef>,
    pub reservation: Option<Instant>,
    pub identity: PlayerIdentity,
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
    pub authenticator: Option<Arc<dyn AccountAuthenticator>>,
    pub observations: ObservationDirectory,
    started: AtomicBool,
    pub clock_origin: Instant,
    unix_origin: u64,
}
#[derive(Clone)]
pub struct Campfire {
    pub(crate) inner: Arc<Inner>,
}
impl Campfire {
    pub fn new(config: Config) -> Result<Self, ConfigError> {
        Self::build(config, None)
    }
    pub fn with_authenticator(
        config: Config,
        authenticator: Arc<dyn AccountAuthenticator>,
    ) -> Result<Self, ConfigError> {
        Self::build(config, Some(authenticator))
    }
    fn build(
        config: Config,
        authenticator: Option<Arc<dyn AccountAuthenticator>>,
    ) -> Result<Self, ConfigError> {
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
                authenticator,
                observations: ObservationDirectory::default(),
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
        modules.insert(game_id.clone(), Arc::new(module));
        self.inner.observations.register_game(game_id);
        Ok(())
    }
    pub fn router(&self) -> Router {
        self.start();
        Router::new()
            .route("/health", get(crate::transport::health))
            .route("/v1/games/{game_id}/guests", post(crate::transport::guest))
            .route(
                "/v1/games/{game_id}/sessions",
                post(crate::transport::account).layer(axum::extract::DefaultBodyLimit::max(
                    self.inner.config.max_frame_bytes,
                )),
            )
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
        self.inner.metrics_snapshot()
    }
    pub fn observer(&self) -> RuntimeObserver {
        RuntimeObserver {
            inner: Arc::downgrade(&self.inner),
        }
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
    pub(crate) fn metrics_snapshot(&self) -> MetricsSnapshot {
        let mut snapshot = self.metrics.snapshot();
        snapshot.active_connections =
            (self.config.max_connections - self.connections.available_permits()) as u64;
        snapshot.unauthenticated_connections = (self.config.max_unauthenticated_connections
            - self.unauthenticated.available_permits())
            as u64;
        snapshot
    }
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
    fn take_issue_rate(
        &self,
        state: &mut State,
        source: String,
        now: Instant,
    ) -> Result<(), Error> {
        state.rate_keys.retain(|_, key| {
            now.saturating_duration_since(key.last_seen) < self.config.rate_key_ttl
        });
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
        let allowed = key.bucket.take(
            self.config.guest_rate_per_second,
            self.config.guest_burst,
            now,
        ) && state.global_rate.take(
            self.config.global_guest_rate_per_second,
            self.config.global_guest_burst,
            now,
        );
        self.metrics
            .rate_keys
            .store(state.rate_keys.len() as u64, Ordering::Relaxed);
        if allowed {
            Ok(())
        } else {
            Err(Error::new("RATE_LIMITED", "session issuance rate exceeded"))
        }
    }
    pub async fn issue_account(
        &self,
        game: &str,
        credential: &str,
        source: String,
    ) -> Result<Value, Error> {
        let began = Instant::now();
        if *self.shutdown.borrow() {
            return Err(Error::busy());
        }
        if !self.has_game(game) {
            return Err(Error::new("GAME_NOT_FOUND", "game is not registered"));
        }
        let authenticator = self.authenticator.as_ref().ok_or_else(|| {
            Error::new(
                "ACCOUNT_AUTH_DISABLED",
                "account authentication is not configured",
            )
        })?;
        if credential.trim().is_empty() || credential.len() > self.config.max_frame_bytes {
            return Err(Error::new(
                "INVALID_MESSAGE",
                "credential is missing or exceeds the configured limit",
            ));
        }
        let _permit = self
            .unauthenticated
            .try_acquire()
            .map_err(|_| Error::busy())?;
        {
            let mut state = self.state.lock().await;
            self.take_issue_rate(&mut state, source, began)?;
        }
        let unavailable =
            || Error::new("AUTH_UNAVAILABLE", "account authentication is unavailable");
        // Both synchronous verifier construction and future polling can panic.
        let verification = catch_unwind(AssertUnwindSafe(|| {
            authenticator.authenticate(game, credential)
        }))
        .map_err(|_| unavailable())?;
        let grant = tokio::time::timeout(
            self.config.auth_timeout,
            AssertUnwindSafe(verification).catch_unwind(),
        )
        .await
        .map_err(|_| Error::new("AUTH_TIMEOUT", "account authentication timed out"))?
        .map_err(|_| unavailable())?
        .map_err(|error| match error {
            AuthError::InvalidCredential => {
                Error::new("UNAUTHENTICATED", "credential was rejected")
            }
            AuthError::Unavailable => unavailable(),
        })?;
        if grant.valid_for.is_zero()
            || grant.valid_for > std::time::Duration::from_secs(366 * 24 * 3600)
            || grant.identity.issuer.trim().is_empty()
            || grant.identity.issuer.len() > 128
            || grant.identity.subject.trim().is_empty()
            || grant.identity.subject.len() > 256
            || serde_json::to_vec(&grant.identity.claims)
                .map_err(|_| unavailable())?
                .len()
                > self.config.max_frame_bytes
        {
            return Err(unavailable());
        }
        let deadline = began
            .checked_add(grant.valid_for.min(self.config.guest_ttl))
            .ok_or_else(unavailable)?;
        if deadline <= Instant::now() {
            return Err(unavailable());
        }
        if *self.shutdown.borrow() {
            return Err(Error::busy());
        }
        let token = random_id("")?;
        let new_key = token_hash(&token);
        let mut state = self.state.lock().await;
        if *self.shutdown.borrow() {
            return Err(Error::busy());
        }
        if deadline <= Instant::now() {
            return Err(unavailable());
        }
        let previous = state
            .sessions
            .iter()
            .find_map(|(key, session)| match &session.identity {
                PlayerIdentity::Account(identity)
                    if session.game_id == game
                        && identity.issuer == grant.identity.issuer
                        && identity.subject == grant.identity.subject =>
                {
                    Some(*key)
                }
                _ => None,
            });
        let identity = PlayerIdentity::Account(grant.identity);
        // Expired sessions cannot be resurrected even when periodic cleanup has not run yet.
        if let Some(key) = previous.filter(|key| state.sessions[key].deadline <= Instant::now()) {
            let expired = state.sessions.get(&key).expect("located expired session");
            if let Some(output) = &expired.output {
                output.close();
            }
            if let Some(reference) = &expired.room {
                leave(&state, reference, &expired.id, None).await;
            }
            state.sessions.remove(&key);
            self.metrics
                .active_sessions
                .store(state.sessions.len() as u64, Ordering::Relaxed);
        }
        // Expired-seat cleanup can await the actor; recheck before issuing a replacement session.
        if *self.shutdown.borrow() {
            return Err(Error::busy());
        }
        if deadline <= Instant::now() {
            return Err(unavailable());
        }
        let previous = previous.filter(|key| state.sessions.contains_key(key));
        let session = if let Some(key) = previous {
            let old = &state.sessions[&key];
            let generation = old.generation.checked_add(1).ok_or_else(Error::busy)?;
            let player = old.id.clone();
            let mut room = old.room.clone();
            let mut reservation = None;
            let rotation = RotationGuard(room::Rotation::new(deadline, identity.clone()));
            if let Some(reference) = &room {
                if let Some(handle) = state
                    .rooms
                    .get(&(reference.game_id.clone(), reference.id.clone()))
                    .filter(|handle| &handle.reference == reference)
                {
                    let (tx, rx) = oneshot::channel();
                    if handle
                        .control
                        .send(Control::Replace {
                            player: player.clone(),
                            generation,
                            rotation: Some(rotation.0.clone()),
                            reply: tx,
                        })
                        .await
                        .is_ok()
                    {
                        reservation = rx.await.ok().flatten();
                    }
                }
                if reservation.is_none() {
                    room = None;
                }
            }
            // Preserve the old map entry across every await; cancellation cannot orphan its seat.
            if *self.shutdown.borrow() {
                return Err(Error::busy());
            }
            if deadline <= Instant::now() {
                return Err(unavailable());
            }
            if room.is_some() && !rotation.0.commit() {
                return Err(Error::busy());
            }
            // No await separates the shared actor decision from this token-map swap.
            let old = state.sessions.remove(&key).expect("rotation is serialized");
            if let Some(output) = old.output {
                output.close();
            }
            Session {
                id: player,
                game_id: game.into(),
                deadline,
                expires_at: self.deadline_millis(deadline),
                generation,
                output: None,
                room,
                reservation,
                identity,
            }
        } else {
            if state.sessions.len() >= self.config.max_sessions {
                inc(&self.metrics.admission_rejections);
                return Err(Error::busy());
            }
            Session {
                id: random_id("a_")?,
                game_id: game.into(),
                deadline,
                expires_at: self.deadline_millis(deadline),
                generation: 0,
                output: None,
                room: None,
                reservation: None,
                identity,
            }
        };
        let response = json!({"player_id":session.id,"token":token,"expires_at":session.expires_at,"game_id":game});
        state.sessions.insert(new_key, session);
        self.metrics
            .active_sessions
            .store(state.sessions.len() as u64, Ordering::Relaxed);
        Ok(response)
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
                identity: PlayerIdentity::Guest,
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
                        rotation: None,
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
        let response = json!({"v":1,"type":"session.authenticated","request_id":request,"guest_id":session.id,"player_id":session.id,
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
        let identity = session.identity.clone();
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
                let (commands, command_rx) = mpsc::channel(self.config.room_input_capacity);
                let (observation, observed) =
                    watch::channel(PublishedRoom::new(&reference, public, capacity));
                let handle = RoomHandle {
                    reference: reference.clone(),
                    control,
                    inputs,
                    commands,
                    public,
                    visible: false,
                    capacity,
                };
                state.rooms.insert(
                    (reference.game_id.clone(), reference.id.clone()),
                    handle.clone(),
                );
                self.observations.insert(&reference, observed);
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
                    room::Channels {
                        observation,
                        controls,
                        inputs: input_rx,
                        commands: command_rx,
                    },
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
                    &handle,
                    AdmissionOptions {
                        player: &player,
                        generation,
                        deadline,
                        output,
                        request,
                        resume: false,
                        identity,
                    },
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
                    handle,
                    AdmissionOptions {
                        player: &player,
                        generation,
                        deadline,
                        output,
                        request,
                        resume,
                        identity,
                    },
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
            Command::Execute {
                room_id,
                room_instance_id,
                command_seq,
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
                let command = GameCommand {
                    player,
                    generation,
                    sequence: *command_seq,
                    payload: payload.clone(),
                    request_id: request.id.clone(),
                    reply: tx,
                };
                match handle.commands.try_send(command) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        inc(&self.metrics.shared_queue_overloads);
                        return Err(Error::busy());
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Err(Error::new("ROOM_NOT_FOUND", "room task has ended"));
                    }
                }
                drop(state);
                Ok((
                    rx.await
                        .map_err(|_| Error::new("ROOM_NOT_FOUND", "room task has ended"))??,
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
        self.observations.remove(&reference);
        let key = (reference.game_id.clone(), reference.id.clone());
        if state
            .rooms
            .get(&key)
            .is_some_and(|handle| handle.reference == reference)
        {
            state.rooms.remove(&key);
        }
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
struct AdmissionOptions<'a> {
    player: &'a str,
    generation: u64,
    deadline: Instant,
    output: Arc<Output>,
    request: &'a Request,
    resume: bool,
    identity: PlayerIdentity,
}
async fn admit(handle: &RoomHandle, admission: AdmissionOptions<'_>) -> Result<Value, Error> {
    let AdmissionOptions {
        player,
        generation,
        deadline,
        output,
        request,
        resume,
        identity,
    } = admission;
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
            identity,
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
    struct ObservedModule {
        callbacks: Arc<std::sync::atomic::AtomicU64>,
        panic_on_tick: bool,
    }
    impl GameModule for ObservedModule {
        fn create(&self, _: &Value, _: &GameContext) -> Result<Box<dyn Game>, crate::GameError> {
            Ok(Box::new(ObservedGame {
                callbacks: self.callbacks.clone(),
                panic_on_tick: self.panic_on_tick,
            }))
        }
    }
    struct ObservedGame {
        callbacks: Arc<std::sync::atomic::AtomicU64>,
        panic_on_tick: bool,
    }
    impl ObservedGame {
        fn called(&self) {
            self.callbacks.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl Game for ObservedGame {
        fn player_joined(&mut self, _: &str) {
            self.called();
        }
        fn player_left(&mut self, _: &str) {
            self.called();
        }
        fn player_connection_changed(&mut self, _: &str, _: bool) {
            self.called();
        }
        fn validate_input(&self, _: &str, _: &Value) -> Result<(), crate::GameError> {
            self.called();
            Ok(())
        }
        fn apply_input(&mut self, _: &str, _: &Value) {
            self.called();
        }
        fn validate_command(&self, _: &str, _: &Value) -> Result<(), crate::GameError> {
            self.called();
            Ok(())
        }
        fn apply_command(&mut self, _: &str, _: &Value) -> Result<Value, crate::GameError> {
            self.called();
            Ok(json!({"secret":"COMMAND_RESULT_SENTINEL"}))
        }
        fn tick(&mut self, _: Duration) {
            self.called();
            assert!(
                !self.panic_on_tick,
                "intentional observation panic regression"
            );
        }
        fn snapshot(&self) -> Value {
            self.called();
            json!({"secret":"PUBLIC_STATE_SENTINEL"})
        }
        fn private_snapshot(&self, _: &str) -> Option<Value> {
            self.called();
            Some(json!({"secret":"PRIVATE_STATE_SENTINEL"}))
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
    struct AccountVerifier;
    impl crate::AccountAuthenticator for AccountVerifier {
        fn authenticate<'a>(
            &'a self,
            _: &'a str,
            credential: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<crate::AccountGrant, crate::AuthError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                if credential == "bad" {
                    return Err(crate::AuthError::InvalidCredential);
                }
                Ok(crate::AccountGrant {
                    identity: crate::AccountIdentity {
                        issuer: "host".into(),
                        subject: credential.into(),
                        claims: json!({"role":"player"}),
                    },
                    valid_for: Duration::from_secs(60),
                })
            })
        }
    }
    struct GrantVerifier {
        grant: crate::AccountGrant,
        delay: Duration,
    }
    impl AccountAuthenticator for GrantVerifier {
        fn authenticate<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<crate::AccountGrant, AuthError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                Ok(self.grant.clone())
            })
        }
    }
    struct GatedVerifier {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl AccountAuthenticator for GatedVerifier {
        fn authenticate<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<crate::AccountGrant, AuthError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(grant())
            })
        }
    }
    fn grant() -> crate::AccountGrant {
        crate::AccountGrant {
            identity: crate::AccountIdentity {
                issuer: "issuer".into(),
                subject: "subject".into(),
                claims: json!({"secret":"grant-private"}),
            },
            valid_for: Duration::from_secs(60),
        }
    }
    fn account_runtime(config: Config, verifier: impl AccountAuthenticator) -> Campfire {
        let runtime = Campfire::with_authenticator(config, Arc::new(verifier)).unwrap();
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        runtime
    }
    #[tokio::test(start_paused = true)]
    async fn account_invalid_grants_are_bounded_and_debug_redacts_identity() {
        let valid = grant();
        let debug = format!("{:?}", PlayerIdentity::Account(valid.identity.clone()));
        assert!(!debug.contains("issuer"));
        assert!(!debug.contains("subject"));
        assert!(!debug.contains("grant-private"));
        let mut invalid = Vec::new();
        let mut zero = valid.clone();
        zero.valid_for = Duration::ZERO;
        invalid.push(zero);
        let mut huge = valid.clone();
        huge.valid_for = Duration::MAX;
        invalid.push(huge);
        for issuer in ["".into(), " ".into(), "x".repeat(129)] {
            let mut bad = valid.clone();
            bad.identity.issuer = issuer;
            invalid.push(bad);
        }
        for subject in ["".into(), " ".into(), "x".repeat(257)] {
            let mut bad = valid.clone();
            bad.identity.subject = subject;
            invalid.push(bad);
        }
        let mut claims = valid;
        claims.identity.claims = json!({"large":"x".repeat(1024)});
        invalid.push(claims);
        for grant in invalid {
            let runtime = account_runtime(
                Config {
                    max_frame_bytes: 512,
                    ..Config::default()
                },
                GrantVerifier {
                    grant,
                    delay: Duration::ZERO,
                },
            );
            assert_eq!(
                runtime
                    .inner
                    .issue_account("a", "credential", "source".into())
                    .await
                    .unwrap_err()
                    .code,
                "AUTH_UNAVAILABLE"
            );
            assert_eq!(runtime.metrics().active_sessions, 0);
            assert_eq!(runtime.inner.unauthenticated.available_permits(), 256);
        }
    }
    #[tokio::test(start_paused = true)]
    async fn account_grant_lifetime_starts_before_verification_and_is_capped_by_guest_ttl() {
        let mut short = grant();
        short.valid_for = Duration::from_secs(1);
        let runtime = account_runtime(
            Config::default(),
            GrantVerifier {
                grant: short,
                delay: Duration::from_secs(2),
            },
        );
        assert_eq!(
            runtime
                .inner
                .issue_account("a", "credential", "source".into())
                .await
                .unwrap_err()
                .code,
            "AUTH_UNAVAILABLE"
        );
        let runtime = account_runtime(
            Config {
                guest_ttl: Duration::from_secs(10),
                ..Config::default()
            },
            GrantVerifier {
                grant: grant(),
                delay: Duration::from_secs(2),
            },
        );
        let began = Instant::now();
        let issued = runtime
            .inner
            .issue_account("a", "credential", "source".into())
            .await
            .unwrap();
        let key = token_hash(issued["token"].as_str().unwrap());
        assert_eq!(
            runtime.inner.state.lock().await.sessions[&key].deadline,
            began + Duration::from_secs(10)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn account_verification_does_not_hold_state_lock_and_cancel_releases_shared_permit() {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let runtime = account_runtime(
            Config {
                max_unauthenticated_connections: 1,
                ..Config::default()
            },
            GatedVerifier {
                started: started.clone(),
                release,
            },
        );
        let inner = runtime.inner.clone();
        let pending = tokio::spawn(async move {
            inner
                .issue_account("a", "credential", "source".into())
                .await
        });
        started.notified().await;
        assert!(runtime.inner.state.try_lock().is_ok());
        assert!(
            runtime
                .inner
                .issue("a", "guest-source".into())
                .await
                .is_ok()
        );
        assert_eq!(
            runtime
                .inner
                .issue_account("a", "other", "another".into())
                .await
                .unwrap_err()
                .code,
            "SERVER_BUSY"
        );
        pending.abort();
        let _ = pending.await;
        assert_eq!(runtime.inner.unauthenticated.available_permits(), 1);
        assert_eq!(runtime.metrics().active_sessions, 1);
    }
    #[tokio::test(start_paused = true)]
    async fn account_timeout_and_shutdown_release_verification_capacity_without_session_mutation() {
        for shutdown in [false, true] {
            let started = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let runtime = account_runtime(
                Config {
                    max_unauthenticated_connections: 1,
                    ..Config::default()
                },
                GatedVerifier {
                    started: started.clone(),
                    release: release.clone(),
                },
            );
            let inner = runtime.inner.clone();
            let pending = tokio::spawn(async move {
                inner
                    .issue_account("a", "credential", "source".into())
                    .await
            });
            started.notified().await;
            if shutdown {
                runtime.shutdown().await;
                release.notify_one();
            } else {
                tokio::time::advance(runtime.inner.config.auth_timeout).await;
            }
            assert_eq!(
                pending.await.unwrap().unwrap_err().code,
                if shutdown {
                    "SERVER_BUSY"
                } else {
                    "AUTH_TIMEOUT"
                }
            );
            assert_eq!(runtime.inner.unauthenticated.available_permits(), 1);
            assert_eq!(runtime.metrics().active_sessions, 0);
        }
    }
    fn stub_membership(
        state: &mut State,
        key: TokenHash,
    ) -> (
        mpsc::Receiver<Control>,
        mpsc::Receiver<GameCommand>,
        mpsc::Sender<GameCommand>,
    ) {
        let reference = RoomRef {
            game_id: "a".into(),
            id: "stub".into(),
            instance: "epoch".into(),
        };
        let (control, controls) = mpsc::channel(8);
        let (inputs, _input_rx) = mpsc::channel(1);
        let (commands, command_rx) = mpsc::channel(1);
        state.sessions.get_mut(&key).unwrap().room = Some(reference.clone());
        state.rooms.insert(
            ("a".into(), "stub".into()),
            RoomHandle {
                reference,
                control,
                inputs,
                commands: commands.clone(),
                public: true,
                visible: true,
                capacity: 16,
            },
        );
        (controls, command_rx, commands)
    }
    #[tokio::test(start_paused = true)]
    async fn late_old_instance_close_keeps_replacement_room_and_decrements_only_old_actor() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let issued = runtime.inner.issue("a", "source".into()).await.unwrap();
        let key = token_hash(issued["token"].as_str().unwrap());
        let mut state = runtime.inner.state.lock().await;
        let (_controls, _commands, _sender) = stub_membership(&mut state, key);
        let replacement = state.rooms.values_mut().next().unwrap();
        let old = replacement.reference.clone();
        replacement.reference.instance = "replacement".into();
        let new = replacement.reference.clone();
        state.sessions.get_mut(&key).unwrap().room = Some(new.clone());
        runtime
            .inner
            .metrics
            .active_rooms
            .store(2, Ordering::Relaxed);
        drop(state);

        runtime.inner.closed(old, Ok("empty")).await;

        let state = runtime.inner.state.lock().await;
        assert_eq!(state.rooms.len(), 1, "old supervisor removed the new actor");
        assert_eq!(state.rooms.values().next().unwrap().reference, new);
        assert_eq!(state.sessions[&key].room, Some(new.clone()));
        assert_eq!(runtime.metrics().active_rooms, 1);
        drop(state);
        runtime.inner.closed(new, Ok("shutdown")).await;
        assert!(runtime.inner.state.lock().await.rooms.is_empty());
        assert_eq!(runtime.metrics().active_rooms, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn observer_reports_empty_registered_games_without_manager_or_module_locks() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        runtime
            .register_game(
                "empty",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        let observer = runtime.observer();
        {
            let _manager = runtime.inner.state.lock().await;
            let _modules = runtime.inner.modules.write().unwrap();
            let snapshot = observer.snapshot().unwrap();
            assert_eq!(snapshot.game_ids, ["a", "empty"]);
            assert!(snapshot.rooms.is_empty());
            assert!(!snapshot.stopping);
            assert_eq!(snapshot.uptime_ms, 0);
        }
        tokio::time::advance(Duration::from_millis(250)).await;
        assert_eq!(observer.snapshot().unwrap().uptime_ms, 250);
    }
    #[test]
    fn observer_does_not_keep_runtime_alive() {
        let runtime = Campfire::new(Config::default()).unwrap();
        let observer = runtime.observer();
        assert_eq!(Arc::strong_count(&runtime.inner), 1);
        assert!(observer.snapshot().is_some());
        assert_eq!(Arc::strong_count(&runtime.inner), 1);
        drop(runtime);
        assert!(observer.snapshot().is_none());
    }
    async fn create_observed_room(
        runtime: &Campfire,
    ) -> (TokenHash, u64, Arc<Output>, mpsc::Receiver<Value>, Value) {
        let guest = runtime.inner.issue("a", "creator".into()).await.unwrap();
        let (out, rx) = output(64);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let create = crate::protocol::parse(r#"{"v":1,"type":"room.create","request_id":"create","settings":{"capacity":3,"public":false,"private_setting":"SETTINGS_SENTINEL"}}"#).unwrap();
        let entered = runtime
            .inner
            .command(key, generation, "a", out.clone(), &create)
            .await
            .unwrap()
            .0;
        (key, generation, out, rx, entered)
    }
    #[tokio::test(start_paused = true)]
    async fn observer_tracks_actual_actor_seats_through_join_disconnect_resume_expiry_and_close() {
        let runtime = account_runtime(
            Config {
                reconnect_grace: Duration::from_secs(2),
                ..Config::default()
            },
            AccountVerifier,
        );
        let observer = runtime.observer();
        let (_creator, _, _out, _rx, entered) = create_observed_room(&runtime).await;
        let initial = observer.snapshot().unwrap();
        let room = &initial.rooms[0];
        assert_eq!(
            (&room.game_id, &room.room_id, &room.room_instance_id),
            (
                &"a".to_owned(),
                &entered["room_id"].as_str().unwrap().to_owned(),
                &entered["room_instance_id"].as_str().unwrap().to_owned()
            )
        );
        assert!(!room.public);
        assert_eq!(
            (room.capacity, room.connected_members, room.reserved_members),
            (3, 1, 0)
        );
        let guest = runtime.inner.issue("a", "peer".into()).await.unwrap();
        let token = guest["token"].as_str().unwrap();
        let (out, _rx) = output(64);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", token, "auth", out.clone())
            .await
            .unwrap();
        let join = crate::protocol::parse(&json!({"v":1,"type":"room.join","request_id":"join","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"]}).to_string()).unwrap();
        runtime
            .inner
            .command(key, generation, "a", out, &join)
            .await
            .unwrap();
        assert_eq!(observer.snapshot().unwrap().rooms[0].connected_members, 2);
        runtime.inner.disconnect(key, generation).await;
        let reserved = observer.snapshot().unwrap().rooms.remove(0);
        assert_eq!(
            (reserved.connected_members, reserved.reserved_members),
            (1, 1)
        );
        let (out, _rx) = output(64);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", token, "resume-auth", out.clone())
            .await
            .unwrap();
        let resume = crate::protocol::parse(&json!({"v":1,"type":"resume","request_id":"resume","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"]}).to_string()).unwrap();
        runtime
            .inner
            .command(key, generation, "a", out, &resume)
            .await
            .unwrap();
        let resumed = observer.snapshot().unwrap().rooms.remove(0);
        assert_eq!(
            (resumed.connected_members, resumed.reserved_members),
            (2, 0)
        );
        runtime.inner.disconnect(key, generation).await;
        tokio::time::advance(Duration::from_secs(2)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        let expired = observer.snapshot().unwrap().rooms.remove(0);
        assert_eq!(
            (expired.connected_members, expired.reserved_members),
            (1, 0)
        );
        assert!(expired.server_tick > 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert!(observer.snapshot().unwrap().rooms[0].snapshot_seq > 0);
        runtime.shutdown().await;
        let stopped = observer.snapshot().unwrap();
        assert!(stopped.stopping);
        assert!(stopped.rooms.is_empty());
        assert_eq!(stopped.metrics.active_rooms, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn observer_only_serializes_allowlisted_summary_without_game_callbacks_or_mutations() {
        let callbacks = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let runtime = Campfire::with_authenticator(
            Config::default(),
            Arc::new(GrantVerifier {
                grant: grant(),
                delay: Duration::ZERO,
            }),
        )
        .unwrap();
        runtime
            .register_game(
                "a",
                ObservedModule {
                    callbacks: callbacks.clone(),
                    panic_on_tick: false,
                },
            )
            .unwrap();
        let issued = runtime
            .inner
            .issue_account("a", "CREDENTIAL_SENTINEL", "source".into())
            .await
            .unwrap();
        let (out, _rx) = output(64);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", issued["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let create = crate::protocol::parse(r#"{"v":1,"type":"room.create","request_id":"create","settings":{"public":false,"secret":"SETTINGS_SENTINEL"}}"#).unwrap();
        let entered = runtime
            .inner
            .command(key, generation, "a", out.clone(), &create)
            .await
            .unwrap()
            .0;
        let command = crate::protocol::parse(&json!({"v":1,"type":"game.command","request_id":"command","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"],"command_seq":1,"payload":{"secret":"COMMAND_PAYLOAD_SENTINEL"}}).to_string()).unwrap();
        runtime
            .inner
            .command(key, generation, "a", out.clone(), &command)
            .await
            .unwrap();
        let input = crate::protocol::parse(&json!({"v":1,"type":"game.input","request_id":"input","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"],"input_seq":1,"payload":{"secret":"INPUT_PAYLOAD_SENTINEL"}}).to_string()).unwrap();
        runtime
            .inner
            .command(key, generation, "a", out, &input)
            .await
            .unwrap();
        let observer = runtime.observer();
        let before = observer.snapshot().unwrap();
        let calls = callbacks.load(Ordering::Relaxed);
        let _manager = runtime.inner.state.lock().await;
        let _modules = runtime.inner.modules.write().unwrap();
        for _ in 0..32 {
            let snapshot = observer.snapshot().unwrap();
            assert_eq!(snapshot.metrics.ticks, before.metrics.ticks);
            assert_eq!(snapshot.metrics.snapshots, before.metrics.snapshots);
            assert_eq!(snapshot.rooms[0].server_tick, before.rooms[0].server_tick);
            assert_eq!(snapshot.rooms[0].snapshot_seq, before.rooms[0].snapshot_seq);
            let serialized = serde_json::to_string(&snapshot).unwrap();
            for secret in [
                "SETTINGS_SENTINEL",
                "CREDENTIAL_SENTINEL",
                "INPUT_PAYLOAD_SENTINEL",
                "COMMAND_PAYLOAD_SENTINEL",
                "PUBLIC_STATE_SENTINEL",
                "PRIVATE_STATE_SENTINEL",
                "COMMAND_RESULT_SENTINEL",
                "issuer",
                "subject",
                "grant-private",
                issued["token"].as_str().unwrap(),
                issued["player_id"].as_str().unwrap(),
            ] {
                assert!(!serialized.contains(secret), "observation leaked {secret}");
            }
            let value = serde_json::to_value(&snapshot.rooms[0]).unwrap();
            let mut keys: Vec<_> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    "capacity",
                    "connected_members",
                    "game_id",
                    "public",
                    "reserved_members",
                    "room_id",
                    "room_instance_id",
                    "server_tick",
                    "snapshot_seq",
                    "summary_age_ms"
                ]
            );
        }
        assert_eq!(callbacks.load(Ordering::Relaxed), calls);
        drop(_modules);
        drop(_manager);
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn observer_omits_panicked_actor_and_supervision_refunds_active_room_once() {
        let runtime = Campfire::new(Config {
            tick_hz: 1,
            snapshot_hz: 1,
            ..Config::default()
        })
        .unwrap();
        runtime
            .register_game(
                "a",
                ObservedModule {
                    callbacks: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    panic_on_tick: true,
                },
            )
            .unwrap();
        let observer = runtime.observer();
        let (_, _, _out, _rx, _) = create_observed_room(&runtime).await;
        assert_eq!(observer.snapshot().unwrap().rooms.len(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let snapshot = observer.snapshot().unwrap();
        assert!(snapshot.rooms.is_empty());
        assert_eq!(snapshot.metrics.active_rooms, 0);
        assert_eq!(snapshot.metrics.room_panics, 1);
        tokio::time::advance(Duration::from_secs(3)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(observer.snapshot().unwrap().metrics.active_rooms, 0);
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn account_cancelled_rotation_keeps_old_token_identity_and_deadline_before_actor_commit()
    {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let issued = runtime
            .inner
            .issue_account("a", "alice", "source".into())
            .await
            .unwrap();
        let token = issued["token"].as_str().unwrap().to_owned();
        let (out, _rx) = output(16);
        let (key, _, deadline, _) = runtime
            .inner
            .authenticate("a", &token, "auth", out)
            .await
            .unwrap();
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let identity = runtime.inner.state.lock().await.sessions[&key]
            .identity
            .clone();
        let inner = runtime.inner.clone();
        let pending =
            tokio::spawn(async move { inner.issue_account("a", "alice", "source".into()).await });
        let Control::Replace { reply, .. } = controls.recv().await.unwrap() else {
            panic!("expected staged replacement")
        };
        pending.abort();
        let _ = pending.await;
        drop(reply);
        let state = runtime.inner.state.lock().await;
        assert_eq!(state.sessions[&key].identity, identity);
        assert_eq!(state.sessions[&key].deadline, deadline);
        assert_eq!(state.sessions.len(), 1);
        drop(state);
        assert!(controls.try_recv().is_err());
        let inner = runtime.inner.clone();
        let (out, _rx) = output(16);
        let authentication =
            tokio::spawn(async move { inner.authenticate("a", &token, "retry", out).await });
        let Control::Replace {
            reply, rotation, ..
        } = controls.recv().await.unwrap()
        else {
            panic!("old token must authenticate")
        };
        assert!(rotation.is_none());
        reply
            .send(Some(Instant::now() + Duration::from_secs(30)))
            .unwrap();
        assert_eq!(authentication.await.unwrap().unwrap().0, key);
    }
    #[tokio::test(start_paused = true)]
    async fn bounded_command_queue_full_is_non_consuming_and_retry_uses_same_sequence() {
        let runtime = Campfire::new(Config::default()).unwrap();
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        let issued = runtime.inner.issue("a", "source".into()).await.unwrap();
        let (out, _rx) = output(16);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", issued["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let (_controls, mut commands, sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let (tx, _rx) = oneshot::channel();
        sender
            .try_send(GameCommand {
                player: "filler".into(),
                generation: 0,
                sequence: 99,
                payload: json!({}),
                request_id: "filler".into(),
                reply: tx,
            })
            .unwrap_or_else(|_| panic!("fill queue"));
        let request=crate::protocol::parse(r#"{"v":1,"type":"game.command","request_id":"command","room_id":"stub","room_instance_id":"epoch","command_seq":1,"payload":{"buy":"key"}}"#).unwrap();
        assert_eq!(
            runtime
                .inner
                .command(key, generation, "a", out.clone(), &request)
                .await
                .unwrap_err()
                .code,
            "SERVER_BUSY"
        );
        assert_eq!(commands.recv().await.unwrap().sequence, 99);
        let inner = runtime.inner.clone();
        let retry =
            tokio::spawn(async move { inner.command(key, generation, "a", out, &request).await });
        let command = commands.recv().await.unwrap();
        assert_eq!(command.sequence, 1);
        command
            .reply
            .send(Ok(
                json!({"type":"game.command.result","command_seq":1,"ok":true}),
            ))
            .unwrap();
        assert_eq!(retry.await.unwrap().unwrap().0["command_seq"], 1);
        assert_eq!(runtime.metrics().shared_queue_overloads, 1);
        assert_eq!(runtime.metrics().abusive_inputs, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn account_grant_expiring_while_waiting_for_commit_does_not_create_session() {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let runtime = account_runtime(
            Config::default(),
            GatedVerifier {
                started: started.clone(),
                release: release.clone(),
            },
        );
        let inner = runtime.inner.clone();
        let pending = tokio::spawn(async move {
            inner
                .issue_account("a", "credential", "source".into())
                .await
        });
        started.notified().await;
        let state = runtime.inner.state.lock().await;
        release.notify_one();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        drop(state);
        assert_eq!(pending.await.unwrap().unwrap_err().code, "AUTH_UNAVAILABLE");
        assert_eq!(runtime.metrics().active_sessions, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn account_grant_expiring_during_expired_seat_cleanup_does_not_commit() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let issued = runtime
            .inner
            .issue_account("a", "alice", "source".into())
            .await
            .unwrap();
        let key = token_hash(issued["token"].as_str().unwrap());
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        tokio::time::advance(Duration::from_secs(60)).await;
        let inner = runtime.inner.clone();
        let pending =
            tokio::spawn(async move { inner.issue_account("a", "alice", "source".into()).await });
        let Control::Leave { reply, .. } = controls.recv().await.unwrap() else {
            panic!("expired seat must leave")
        };
        tokio::time::advance(Duration::from_secs(60)).await;
        reply.send(()).unwrap();
        assert_eq!(pending.await.unwrap().unwrap_err().code, "AUTH_UNAVAILABLE");
        assert!(runtime.inner.state.lock().await.sessions.is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn accepted_rotation_preserves_real_actor_ledger_when_old_ttl_expires_before_next_poll() {
        let runtime = account_runtime(
            Config {
                guest_ttl: Duration::from_secs(1),
                ..Config::default()
            },
            AccountVerifier,
        );
        let issued = runtime
            .inner
            .issue_account("a", "alice", "source".into())
            .await
            .unwrap();
        let (out, _rx) = output(32);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", issued["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let create = crate::protocol::parse(
            r#"{"v":1,"type":"room.create","request_id":"create","settings":{}}"#,
        )
        .unwrap();
        let entered = runtime
            .inner
            .command(key, generation, "a", out.clone(), &create)
            .await
            .unwrap()
            .0;
        let command=crate::protocol::parse(&json!({"v":1,"type":"game.command","request_id":"command","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"],"command_seq":1,"payload":{}}).to_string()).unwrap();
        runtime
            .inner
            .command(key, generation, "a", out.clone(), &command)
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(500)).await;
        let rotated = runtime
            .inner
            .issue_account("a", "alice", "source".into())
            .await
            .unwrap();
        assert_eq!(issued["player_id"], rotated["player_id"]);
        // The issuer swapped the map key, but the actor has not polled its commit control yet.
        tokio::time::advance(Duration::from_millis(500)).await;
        let (out, _rx) = output(32);
        let (key, generation, _, auth) = runtime
            .inner
            .authenticate("a", rotated["token"].as_str().unwrap(), "new", out.clone())
            .await
            .unwrap();
        assert_eq!(
            auth["can_resume"], true,
            "accepted grant must replace the old actor TTL before expiry"
        );
        let resume=crate::protocol::parse(&json!({"v":1,"type":"resume","request_id":"resume","room_id":entered["room_id"],"room_instance_id":entered["room_instance_id"]}).to_string()).unwrap();
        let full = runtime
            .inner
            .command(key, generation, "a", out, &resume)
            .await
            .unwrap()
            .0;
        assert_eq!(full["last_processed_command_seq"], 1);
        assert_eq!(full["command_results"].as_array().unwrap().len(), 1);
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn account_valid_grant_is_scoped_and_rotation_replaces_token_without_replacing_player() {
        let runtime =
            Campfire::with_authenticator(Config::default(), Arc::new(AccountVerifier)).unwrap();
        for game in ["a", "b"] {
            runtime
                .register_game(
                    game,
                    TestModule {
                        close_on_create: None,
                    },
                )
                .unwrap();
        }
        let first = runtime
            .inner
            .issue_account("a", "alice", "source".into())
            .await
            .unwrap();
        let second = runtime
            .inner
            .issue_account("a", "alice", "source".into())
            .await
            .unwrap();
        let other_game = runtime
            .inner
            .issue_account("b", "alice", "source".into())
            .await
            .unwrap();
        assert_eq!(first["player_id"], second["player_id"]);
        assert_ne!(first["token"], second["token"]);
        assert_ne!(first["player_id"], other_game["player_id"]);
        let (out, _rx) = output(8);
        assert_eq!(
            runtime
                .inner
                .authenticate("a", first["token"].as_str().unwrap(), "old", out.clone())
                .await
                .unwrap_err()
                .code,
            "UNAUTHENTICATED"
        );
        let (_, _, _, authenticated) = runtime
            .inner
            .authenticate("a", second["token"].as_str().unwrap(), "new", out)
            .await
            .unwrap();
        assert_eq!(authenticated["player_id"], second["player_id"]);
        assert_eq!(runtime.metrics().active_sessions, 2);
    }
    #[tokio::test(start_paused = true)]
    async fn account_disabled_and_invalid_credentials_do_not_create_sessions() {
        let runtime = Campfire::new(Config::default()).unwrap();
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .issue_account("a", "alice", "source".into())
                .await
                .unwrap_err()
                .code,
            "ACCOUNT_AUTH_DISABLED"
        );
        assert!(runtime.inner.issue("a", "source".into()).await.is_ok());
        let runtime =
            Campfire::with_authenticator(Config::default(), Arc::new(AccountVerifier)).unwrap();
        runtime
            .register_game(
                "a",
                TestModule {
                    close_on_create: None,
                },
            )
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .issue_account("a", "bad", "source".into())
                .await
                .unwrap_err()
                .code,
            "UNAUTHENTICATED"
        );
        assert_eq!(runtime.metrics().active_sessions, 0);
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
        assert!(runtime.observer().snapshot().unwrap().rooms.is_empty());
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
