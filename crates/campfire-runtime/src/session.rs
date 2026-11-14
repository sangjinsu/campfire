use crate::{
    AccountAuthenticator, AuthError, Config, ConfigError, ConnectionLimits, GameContext,
    GameModule, PlayerIdentity, RuntimeOptions,
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
        Arc, RwLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, OwnedMutexGuard, Semaphore, mpsc, oneshot, watch},
    time::Instant,
};

pub(crate) type TokenHash = [u8; 32];
type CommandResult = Result<(Value, bool), Error>;
type AuthenticationResult = Result<(TokenHash, u64, Instant, Value), Error>;
type IdentityGates = HashMap<(String, String, String), Weak<Mutex<()>>>;
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
    /// Set for guest tokens that have never authenticated; cleared by the first successful authentication.
    pub unclaimed_until: Option<Instant>,
    pub identity: PlayerIdentity,
    pub lifecycle: Arc<Mutex<()>>,
}
impl Session {
    fn expired(&self, now: Instant) -> bool {
        self.deadline <= now || self.unclaimed_until.is_some_and(|until| until <= now)
    }
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
    pub connection_limits: ConnectionLimits,
    pub modules: RwLock<HashMap<String, Arc<dyn GameModule>>>,
    pub state: Mutex<State>,
    pub metrics: Arc<Metrics>,
    pub connections: Arc<Semaphore>,
    pub unauthenticated: Arc<Semaphore>,
    pub unauthenticated_sources: Arc<crate::source::SourceSlots>,
    pub shutdown: watch::Sender<bool>,
    pub authenticator: Option<Arc<dyn AccountAuthenticator>>,
    pub observations: ObservationDirectory,
    pub operations: Arc<Semaphore>,
    identity_gates: std::sync::Mutex<IdentityGates>,
    started: AtomicBool,
    pub clock_origin: Instant,
    unix_origin: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShutdownReport {
    pub completed: bool,
    pub remaining_rooms: u64,
    pub remaining_connections: u64,
    pub remaining_operations: u64,
}
#[derive(Clone)]
pub struct Campfire {
    pub(crate) inner: Arc<Inner>,
}
impl Campfire {
    pub fn new(config: Config) -> Result<Self, ConfigError> {
        Self::new_with_options(config, RuntimeOptions::default())
    }
    pub fn with_authenticator(
        config: Config,
        authenticator: Arc<dyn AccountAuthenticator>,
    ) -> Result<Self, ConfigError> {
        Self::with_authenticator_and_options(config, authenticator, RuntimeOptions::default())
    }
    pub fn new_with_options(config: Config, options: RuntimeOptions) -> Result<Self, ConfigError> {
        Self::build(config, None, options)
    }
    pub fn with_authenticator_and_options(
        config: Config,
        authenticator: Arc<dyn AccountAuthenticator>,
        options: RuntimeOptions,
    ) -> Result<Self, ConfigError> {
        Self::build(config, Some(authenticator), options)
    }
    fn build(
        config: Config,
        authenticator: Option<Arc<dyn AccountAuthenticator>>,
        options: RuntimeOptions,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        options.connection_limits.validate()?;
        let (shutdown, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(Inner {
                operations: Arc::new(Semaphore::new(config.max_sessions)),
                identity_gates: std::sync::Mutex::new(HashMap::new()),
                connections: Arc::new(Semaphore::new(config.max_connections)),
                unauthenticated: Arc::new(Semaphore::new(config.max_unauthenticated_connections)),
                unauthenticated_sources: Arc::default(),
                state: Mutex::new(State {
                    sessions: HashMap::new(),
                    rooms: BTreeMap::new(),
                    rate_keys: HashMap::new(),
                    global_rate: Bucket::new(config.global_guest_burst),
                }),
                config,
                connection_limits: options.connection_limits,
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
    /// Waits up to five seconds for room, transport, and lifecycle tasks to finish.
    pub async fn shutdown(&self) {
        let report = self.shutdown_with_timeout(Duration::from_secs(5)).await;
        if !report.completed {
            tracing::warn!(
                remaining_rooms = report.remaining_rooms,
                remaining_connections = report.remaining_connections,
                remaining_operations = report.remaining_operations,
                "runtime shutdown deadline reached; tasks remain active"
            );
        }
    }
    /// Bounds the asynchronous wait using one deadline. Running synchronous callbacks
    /// cannot be forcibly interrupted and can delay timers on their runtime thread.
    pub async fn shutdown_with_timeout(&self, timeout: Duration) -> ShutdownReport {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        self.inner.shutdown.send_replace(true);
        let work = async {
            let mut state = self.inner.state.lock().await;
            for session in state.sessions.values() {
                if let Some(output) = &session.output {
                    output.close();
                }
            }
            for handle in state.rooms.values() {
                handle.stop.send_replace(true);
            }
            state.sessions.clear();
            state.rate_keys.clear();
            self.inner
                .metrics
                .active_sessions
                .store(0, Ordering::Relaxed);
            self.inner.metrics.rate_keys.store(0, Ordering::Relaxed);
            drop(state);
            loop {
                let report = self.shutdown_report();
                if report.completed {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let initialized_and_drained = tokio::time::timeout_at(deadline, work).await.is_ok();
        let mut report = self.shutdown_report();
        report.completed &= initialized_and_drained;
        report
    }
    fn shutdown_report(&self) -> ShutdownReport {
        let remaining_rooms = self.inner.metrics.active_rooms.load(Ordering::Acquire);
        let remaining_connections =
            (self.inner.config.max_connections - self.inner.connections.available_permits()) as u64;
        let remaining_operations =
            (self.inner.config.max_sessions - self.inner.operations.available_permits()) as u64;
        ShutdownReport {
            completed: remaining_rooms == 0
                && remaining_connections == 0
                && remaining_operations == 0,
            remaining_rooms,
            remaining_connections,
            remaining_operations,
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
        self: &Arc<Self>,
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
            .clone()
            .try_acquire_owned()
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
        let identity_key = (
            game.to_owned(),
            grant.identity.issuer.clone(),
            grant.identity.subject.clone(),
        );
        let identity_gate = {
            let mut gates = self
                .identity_gates
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            gates.retain(|_, gate| gate.strong_count() > 0);
            let gate = gates
                .get(&identity_key)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| Arc::new(Mutex::new(())));
            gates.insert(identity_key, Arc::downgrade(&gate));
            gate
        };
        let identity_guard = self.acquire_gate(identity_gate).await?;
        let previous = {
            let state = self.state.lock().await;
            state
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
                })
        };
        let lifecycle = if let Some(key) = previous {
            self.state
                .lock()
                .await
                .sessions
                .get(&key)
                .map(|s| s.lifecycle.clone())
                .unwrap_or_else(|| Arc::new(Mutex::new(())))
        } else {
            Arc::new(Mutex::new(()))
        };
        let session_guard = self.acquire_gate(lifecycle.clone()).await?;
        let permit = self
            .operations
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let inner = self.clone();
        let game = game.to_owned();
        let (mut tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (_identity, _session, _operation, _issuance) =
                (identity_guard, session_guard, permit, _permit);
            if tx.is_closed() {
                return;
            }
            let result = inner
                .rotate_account(
                    &game,
                    PlayerIdentity::Account(grant.identity),
                    deadline,
                    previous,
                    lifecycle,
                    &mut tx,
                )
                .await;
            let _ = tx.send(result);
        });
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(Error::busy());
        }
        tokio::select! {
            result = rx => result.map_err(|_| Error::busy())?,
            _ = shutdown.changed() => Err(Error::busy()),
        }
    }
    async fn rotate_account(
        &self,
        game: &str,
        identity: PlayerIdentity,
        deadline: Instant,
        previous: Option<TokenHash>,
        lifecycle: Arc<Mutex<()>>,
        reply: &mut oneshot::Sender<Result<Value, Error>>,
    ) -> Result<Value, Error> {
        let token = random_id("")?;
        let new_key = token_hash(&token);
        let mut state = self.state.lock().await;
        let previous = previous.filter(|key| {
            state
                .sessions
                .get(key)
                .is_some_and(|s| Arc::ptr_eq(&s.lifecycle, &lifecycle))
        });
        if let Some(key) = previous.filter(|key| state.sessions[key].deadline <= Instant::now()) {
            let expired = state
                .sessions
                .remove(&key)
                .expect("located expired session");
            if let Some(output) = expired.output {
                output.close();
            }
            let handle = expired.room.as_ref().and_then(|r| room_handle(&state, r));
            self.metrics
                .active_sessions
                .store(state.sessions.len() as u64, Ordering::Relaxed);
            drop(state);
            if let Some(handle) = handle {
                leave_handle(&handle, &expired.id, expired.generation).await;
            }
            state = self.state.lock().await;
        }
        if *self.shutdown.borrow() || reply.is_closed() {
            return Err(Error::busy());
        }
        if deadline <= Instant::now() {
            return Err(Error::new(
                "AUTH_UNAVAILABLE",
                "account authentication is unavailable",
            ));
        }
        let previous = previous.filter(|key| state.sessions.contains_key(key));
        let session =
            if let Some(key) = previous {
                let old = state.sessions.get_mut(&key).expect("located session");
                let generation = old.generation.checked_add(1).ok_or_else(Error::busy)?;
                // The actor may advance even when the rotation is later aborted.
                old.generation = generation;
                let player = old.id.clone();
                let mut reference = old.room.clone();
                let handle = reference.as_ref().and_then(|r| room_handle(&state, r));
                let rotation = RotationGuard(room::Rotation::new(deadline, identity.clone()));
                drop(state);
                let mut reservation = None;
                if let Some(handle) = &handle {
                    let (tx, rx) = oneshot::channel();
                    if enqueue(
                        handle,
                        Control::Replace {
                            player: player.clone(),
                            generation,
                            rotation: Some(rotation.0.clone()),
                            reply: tx,
                        },
                        Some(&mut *reply),
                        None,
                    )
                    .await
                    .is_ok()
                    {
                        reservation = rx.await.ok().flatten();
                    }
                }
                state = self.state.lock().await;
                let live = reference
                    .as_ref()
                    .is_some_and(|r| room_handle(&state, r).is_some());
                reservation = reservation.filter(|_| live);
                if reservation.is_none() {
                    reference = None;
                }
                let Some(old) = state.sessions.get_mut(&key).filter(|s| {
                    s.generation == generation && Arc::ptr_eq(&s.lifecycle, &lifecycle)
                }) else {
                    drop(state);
                    if let Some(handle) = handle {
                        leave_handle(&handle, &player, generation).await;
                    }
                    return Err(Error::busy());
                };
                // Aborted prepares keep the old token and an accurate generation/reservation.
                if let Some(output) = old.output.take() {
                    output.close();
                }
                if reply.is_closed() {
                    old.reservation = rotation
                        .0
                        .previous_reservation()
                        .or(old.reservation)
                        .or(reservation.map(|until| until.min(old.deadline)))
                        .filter(|_| old.room.is_some());
                    return Err(Error::busy());
                }
                old.room = reference.clone();
                old.reservation = rotation
                    .0
                    .previous_reservation()
                    .or(reservation.map(|until| until.min(old.deadline)))
                    .filter(|_| old.room.is_some());
                if *self.shutdown.borrow() {
                    return Err(Error::busy());
                }
                if deadline <= Instant::now() {
                    return Err(Error::new(
                        "AUTH_UNAVAILABLE",
                        "account authentication is unavailable",
                    ));
                }
                if reference.is_some() && !rotation.0.commit() {
                    return Err(Error::busy());
                }
                // No await separates the actor decision and token-map replacement.
                state.sessions.remove(&key);
                Session {
                    id: player,
                    game_id: game.into(),
                    deadline,
                    expires_at: self.deadline_millis(deadline),
                    generation,
                    output: None,
                    room: reference,
                    reservation,
                    unclaimed_until: None,
                    identity,
                    lifecycle,
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
                    unclaimed_until: None,
                    identity,
                    lifecycle,
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
        if *self.shutdown.borrow() {
            return Err(Error::busy());
        }
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
                unclaimed_until: Some(deadline.min(now + self.config.unclaimed_guest_ttl)),
                identity: PlayerIdentity::Guest,
                lifecycle: Arc::new(Mutex::new(())),
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
    async fn acquire_gate(&self, gate: Arc<Mutex<()>>) -> Result<OwnedMutexGuard<()>, Error> {
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(Error::busy());
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => Err(Error::busy()),
            guard = gate.lock_owned() => Ok(guard),
        }
    }
    async fn session_gate(&self, key: TokenHash) -> Result<OwnedMutexGuard<()>, Error> {
        let gate = self
            .state
            .lock()
            .await
            .sessions
            .get(&key)
            .map(|session| session.lifecycle.clone())
            .ok_or_else(|| Error::new("UNAUTHENTICATED", "session is no longer active"))?;
        let guard = self.acquire_gate(gate.clone()).await?;
        let state = self.state.lock().await;
        if *self.shutdown.borrow()
            || !state
                .sessions
                .get(&key)
                .is_some_and(|session| Arc::ptr_eq(&gate, &session.lifecycle))
        {
            return Err(Error::new("UNAUTHENTICATED", "session is no longer active"));
        }
        Ok(guard)
    }
    pub async fn authenticate(
        self: &Arc<Self>,
        game: &str,
        token: &str,
        request: &str,
        output: Arc<Output>,
    ) -> Result<(TokenHash, u64, Instant, Value), Error> {
        if token.len() > 512 {
            return Err(Error::new("UNAUTHENTICATED", "invalid or expired token"));
        }
        let key = token_hash(token);
        let gate = self.session_gate(key).await?;
        let permit = self
            .operations
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let (mut tx, rx) = oneshot::channel();
        let inner = self.clone();
        let game = game.to_owned();
        let request = request.to_owned();
        tokio::spawn(async move {
            let (_gate, _permit) = (gate, permit);
            if tx.is_closed() {
                return;
            }
            let result = inner
                .authenticate_owned(key, &game, &request, output.clone(), &mut tx)
                .await;
            let authenticated_generation = result.as_ref().ok().map(|value| value.1);
            if tx.send(result).is_err() {
                output.close();
                if let Some(generation) = authenticated_generation {
                    inner.disconnect_owned(key, generation).await;
                }
            }
        });
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(Error::busy());
        }
        tokio::select! {
            result = rx => result.map_err(|_| Error::busy())?,
            _ = shutdown.changed() => Err(Error::busy()),
        }
    }
    async fn authenticate_owned(
        &self,
        key: TokenHash,
        game: &str,
        request: &str,
        output: Arc<Output>,
        caller: &mut oneshot::Sender<AuthenticationResult>,
    ) -> Result<(TokenHash, u64, Instant, Value), Error> {
        let (generation, player, reference, handle) = {
            let mut state = self.state.lock().await;
            let session = state
                .sessions
                .get_mut(&key)
                .ok_or_else(|| Error::new("UNAUTHENTICATED", "invalid or expired token"))?;
            if session.expired(Instant::now()) {
                return Err(Error::new("UNAUTHENTICATED", "invalid or expired token"));
            }
            if session.game_id != game {
                return Err(Error::new("GAME_MISMATCH", "token belongs to another game"));
            }
            if caller.is_closed() || !output.is_alive() || *self.shutdown.borrow() {
                return Err(Error::new(
                    "CANCELLED",
                    "connection closed during authentication",
                ));
            }
            session.generation = session.generation.checked_add(1).ok_or_else(Error::busy)?;
            if let Some(previous) = session.output.take() {
                previous.close();
            }
            let (generation, player, reference) =
                (session.generation, session.id.clone(), session.room.clone());
            let handle = reference.as_ref().and_then(|r| room_handle(&state, r));
            (generation, player, reference, handle)
        };
        let reservation = if let Some(handle) = &handle {
            let (tx, rx) = oneshot::channel();
            enqueue(
                handle,
                Control::Replace {
                    player: player.clone(),
                    generation,
                    rotation: None,
                    reply: tx,
                },
                Some(&mut *caller),
                Some(&output),
            )
            .await?;
            rx.await.ok().flatten()
        } else {
            None
        };
        let mut state = self.state.lock().await;
        let valid = !*self.shutdown.borrow()
            && state
                .sessions
                .get(&key)
                .is_some_and(|s| s.generation == generation && !s.expired(Instant::now()));
        if !valid {
            drop(state);
            if let Some(handle) = handle {
                leave_handle(&handle, &player, generation).await;
            }
            return Err(Error::new("UNAUTHENTICATED", "session is no longer active"));
        }
        let live_reference = reference
            .as_ref()
            .is_some_and(|r| room_handle(&state, r).is_some());
        let session = state.sessions.get_mut(&key).expect("validated session");
        if session.room == reference {
            session.reservation = reservation.filter(|_| live_reference);
            if session.reservation.is_none() {
                session.room = None;
            }
        }
        if !output.is_alive() {
            return Err(Error::new(
                "CANCELLED",
                "connection closed during authentication",
            ));
        }
        session.output = Some(output);
        session.unclaimed_until = None;
        let response = json!({"v":1,"type":"session.authenticated","request_id":request,"guest_id":session.id,"player_id":session.id,
            "expires_at":session.expires_at,"game_id":game,"room_id":session.room.as_ref().map(|r|&r.id),
            "room_instance_id":session.room.as_ref().map(|r|&r.instance),"can_resume":session.reservation.is_some_and(|d|d>Instant::now()),
            "reservation_expires_at":session.reservation.map(|deadline|self.deadline_millis(deadline))});
        Ok((key, generation, session.deadline, response))
    }
    pub async fn disconnect(self: &Arc<Self>, key: TokenHash, generation: u64) {
        let Ok(gate) = self.session_gate(key).await else {
            return;
        };
        let Ok(permit) = self.operations.clone().try_acquire_owned() else {
            return;
        };
        let inner = self.clone();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (_gate, _permit) = (gate, permit);
            inner.disconnect_owned(key, generation).await;
            let _ = tx.send(());
        });
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return;
        }
        tokio::select! { _ = rx => {}, _ = shutdown.changed() => {} }
    }
    async fn disconnect_owned(&self, key: TokenHash, generation: u64) {
        let (player, reference, handle) = {
            let mut state = self.state.lock().await;
            let Some(session) = state
                .sessions
                .get_mut(&key)
                .filter(|s| s.generation == generation)
            else {
                return;
            };
            if let Some(output) = session.output.take() {
                output.close();
            }
            let (player, reference) = (session.id.clone(), session.room.clone());
            let handle = reference.as_ref().and_then(|r| room_handle(&state, r));
            (player, reference, handle)
        };
        let mut reservation = None;
        if let Some(handle) = &handle {
            let (tx, rx) = oneshot::channel();
            if handle
                .control
                .send(Control::Disconnect {
                    player,
                    generation,
                    reply: tx,
                })
                .await
                .is_ok()
            {
                reservation = rx.await.ok().flatten();
            }
        }
        let mut state = self.state.lock().await;
        let live = reference
            .as_ref()
            .is_some_and(|r| room_handle(&state, r).is_some());
        if let Some(session) = state
            .sessions
            .get_mut(&key)
            .filter(|s| s.generation == generation && s.room == reference)
        {
            if live && reservation.is_some() {
                session.reservation = reservation;
            } else {
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
        if matches!(
            request.command,
            Command::Input { .. }
                | Command::Execute { .. }
                | Command::List { .. }
                | Command::Authenticate { .. }
        ) {
            return self
                .command_owned(key, generation, game, output, request, None)
                .await;
        }
        let gate = self.session_gate(key).await?;
        let permit = self
            .operations
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let (mut tx, rx) = oneshot::channel();
        let inner = self.clone();
        let game = game.to_owned();
        let request = request.clone();
        tokio::spawn(async move {
            let (_gate, _permit) = (gate, permit);
            if tx.is_closed() || !output.is_alive() {
                return;
            }
            let result = inner
                .command_owned(
                    key,
                    generation,
                    &game,
                    output.clone(),
                    &request,
                    Some(&mut tx),
                )
                .await;
            if tx.send(result).is_err() || !output.is_alive() {
                output.close();
                inner.disconnect_owned(key, generation).await;
            }
        });
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(Error::busy());
        }
        tokio::select! {
            result = rx => result.map_err(|_| Error::busy())?,
            _ = shutdown.changed() => Err(Error::busy()),
        }
    }
    async fn command_owned(
        self: &Arc<Self>,
        key: TokenHash,
        generation: u64,
        game: &str,
        output: Arc<Output>,
        request: &Request,
        caller: Option<&mut oneshot::Sender<CommandResult>>,
    ) -> Result<(Value, bool), Error> {
        if request.game_id.as_ref().is_some_and(|id| id != game) {
            return Err(Error::new(
                "GAME_MISMATCH",
                "command game_id differs from authenticated game",
            ));
        }
        let mut state = self.state.lock().await;
        if *self.shutdown.borrow() || caller.as_ref().is_some_and(|reply| reply.is_closed()) {
            return Err(Error::new("CANCELLED", "request cancelled before enqueue"));
        }
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
                let handle = membership.as_ref().and_then(|r| room_handle(&state, r));
                drop(state);
                if let Some(handle) = handle {
                    let (reply, received) = oneshot::channel();
                    enqueue(
                        &handle,
                        Control::Leave {
                            player: player.clone(),
                            generation: Some(generation),
                            reply,
                        },
                        caller,
                        None,
                    )
                    .await?;
                    let _ = received.await;
                }
                let mut state = self.state.lock().await;
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
                let handles: Vec<_> = state
                    .rooms
                    .values()
                    .filter(|h| {
                        h.reference.game_id == game
                            && h.public
                            && h.visible
                            && cursor
                                .as_ref()
                                .is_none_or(|cursor| h.reference.id > *cursor)
                    })
                    .cloned()
                    .collect();
                drop(state);
                let mut rooms = Vec::new();
                for handle in handles {
                    if let Some(summary) = self.observations.room_summary(&handle.reference) {
                        rooms.push(json!({"game_id":game,"room_id":handle.reference.id,"room_instance_id":handle.reference.instance,
                            "members":summary.connected_members + summary.reserved_members,"capacity":handle.capacity}));
                        if rooms.len() > *limit as usize {
                            break;
                        }
                    }
                }
                let next_cursor = if rooms.len() > *limit as usize {
                    rooms.pop();
                    rooms
                        .last()
                        .and_then(|v| v["room_id"].as_str())
                        .map(str::to_owned)
                } else {
                    None
                };
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
                let reference = RoomRef {
                    game_id: game.into(),
                    id: random_id("r_")?,
                    instance: random_id("i_")?,
                };
                let (room_stop, stop_rx) = watch::channel(false);
                let (control, controls) = mpsc::channel(64);
                let (inputs, input_rx) = mpsc::channel(self.config.room_input_capacity);
                let (commands, command_rx) = mpsc::channel(self.config.room_input_capacity);
                let (observation, observed) =
                    watch::channel(PublishedRoom::new(&reference, public, capacity));
                let handle = RoomHandle {
                    reference: reference.clone(),
                    stop: room_stop,
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
                drop(state);
                let game_state = match catch_unwind(AssertUnwindSafe(|| {
                    module.create(&Value::Object(settings), &context)
                }))
                .map_err(|_| Error::new("GAME_FAILED", "game factory panicked"))
                .and_then(|result| result.map_err(Error::from))
                {
                    Ok(game) => game,
                    Err(error) => {
                        self.closed(reference, Ok("factory_failed")).await;
                        return Err(error);
                    }
                };
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
                        stop: stop_rx,
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
                        caller,
                    },
                )
                .await;
                match result {
                    Ok(response) => {
                        self.commit_admission(key, generation, &handle, &player)
                            .await?;
                        Ok((response, true))
                    }
                    Err(error) => {
                        handle.stop.send_replace(true);
                        handle.control.closed().await;
                        let mut state = self.state.lock().await;
                        let slot = (reference.game_id.clone(), reference.id.clone());
                        if state
                            .rooms
                            .get(&slot)
                            .is_some_and(|h| h.reference == reference)
                        {
                            state.rooms.remove(&slot);
                        }
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
                let handle = lookup(&state, game, room_id, room_instance_id)?.clone();
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
                drop(state);
                let response = admit(
                    &handle,
                    AdmissionOptions {
                        player: &player,
                        generation,
                        deadline,
                        output,
                        request,
                        resume,
                        identity,
                        caller,
                    },
                )
                .await?;
                self.commit_admission(key, generation, &handle, &player)
                    .await?;
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
                    let handle = room_handle(&state, &reference);
                    drop(state);
                    if let Some(handle) = handle {
                        let (reply, received) = oneshot::channel();
                        enqueue(
                            &handle,
                            Control::Leave {
                                player: player.clone(),
                                generation: Some(generation),
                                reply,
                            },
                            caller,
                            None,
                        )
                        .await?;
                        let _ = received.await;
                    }
                    let mut state = self.state.lock().await;
                    if let Some(session) = state.sessions.get_mut(&key).filter(|s| {
                        s.generation == generation && s.room.as_ref() == Some(&reference)
                    }) {
                        session.room = None;
                        session.reservation = None;
                        output.latest.send_replace(None);
                    }
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
    async fn commit_admission(
        &self,
        key: TokenHash,
        generation: u64,
        handle: &RoomHandle,
        player: &str,
    ) -> Result<(), Error> {
        let mut state = self.state.lock().await;
        let valid = !*self.shutdown.borrow()
            && room_handle(&state, &handle.reference).is_some()
            && state
                .sessions
                .get(&key)
                .is_some_and(|s| s.generation == generation && s.deadline > Instant::now());
        if !valid {
            drop(state);
            leave_handle(handle, player, generation).await;
            return Err(Error::new(
                "UNAUTHENTICATED",
                "admission owner is no longer active",
            ));
        }
        let session = state.sessions.get_mut(&key).expect("validated owner");
        session.room = Some(handle.reference.clone());
        session.reservation = None;
        if let Some(handle) = state.rooms.get_mut(&(
            handle.reference.game_id.clone(),
            handle.reference.id.clone(),
        )) {
            handle.visible = true;
        }
        Ok(())
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
    caller: Option<&'a mut oneshot::Sender<CommandResult>>,
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
        caller,
    } = admission;
    let (tx, rx) = oneshot::channel();
    enqueue(
        handle,
        Control::Enter(Admission {
            player: player.into(),
            generation,
            token_deadline: deadline,
            output: output.clone(),
            request_id: request.id.clone(),
            resume,
            identity,
            reply: tx,
        }),
        caller,
        Some(&output),
    )
    .await?;
    rx.await
        .map_err(|_| Error::new("ROOM_NOT_FOUND", "room task has ended"))?
}
async fn enqueue<T>(
    handle: &RoomHandle,
    control: Control,
    mut caller: Option<&mut oneshot::Sender<T>>,
    output: Option<&Output>,
) -> Result<(), Error> {
    let permit = tokio::select! {
        biased;
        _ = async {
            if let Some(reply) = caller.as_mut() { reply.closed().await; }
            else { std::future::pending::<()>().await; }
        } => return Err(Error::new("CANCELLED", "request cancelled before enqueue")),
        permit = handle.control.reserve() => permit.map_err(|_| Error::new("ROOM_NOT_FOUND", "room task has ended"))?,
    };
    if caller.is_some_and(|reply| reply.is_closed())
        || output.is_some_and(|output| !output.is_alive())
    {
        return Err(Error::new("CANCELLED", "request cancelled before enqueue"));
    }
    permit.send(control);
    Ok(())
}
fn room_handle(state: &State, reference: &RoomRef) -> Option<RoomHandle> {
    state
        .rooms
        .get(&(reference.game_id.clone(), reference.id.clone()))
        .filter(|handle| handle.reference == *reference)
        .cloned()
}
async fn leave_handle(handle: &RoomHandle, player: &str, generation: u64) {
    let (tx, rx) = oneshot::channel();
    if handle
        .control
        .send(Control::Leave {
            player: player.into(),
            generation: Some(generation),
            reply: tx,
        })
        .await
        .is_ok()
    {
        let _ = rx.await;
    }
}
async fn cleanup(inner: &Arc<Inner>) {
    let now = Instant::now();
    let candidates: Vec<_> = {
        let mut state = inner.state.lock().await;
        state.rate_keys.retain(|_, key| {
            now.saturating_duration_since(key.last_seen) < inner.config.rate_key_ttl
        });
        inner
            .metrics
            .rate_keys
            .store(state.rate_keys.len() as u64, Ordering::Relaxed);
        state
            .sessions
            .iter()
            .filter(|(_, s)| s.expired(now) || s.reservation.is_some_and(|d| d <= now))
            .map(|(key, s)| (*key, s.lifecycle.clone()))
            .collect()
    };
    for (key, gate) in candidates {
        let Ok(guard) = gate.clone().try_lock_owned() else {
            continue;
        };
        let Ok(permit) = inner.operations.clone().try_acquire_owned() else {
            continue;
        };
        let mut state = inner.state.lock().await;
        let Some(session) = state
            .sessions
            .get_mut(&key)
            .filter(|s| Arc::ptr_eq(&s.lifecycle, &gate))
        else {
            continue;
        };
        let expired = session.expired(Instant::now());
        if !expired && session.reservation.is_none_or(|d| d > Instant::now()) {
            continue;
        }
        let reference = session.room.take();
        let player = session.id.clone();
        let generation = session.generation;
        session.reservation = None;
        if expired {
            if let Some(output) = session.output.take() {
                output.close();
            }
            state.sessions.remove(&key);
        }
        let handle = reference.as_ref().and_then(|r| room_handle(&state, r));
        inner
            .metrics
            .active_sessions
            .store(state.sessions.len() as u64, Ordering::Relaxed);
        drop(state);
        if let Some(handle) = handle {
            tokio::spawn(async move {
                let (_guard, _permit) = (guard, permit);
                leave_handle(&handle, &player, generation).await;
            });
        }
    }
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
    async fn hardening_stop_bypasses_full_control_queue() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let (out, _rx) = output(32);
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
        let mut replies = Vec::new();
        for _ in 0..handle.control.max_capacity() {
            let (reply, received) = oneshot::channel();
            handle.control.try_send(Control::List { reply }).unwrap();
            replies.push(received);
        }
        runtime.shutdown().await;
        for reply in replies {
            assert!(reply.await.is_err(), "Stop must bypass queued controls");
        }
        assert_eq!(runtime.metrics().active_rooms, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_shutdown_reports_pending_without_claiming_completion() {
        let runtime = Campfire::new(Config::default()).unwrap();
        let connection = runtime
            .inner
            .connections
            .clone()
            .try_acquire_owned()
            .unwrap();
        let operation = runtime
            .inner
            .operations
            .clone()
            .try_acquire_owned()
            .unwrap();
        let began = Instant::now();
        assert!(
            tokio::time::timeout(Duration::from_secs(6), runtime.shutdown())
                .await
                .is_ok(),
            "shutdown wrapper must return after its one bounded deadline"
        );
        assert_eq!(began.elapsed(), Duration::from_secs(5));
        let began = Instant::now();
        let report = runtime.shutdown_with_timeout(Duration::from_secs(2)).await;
        assert_eq!(
            report,
            ShutdownReport {
                completed: false,
                remaining_rooms: 0,
                remaining_connections: 1,
                remaining_operations: 1
            }
        );
        assert_eq!(began.elapsed(), Duration::from_secs(2));
        drop(connection);
        drop(operation);
        let report = runtime.shutdown_with_timeout(Duration::from_secs(2)).await;
        assert_eq!(
            report,
            ShutdownReport {
                completed: true,
                remaining_rooms: 0,
                remaining_connections: 0,
                remaining_operations: 0
            }
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hardening_shutdown_reports_blocked_callback_until_released() {
        struct Blocker {
            armed: AtomicBool,
            started: std::sync::mpsc::Sender<()>,
            released: std::sync::Mutex<bool>,
            release: std::sync::Condvar,
        }
        struct Release(Arc<Blocker>);
        impl Drop for Release {
            fn drop(&mut self) {
                *self.0.released.lock().unwrap() = true;
                self.0.release.notify_all();
            }
        }
        struct BlockingModule(Arc<Blocker>);
        impl GameModule for BlockingModule {
            fn create(
                &self,
                _: &Value,
                _: &GameContext,
            ) -> Result<Box<dyn Game>, crate::GameError> {
                Ok(Box::new(BlockingGame(self.0.clone())))
            }
        }
        struct BlockingGame(Arc<Blocker>);
        impl Game for BlockingGame {
            fn player_joined(&mut self, _: &str) {}
            fn player_left(&mut self, _: &str) {}
            fn player_connection_changed(&mut self, _: &str, _: bool) {}
            fn validate_input(&self, _: &str, _: &Value) -> Result<(), crate::GameError> {
                Ok(())
            }
            fn apply_input(&mut self, _: &str, _: &Value) {}
            fn tick(&mut self, _: Duration) {
                if self.0.armed.swap(false, Ordering::Relaxed) {
                    self.0.started.send(()).unwrap();
                    let mut released = self.0.released.lock().unwrap();
                    while !*released {
                        released = self.0.release.wait(released).unwrap();
                    }
                }
            }
            fn snapshot(&self) -> Value {
                json!({})
            }
        }
        let (started, started_rx) = std::sync::mpsc::channel();
        let blocker = Arc::new(Blocker {
            armed: AtomicBool::new(false),
            started,
            released: std::sync::Mutex::new(false),
            release: std::sync::Condvar::new(),
        });
        let release = Release(blocker.clone());
        let runtime = Campfire::new(Config::default()).unwrap();
        runtime
            .register_game("a", BlockingModule(blocker.clone()))
            .unwrap();
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let (out, _rx) = output(32);
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
        let (reported, received) = oneshot::channel();
        let shutdown_runtime = runtime.clone();
        let shutdown_thread = std::thread::spawn(move || {
            // The deadline runs on a different scheduler from the blocked callback.
            let _release = release;
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            let clock = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            let report =
                clock.block_on(shutdown_runtime.shutdown_with_timeout(Duration::from_millis(20)));
            assert_eq!(shutdown_runtime.metrics().active_rooms, 1);
            reported.send(report).unwrap();
        });
        blocker.armed.store(true, Ordering::Relaxed);
        let report = received.await.unwrap();
        shutdown_thread.join().unwrap();
        assert_eq!(
            report,
            ShutdownReport {
                completed: false,
                remaining_rooms: 1,
                remaining_connections: 0,
                remaining_operations: 0
            }
        );
        let report = runtime.shutdown_with_timeout(Duration::from_secs(1)).await;
        assert!(report.completed);
        assert_eq!(report.remaining_rooms, 0);
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
                stop: watch::channel(false).0,
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
    async fn hardening_cancelled_enter_after_real_actor_commit_preserves_one_reserved_seat() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let host = runtime.inner.issue("a", "host".into()).await.unwrap();
        let (host_out, _host_rx) = output(32);
        let (host_key, host_generation, _, _) = runtime
            .inner
            .authenticate(
                "a",
                host["token"].as_str().unwrap(),
                "host",
                host_out.clone(),
            )
            .await
            .unwrap();
        let create = crate::protocol::parse(
            r#"{"v":1,"type":"room.create","request_id":"create","settings":{}}"#,
        )
        .unwrap();
        let created = runtime
            .inner
            .command(host_key, host_generation, "a", host_out, &create)
            .await
            .unwrap()
            .0;
        let guest = runtime.inner.issue("a", "guest".into()).await.unwrap();
        let (out, _rx) = output(32);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "guest", out.clone())
            .await
            .unwrap();
        let (proxy, mut controls) = mpsc::channel(8);
        let actual = {
            let mut state = runtime.inner.state.lock().await;
            let handle = state.rooms.values_mut().next().unwrap();
            std::mem::replace(&mut handle.control, proxy)
        };
        let (committed, committed_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let actor = actual.clone();
        let forwarding = tokio::spawn(async move {
            let Some(Control::Enter(mut admission)) = controls.recv().await else {
                panic!("Enter expected")
            };
            let (ack, received) = oneshot::channel();
            let original = std::mem::replace(&mut admission.reply, ack);
            actor.send(Control::Enter(admission)).await.unwrap();
            let result = received.await.unwrap();
            committed.send(()).unwrap();
            release_rx.await.unwrap();
            original.send(result).unwrap();
            let Some(control @ Control::Disconnect { .. }) = controls.recv().await else {
                panic!("Disconnect expected")
            };
            actor.send(control).await.unwrap();
        });
        let request = crate::protocol::parse(&json!({"v":1,"type":"room.join","request_id":"join","room_id":created["room_id"],"room_instance_id":created["room_instance_id"]}).to_string()).unwrap();
        let inner = runtime.inner.clone();
        let pending =
            tokio::spawn(async move { inner.command(key, generation, "a", out, &request).await });
        committed_rx.await.unwrap();
        pending.abort();
        let _ = pending.await;
        release.send(()).unwrap();
        forwarding.await.unwrap();
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        let (reply, received) = oneshot::channel();
        actual.send(Control::List { reply }).await.unwrap();
        assert_eq!(
            received.await.unwrap(),
            2,
            "host and cancelled committed guest each retain exactly one seat"
        );
        {
            let mut state = runtime.inner.state.lock().await;
            let guest = &state.sessions[&key];
            assert_eq!(guest.generation, generation);
            assert!(guest.room.is_some());
            assert!(guest.reservation.is_some());
            assert!(guest.output.is_none());
            assert_eq!(state.rooms.len(), 1);
            assert_eq!(runtime.metrics().active_rooms, 1);
            state.rooms.values_mut().next().unwrap().control = actual;
        }
        let summary = runtime.observer().snapshot().unwrap().rooms.pop().unwrap();
        assert_eq!(
            (summary.connected_members, summary.reserved_members),
            (1, 1)
        );
        runtime.shutdown().await;
        assert_eq!(runtime.metrics().active_rooms, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_cached_pagination_skips_closed_private_and_uncommitted_rooms() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let (out, _rx) = output(16);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let (_controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let mut publishers = Vec::new();
        {
            let mut state = runtime.inner.state.lock().await;
            let template = state.rooms.pop_first().unwrap().1;
            for (id, public, visible, alive) in [
                ("a", true, true, false),
                ("b", true, true, true),
                ("c", false, true, true),
                ("d", true, false, true),
                ("e", true, true, true),
                ("f", true, true, true),
            ] {
                let mut handle = template.clone();
                handle.reference.id = id.into();
                handle.public = public;
                handle.visible = visible;
                let (publisher, receiver) =
                    watch::channel(PublishedRoom::new(&handle.reference, public, 16));
                runtime
                    .inner
                    .observations
                    .insert(&handle.reference, receiver);
                if alive {
                    publishers.push(publisher);
                }
                state.rooms.insert(("a".into(), id.into()), handle);
            }
        }
        let mut cursor = Value::Null;
        for (expected, next) in [("b", json!("b")), ("e", json!("e")), ("f", Value::Null)] {
            let request = crate::protocol::parse(
                &json!({"v":1,"type":"room.list","request_id":"list","cursor":cursor,"limit":1})
                    .to_string(),
            )
            .unwrap();
            let listed = runtime
                .inner
                .command(key, generation, "a", out.clone(), &request)
                .await
                .unwrap()
                .0;
            assert_eq!(listed["rooms"].as_array().unwrap().len(), 1);
            assert_eq!(listed["rooms"][0]["room_id"], expected);
            assert_eq!(listed["next_cursor"], next);
            cursor = next;
        }
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_cancellation_before_enter_enqueue_has_no_admission() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let guest = runtime.inner.issue("a", "first".into()).await.unwrap();
        let (out, _rx) = output(16);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let handle = {
            let mut state = runtime.inner.state.lock().await;
            state.sessions.get_mut(&key).unwrap().room = None;
            state.rooms.values().next().unwrap().clone()
        };
        for _ in 0..8 {
            let (reply, _) = oneshot::channel();
            handle.control.try_send(Control::List { reply }).unwrap();
        }
        let inner = runtime.inner.clone();
        let request = crate::protocol::parse(r#"{"v":1,"type":"room.join","request_id":"join","room_id":"stub","room_instance_id":"epoch"}"#).unwrap();
        let pending =
            tokio::spawn(async move { inner.command(key, generation, "a", out, &request).await });
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        pending.abort();
        let _ = pending.await;
        for _ in 0..8 {
            assert!(matches!(controls.recv().await, Some(Control::List { .. })));
        }
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            controls.try_recv().is_err(),
            "cancelled request must not enqueue Enter after queue space returns"
        );
        assert!(
            runtime.inner.state.lock().await.sessions[&key]
                .room
                .is_none()
        );
        assert_eq!(
            runtime.inner.operations.available_permits(),
            runtime.inner.config.max_sessions
        );
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_shutdown_releases_callers_waiting_for_session_gate() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let token = guest["token"].as_str().unwrap().to_owned();
        let key = token_hash(&token);
        let gate = runtime.inner.state.lock().await.sessions[&key]
            .lifecycle
            .clone();
        let _blocked = gate.lock_owned().await;
        let inner = runtime.inner.clone();
        let (out, _rx) = output(16);
        let caller =
            tokio::spawn(async move { inner.authenticate("a", &token, "auth", out).await });
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        runtime.inner.shutdown.send_replace(true);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), caller)
                .await
                .is_ok(),
            "shutdown must release callers that have not spawned an operation"
        );
        assert_eq!(
            runtime.inner.operations.available_permits(),
            runtime.inner.config.max_sessions
        );
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_disconnect_uses_actor_deadline_instead_of_late_ack_time() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let guest = runtime.inner.issue("a", "source".into()).await.unwrap();
        let (out, _rx) = output(16);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out)
            .await
            .unwrap();
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let inner = runtime.inner.clone();
        let pending = tokio::spawn(async move { inner.disconnect(key, generation).await });
        let Control::Disconnect { reply, .. } = controls.recv().await.unwrap() else {
            panic!("Disconnect expected")
        };
        let actor_deadline = Instant::now() + Duration::from_secs(30);
        tokio::time::advance(Duration::from_secs(5)).await;
        reply.send(Some(actor_deadline)).unwrap();
        pending.await.unwrap();
        assert_eq!(
            runtime.inner.state.lock().await.sessions[&key].reservation,
            Some(actor_deadline)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_cancelled_short_rotation_preserves_actor_reservation_deadline() {
        struct ChangingGrant(Arc<std::sync::atomic::AtomicU64>);
        impl AccountAuthenticator for ChangingGrant {
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
                    Ok(crate::AccountGrant {
                        identity: crate::AccountIdentity {
                            issuer: "host".into(),
                            subject: "alice".into(),
                            claims: json!({}),
                        },
                        valid_for: Duration::from_secs(self.0.load(Ordering::Relaxed)),
                    })
                })
            }
        }
        let duration = Arc::new(std::sync::atomic::AtomicU64::new(60));
        let runtime = account_runtime(Config::default(), ChangingGrant(duration.clone()));
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
        runtime
            .inner
            .command(key, generation, "a", out, &create)
            .await
            .unwrap();
        duration.store(1, Ordering::Relaxed);
        let (proxy, mut controls) = mpsc::channel(8);
        let actual = {
            let mut state = runtime.inner.state.lock().await;
            std::mem::replace(&mut state.rooms.values_mut().next().unwrap().control, proxy)
        };
        let (prepared, prepared_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let actor = actual.clone();
        let forwarding = tokio::spawn(async move {
            let Some(Control::Replace {
                player,
                generation,
                rotation,
                reply,
            }) = controls.recv().await
            else {
                panic!("Replace expected")
            };
            let (ack, received) = oneshot::channel();
            actor
                .send(Control::Replace {
                    player,
                    generation,
                    rotation,
                    reply: ack,
                })
                .await
                .unwrap();
            let result = received.await.unwrap();
            prepared.send(()).unwrap();
            release_rx.await.unwrap();
            reply.send(result).unwrap();
        });
        let inner = runtime.inner.clone();
        let pending =
            tokio::spawn(async move { inner.issue_account("a", "alice", "source".into()).await });
        prepared_rx.await.unwrap();
        let prior_deadline = Instant::now() + Duration::from_secs(30);
        pending.abort();
        let _ = pending.await;
        release.send(()).unwrap();
        forwarding.await.unwrap();
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        {
            let mut state = runtime.inner.state.lock().await;
            state.rooms.values_mut().next().unwrap().control = actual.clone();
            assert_eq!(state.sessions[&key].reservation, Some(prior_deadline));
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        cleanup(&runtime.inner).await;
        assert!(
            runtime.inner.state.lock().await.sessions[&key]
                .room
                .is_some()
        );
        let (reply, received) = oneshot::channel();
        actual.send(Control::List { reply }).await.unwrap();
        assert_eq!(received.await.unwrap(), 1);
        assert_eq!(runtime.metrics().active_rooms, 1);
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_removed_sessions_do_not_release_operation_capacity_early() {
        let runtime = account_runtime(
            Config {
                max_sessions: 1,
                guest_ttl: Duration::from_secs(1),
                ..Config::default()
            },
            AccountVerifier,
        );
        let guest = runtime.inner.issue("a", "first".into()).await.unwrap();
        let key = token_hash(guest["token"].as_str().unwrap());
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        tokio::time::advance(Duration::from_secs(1)).await;
        cleanup(&runtime.inner).await;
        let Control::Leave {
            generation, reply, ..
        } = controls.recv().await.unwrap()
        else {
            panic!("cleanup must leave")
        };
        assert_eq!(generation, Some(0));
        assert_eq!(runtime.metrics().active_sessions, 0);
        assert_eq!(runtime.inner.operations.available_permits(), 0);
        let replacement = runtime.inner.issue("a", "second".into()).await.unwrap();
        let (out, _rx) = output(16);
        assert_eq!(
            runtime
                .inner
                .authenticate(
                    "a",
                    replacement["token"].as_str().unwrap(),
                    "auth",
                    out.clone()
                )
                .await
                .unwrap_err()
                .code,
            "SERVER_BUSY"
        );
        reply.send(()).unwrap();
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            runtime
                .inner
                .authenticate("a", replacement["token"].as_str().unwrap(), "auth", out)
                .await
                .is_ok()
        );
        assert_eq!(runtime.inner.operations.available_permits(), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_parallel_rotation_and_expiry() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let issued = runtime
            .inner
            .issue_account("a", "alice", "first".into())
            .await
            .unwrap();
        let token = issued["token"].as_str().unwrap().to_owned();
        let key = token_hash(&token);
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let other = runtime.inner.issue("a", "second".into()).await.unwrap();
        let other_key = token_hash(other["token"].as_str().unwrap());
        runtime
            .inner
            .state
            .lock()
            .await
            .sessions
            .get_mut(&other_key)
            .unwrap()
            .deadline = Instant::now();
        let inner = runtime.inner.clone();
        let pending =
            tokio::spawn(async move { inner.issue_account("a", "alice", "rotation".into()).await });
        let Control::Replace {
            generation: prepared,
            reply,
            rotation,
            ..
        } = controls.recv().await.unwrap()
        else {
            panic!("rotation prepare")
        };
        cleanup(&runtime.inner).await;
        {
            let state = runtime.inner.state.lock().await;
            assert!(state.sessions.contains_key(&key));
            assert!(!state.sessions.contains_key(&other_key));
        }
        pending.abort();
        let _ = pending.await;
        reply
            .send(Some(Instant::now() + Duration::from_secs(30)))
            .unwrap();
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            !rotation.unwrap().commit(),
            "cancelled prepare must be aborted"
        );
        let inner = runtime.inner.clone();
        let (out, _rx) = output(16);
        let retry =
            tokio::spawn(async move { inner.authenticate("a", &token, "retry", out).await });
        let Control::Replace {
            generation, reply, ..
        } = controls.recv().await.unwrap()
        else {
            panic!("authentication replacement")
        };
        assert!(generation > prepared);
        reply
            .send(Some(Instant::now() + Duration::from_secs(30)))
            .unwrap();
        assert_eq!(retry.await.unwrap().unwrap().1, generation);
        assert_eq!(runtime.metrics().active_sessions, 1);
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_identity_gate_registry_is_bounded() {
        let runtime = account_runtime(
            Config {
                guest_burst: 100,
                ..Config::default()
            },
            AccountVerifier,
        );
        for i in 0..40 {
            runtime
                .inner
                .issue_account("a", &format!("player-{i}"), "source".into())
                .await
                .unwrap();
        }
        assert_eq!(runtime.metrics().active_sessions, 40);
        assert!(runtime.inner.identity_gates.lock().unwrap().len() <= 1);
        assert_eq!(
            runtime.inner.operations.available_permits(),
            runtime.inner.config.max_sessions
        );
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_factory_runs_without_state_lock_and_reserves_room_capacity() {
        struct FactoryProbe {
            inner: Weak<Inner>,
        }
        impl GameModule for FactoryProbe {
            fn create(
                &self,
                _: &Value,
                _: &GameContext,
            ) -> Result<Box<dyn Game>, crate::GameError> {
                let inner = self.inner.upgrade().unwrap();
                let state = inner
                    .state
                    .try_lock()
                    .expect("factory must run outside State");
                assert_eq!(
                    state.rooms.len(),
                    1,
                    "pending factory consumes the room slot"
                );
                assert!(!state.rooms.values().next().unwrap().visible);
                assert_eq!(inner.metrics.active_rooms.load(Ordering::Relaxed), 1);
                Ok(Box::<TestGame>::default())
            }
        }
        let runtime = Campfire::new(Config {
            max_rooms: 1,
            ..Config::default()
        })
        .unwrap();
        runtime
            .register_game(
                "a",
                FactoryProbe {
                    inner: Arc::downgrade(&runtime.inner),
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
        let create = crate::protocol::parse(
            r#"{"v":1,"type":"room.create","request_id":"create","settings":{}}"#,
        )
        .unwrap();
        runtime
            .inner
            .command(key, generation, "a", out, &create)
            .await
            .unwrap();
        assert_eq!(runtime.metrics().active_rooms, 1);
        assert!(
            runtime
                .inner
                .state
                .lock()
                .await
                .rooms
                .values()
                .next()
                .unwrap()
                .visible
        );
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_slow_room_does_not_block_other_session() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let first = runtime.inner.issue("a", "first".into()).await.unwrap();
        let token = first["token"].as_str().unwrap().to_owned();
        let key = token_hash(&token);
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let inner = runtime.inner.clone();
        let (out, _rx) = output(16);
        let blocked = tokio::spawn(async move { inner.authenticate("a", &token, "a", out).await });
        let Control::Replace { reply, .. } = controls.recv().await.unwrap() else {
            panic!("expected Replace barrier")
        };
        let independent = tokio::time::timeout(Duration::from_secs(1), async {
            let second = runtime.inner.issue("a", "second".into()).await.unwrap();
            let (out, _rx) = output(16);
            let (key, generation, _, _) = runtime
                .inner
                .authenticate("a", second["token"].as_str().unwrap(), "b", out.clone())
                .await
                .unwrap();
            let request = crate::protocol::parse(
                r#"{"v":1,"type":"room.list","request_id":"list","cursor":null,"limit":10}"#,
            )
            .unwrap();
            runtime
                .inner
                .command(key, generation, "a", out, &request)
                .await
                .unwrap();
        })
        .await;
        blocked.abort();
        drop(reply);
        assert!(
            independent.is_ok(),
            "unrelated session must progress while Replace is blocked"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_list_uses_cached_summary() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let guest = runtime.inner.issue("a", "first".into()).await.unwrap();
        let (out, _rx) = output(16);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let (_controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        let reference = runtime
            .inner
            .state
            .lock()
            .await
            .rooms
            .values()
            .next()
            .unwrap()
            .reference
            .clone();
        let mut published = PublishedRoom::new(&reference, true, 16);
        published.summary.connected_members = 2;
        published.summary.reserved_members = 1;
        let (_publisher, receiver) = watch::channel(published);
        runtime.inner.observations.insert(&reference, receiver);
        let request = crate::protocol::parse(
            r#"{"v":1,"type":"room.list","request_id":"list","cursor":null,"limit":1}"#,
        )
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            runtime.inner.command(key, generation, "a", out, &request),
        )
        .await;
        let listed = result
            .expect("cached list must not contact control receiver")
            .unwrap()
            .0;
        assert_eq!(listed["rooms"][0]["members"], 3);
        assert_eq!(listed["next_cursor"], Value::Null);
    }
    #[tokio::test(start_paused = true)]
    async fn hardening_cancelled_enter_reconciles_seat() {
        let runtime = account_runtime(Config::default(), AccountVerifier);
        let guest = runtime.inner.issue("a", "first".into()).await.unwrap();
        let (out, _rx) = output(16);
        let (key, generation, _, _) = runtime
            .inner
            .authenticate("a", guest["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        let (mut controls, _commands, _sender) =
            stub_membership(&mut *runtime.inner.state.lock().await, key);
        runtime
            .inner
            .state
            .lock()
            .await
            .sessions
            .get_mut(&key)
            .unwrap()
            .room = None;
        let inner = runtime.inner.clone();
        let request = crate::protocol::parse(r#"{"v":1,"type":"room.join","request_id":"join","room_id":"stub","room_instance_id":"epoch"}"#).unwrap();
        let pending =
            tokio::spawn(async move { inner.command(key, generation, "a", out, &request).await });
        let Control::Enter(admission) = controls.recv().await.unwrap() else {
            panic!("expected Enter")
        };
        pending.abort();
        let _ = pending.await;
        assert!(
            !admission.reply.is_closed(),
            "enqueued Enter must retain its result owner"
        );
        admission
            .reply
            .send(Ok(json!({"type":"room.joined"})))
            .unwrap();
        let control = tokio::time::timeout(Duration::from_secs(1), controls.recv())
            .await
            .unwrap()
            .unwrap();
        let Control::Disconnect {
            generation: disconnected,
            reply,
            ..
        } = control
        else {
            panic!("cancelled Enter must reserve seat")
        };
        assert_eq!(disconnected, generation);
        reply
            .send(Some(Instant::now() + Duration::from_secs(30)))
            .unwrap();
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let state = runtime.inner.state.lock().await;
        assert_eq!(
            state.sessions[&key].room.as_ref().unwrap().instance,
            "epoch"
        );
        assert!(state.sessions[&key].reservation.is_some());
        assert!(state.sessions[&key].output.is_none());
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
    async fn hardening_create_cancel_releases_slot() {
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

    #[tokio::test(start_paused = true)]
    async fn unclaimed_guest_tokens_release_session_capacity() {
        let runtime = account_runtime(
            Config {
                max_sessions: 2,
                unclaimed_guest_ttl: Duration::from_secs(5),
                ..Config::default()
            },
            AccountVerifier,
        );
        let claimed = runtime.inner.issue("a", "one".into()).await.unwrap();
        let unused = runtime.inner.issue("a", "two".into()).await.unwrap();
        let (out, _rx) = output(32);
        runtime
            .inner
            .authenticate("a", claimed["token"].as_str().unwrap(), "auth", out.clone())
            .await
            .unwrap();
        assert_eq!(
            runtime
                .inner
                .issue("a", "three".into())
                .await
                .unwrap_err()
                .code,
            "SERVER_BUSY"
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        // The unused token is rejected even before the periodic cleanup observes it.
        let (late, _late_rx) = output(32);
        assert_eq!(
            runtime
                .inner
                .authenticate("a", unused["token"].as_str().unwrap(), "late", late)
                .await
                .unwrap_err()
                .code,
            "UNAUTHENTICATED"
        );
        cleanup(&runtime.inner).await;
        {
            let state = runtime.inner.state.lock().await;
            assert_eq!(state.sessions.len(), 1);
            assert!(
                state
                    .sessions
                    .contains_key(&token_hash(claimed["token"].as_str().unwrap()))
            );
        }
        assert_eq!(runtime.metrics().active_sessions, 1);
        runtime.inner.issue("a", "three".into()).await.unwrap();
        // A claimed token keeps its full guest lifetime after the connection closes.
        out.close();
        tokio::time::advance(Duration::from_secs(60)).await;
        cleanup(&runtime.inner).await;
        let (again, _again_rx) = output(32);
        runtime
            .inner
            .authenticate("a", claimed["token"].as_str().unwrap(), "again", again)
            .await
            .unwrap();
        runtime.shutdown().await;
    }
    #[tokio::test(start_paused = true)]
    async fn unclaimed_guest_ttl_never_extends_guest_ttl() {
        let runtime = account_runtime(
            Config {
                guest_ttl: Duration::from_secs(1),
                unclaimed_guest_ttl: Duration::from_secs(60),
                ..Config::default()
            },
            AccountVerifier,
        );
        let guest = runtime.inner.issue("a", "one".into()).await.unwrap();
        let key = token_hash(guest["token"].as_str().unwrap());
        {
            let state = runtime.inner.state.lock().await;
            assert_eq!(
                state.sessions[&key].unclaimed_until,
                Some(state.sessions[&key].deadline)
            );
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        cleanup(&runtime.inner).await;
        assert!(runtime.inner.state.lock().await.sessions.is_empty());
        runtime.shutdown().await;
    }
}
