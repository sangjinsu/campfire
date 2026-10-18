use crate::{
    Config, Game, PlayerIdentity,
    metrics::{Metrics, inc, max},
    observation::PublishedRoom,
    output::Output,
    protocol::{Error, fingerprint},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{Instant, MissedTickBehavior},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoomRef {
    pub game_id: String,
    pub id: String,
    pub instance: String,
}
#[derive(Clone)]
pub(crate) struct RoomHandle {
    pub reference: RoomRef,
    pub control: mpsc::Sender<Control>,
    pub inputs: mpsc::Sender<Input>,
    pub commands: mpsc::Sender<GameCommand>,
    pub public: bool,
    pub visible: bool,
    pub capacity: usize,
}
pub(crate) struct Admission {
    pub player: String,
    pub generation: u64,
    pub token_deadline: Instant,
    pub output: Arc<Output>,
    pub request_id: String,
    pub resume: bool,
    pub identity: PlayerIdentity,
    pub reply: oneshot::Sender<Result<Value, Error>>,
}
pub(crate) struct Input {
    pub player: String,
    pub generation: u64,
    pub sequence: u64,
    pub payload: Value,
    pub received: Instant,
    pub reply: oneshot::Sender<Result<(), Error>>,
}
pub(crate) struct GameCommand {
    pub player: String,
    pub generation: u64,
    pub sequence: u64,
    pub payload: Value,
    pub request_id: String,
    pub reply: oneshot::Sender<Result<Value, Error>>,
}
#[derive(Clone, Copy)]
enum RotationDecision {
    Pending { valid_until: Option<Instant> },
    Committed(Instant),
    Aborted,
}
/// One synchronous decision coordinates the token-map commit with actor expiry.
pub(crate) struct Rotation {
    deadline: Instant,
    identity: PlayerIdentity,
    decision: StdMutex<RotationDecision>,
}
impl Rotation {
    pub fn new(deadline: Instant, identity: PlayerIdentity) -> Arc<Self> {
        Arc::new(Self {
            deadline,
            identity,
            decision: StdMutex::new(RotationDecision::Pending { valid_until: None }),
        })
    }
    fn prepare(&self, valid_until: Instant) -> bool {
        let mut decision = self
            .decision
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(*decision, RotationDecision::Pending { .. }) && valid_until > Instant::now() {
            *decision = RotationDecision::Pending {
                valid_until: Some(valid_until),
            };
            true
        } else {
            false
        }
    }
    pub fn commit(&self) -> bool {
        let mut decision = self
            .decision
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        if matches!(*decision,RotationDecision::Pending {valid_until:Some(until)} if until>now)
            && self.deadline > now
        {
            *decision = RotationDecision::Committed(now);
            true
        } else {
            *decision = RotationDecision::Aborted;
            false
        }
    }
    pub fn abort(&self) -> bool {
        let mut decision = self
            .decision
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(*decision, RotationDecision::Committed(_)) {
            false
        } else {
            *decision = RotationDecision::Aborted;
            true
        }
    }
    fn outcome(&self) -> RotationDecision {
        *self
            .decision
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}
pub(crate) enum Control {
    Enter(Admission),
    Replace {
        player: String,
        generation: u64,
        rotation: Option<Arc<Rotation>>,
        reply: oneshot::Sender<Option<Instant>>,
    },
    Disconnect {
        player: String,
        generation: u64,
        reply: oneshot::Sender<()>,
    },
    Leave {
        player: String,
        generation: Option<u64>,
        reply: oneshot::Sender<()>,
    },
    List {
        reply: oneshot::Sender<usize>,
    },
    Stop,
}
struct Pending {
    sequence: u64,
    generation: u64,
    payload: Value,
    received: Instant,
}
struct Member {
    generation: u64,
    token_deadline: Instant,
    reserved_until: Option<Instant>,
    output: Option<Arc<Output>>,
    highest_received: u64,
    processed: u64,
    pending: Option<Pending>,
    command_processed: u64,
    command_results: VecDeque<(u64, [u8; 32], Value)>,
    rotation: Option<PendingRotation>,
}
struct PendingRotation {
    transaction: Arc<Rotation>,
    reservation: Instant,
}
impl Drop for PendingRotation {
    fn drop(&mut self) {
        self.transaction.abort();
    }
}
struct Room {
    reference: RoomRef,
    config: Config,
    capacity: usize,
    game: Box<dyn Game>,
    members: BTreeMap<String, Member>,
    tick: u64,
    snapshot_seq: u64,
    ever_joined: bool,
    metrics: Arc<Metrics>,
    observation: watch::Sender<PublishedRoom>,
    observation_dirty: bool,
    last_observed: Instant,
}

pub(crate) struct Channels {
    pub observation: watch::Sender<PublishedRoom>,
    pub controls: mpsc::Receiver<Control>,
    pub inputs: mpsc::Receiver<Input>,
    pub commands: mpsc::Receiver<GameCommand>,
}
enum Data {
    Input(Option<Input>),
    Command(Option<GameCommand>),
}
pub(crate) async fn run(
    reference: RoomRef,
    config: Config,
    capacity: usize,
    game: Box<dyn Game>,
    metrics: Arc<Metrics>,
    channels: Channels,
) -> &'static str {
    let Channels {
        observation,
        mut controls,
        mut inputs,
        mut commands,
    } = channels;
    let mut room = Room {
        reference,
        config: config.clone(),
        capacity,
        game,
        members: BTreeMap::new(),
        tick: 0,
        snapshot_seq: 0,
        ever_joined: false,
        metrics,
        observation,
        observation_dirty: true,
        last_observed: Instant::now(),
    };
    room.observe_if_due();
    let period = config.tick_interval();
    let start = Instant::now() + period;
    let mut ticker = tokio::time::interval_at(start, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut snapshot_phase = 0_u64;
    loop {
        tokio::select! { biased;
            scheduled=ticker.tick()=> {
                max(&room.metrics.max_tick_delay_micros,Instant::now().saturating_duration_since(scheduled).as_micros() as u64);
                let began=std::time::Instant::now();
                room.step(period);
                snapshot_phase+=u64::from(config.snapshot_hz);
                if snapshot_phase>=u64::from(config.tick_hz) {
                    room.publish();
                    snapshot_phase-=u64::from(config.tick_hz);
                }
                let elapsed=began.elapsed();
                max(&room.metrics.max_tick_duration_micros,elapsed.as_micros() as u64);
                if elapsed>period {tracing::warn!(game_id=%room.reference.game_id,room_id=%room.reference.id,server_tick=room.tick,duration_micros=elapsed.as_micros() as u64,budget_micros=period.as_micros() as u64,"trusted game tick exceeded processing budget");}
            }
            command=controls.recv()=> {
                match command {
                    Some(Control::Stop)|None=>return "shutdown",
                    Some(command)=>room.control(command),
                }
            }
            data=async { tokio::select! {
                input=inputs.recv()=>Data::Input(input),
                command=commands.recv()=>Data::Command(command),
            }}=> { match data {
                Data::Input(Some(input))=> {
                    let began=std::time::Instant::now();room.input(input);
                    max(&room.metrics.max_input_duration_micros,began.elapsed().as_micros() as u64);
                }
                Data::Command(Some(command))=>room.command(command),
                _=>{},
            }}
        }
        room.observe_if_due();
        if room.ever_joined && room.members.is_empty() {
            return "empty";
        }
    }
}
impl Room {
    fn observe_if_due(&mut self) {
        if !self.observation_dirty && self.last_observed.elapsed() < Duration::from_secs(1) {
            return;
        }
        let mut summary = self.observation.borrow().summary.clone();
        summary.reserved_members = self
            .members
            .values()
            .filter(|member| member.reserved_until.is_some())
            .count();
        summary.connected_members = self.members.len() - summary.reserved_members;
        summary.server_tick = self.tick;
        summary.snapshot_seq = self.snapshot_seq;
        let now = Instant::now();
        self.observation.send_replace(PublishedRoom {
            summary,
            published_at: now,
        });
        self.last_observed = now;
        self.observation_dirty = false;
    }
    fn reserve(&mut self, player: &str, generation: u64) {
        if let Some(member) = self.members.get_mut(player) {
            if member.generation != generation {
                return;
            }
            member.pending = None;
            member.highest_received = member.processed;
            member.output = None;
            self.observation_dirty = true;
            member.reserved_until.get_or_insert_with(|| {
                (Instant::now() + self.config.reconnect_grace).min(member.token_deadline)
            });
            self.game.player_connection_changed(player, false);
        }
    }
    fn control(&mut self, command: Control) {
        self.settle_rotations();
        match command {
            Control::Enter(admission) => {
                self.enter(admission);
            }
            Control::Replace {
                player,
                generation,
                rotation,
                reply,
            } => {
                self.expire();
                let mut reservation = None;
                if let Some(member) = self.members.get(&player) {
                    let candidate = member
                        .reserved_until
                        .unwrap_or_else(|| Instant::now() + self.config.reconnect_grace);
                    let valid_until = candidate.min(member.token_deadline);
                    if rotation
                        .as_ref()
                        .is_some_and(|transaction| !transaction.prepare(valid_until))
                    {
                        let _ = reply.send(None);
                        return;
                    }
                    let old = member.generation;
                    if let Some(output) = &member.output {
                        output.close();
                    }
                    self.reserve(&player, old);
                    let member = self.members.get_mut(&player).expect("existing member");
                    if let Some(previous) = member.rotation.take() {
                        previous.transaction.abort();
                    }
                    member.generation = generation;
                    if let Some(transaction) = rotation {
                        reservation = Some(candidate.min(transaction.deadline));
                        member.rotation = Some(PendingRotation {
                            transaction,
                            reservation: candidate,
                        });
                    } else {
                        reservation = member.reserved_until;
                    }
                }
                let _ = reply.send(reservation);
            }
            Control::Disconnect {
                player,
                generation,
                reply,
            } => {
                self.reserve(&player, generation);
                let _ = reply.send(());
            }
            Control::Leave {
                player,
                generation,
                reply,
            } => {
                if self
                    .members
                    .get(&player)
                    .is_some_and(|m| generation.is_none_or(|g| g == m.generation))
                {
                    if let Some(member) = self.members.remove(&player)
                        && let Some(rotation) = member.rotation
                    {
                        rotation.transaction.abort();
                    }
                    self.observation_dirty = true;
                    self.game.player_left(&player);
                }
                let _ = reply.send(());
            }
            Control::List { reply } => {
                let _ = reply.send(self.members.len());
            }
            Control::Stop => {}
        }
    }
    fn enter(&mut self, admission: Admission) {
        let Admission {
            player,
            generation,
            token_deadline,
            output,
            request_id,
            resume,
            identity,
            reply,
        } = admission;
        if !output.is_alive() || reply.is_closed() {
            let _ = reply.send(Err(Error::new(
                "CANCELLED",
                "connection closed before admission",
            )));
            return;
        }
        self.expire();
        if resume {
            match self.members.get(&player) {
                Some(member)
                    if member.generation == generation
                        && member
                            .reserved_until
                            .is_some_and(|deadline| deadline > Instant::now()) => {}
                _ => {
                    let _ = reply.send(Err(Error::new(
                        "RESUME_EXPIRED",
                        "no current reservation can be resumed",
                    )));
                    return;
                }
            }
        } else {
            if self.members.contains_key(&player) {
                let _ = reply.send(Err(Error::new(
                    "ALREADY_IN_ROOM",
                    "player already belongs to this room",
                )));
                return;
            }
            if self.members.len() >= self.capacity {
                let _ = reply.send(Err(Error::new("ROOM_FULL", "room capacity is exhausted")));
                return;
            }
        }
        // This is the admission commit point. All later delivery failures preserve the seat.
        self.observation_dirty = true;
        if resume {
            let member = self
                .members
                .get_mut(&player)
                .expect("validated reservation");
            member.reserved_until = None;
            member.output = Some(output.clone());
            member.highest_received = member.processed;
            member.pending = None;
            self.game.player_connection_changed(&player, true);
        } else {
            self.game.player_joined_with_identity(&player, &identity);
            self.members.insert(
                player.clone(),
                Member {
                    generation,
                    token_deadline,
                    reserved_until: None,
                    output: Some(output.clone()),
                    highest_received: 0,
                    processed: 0,
                    pending: None,
                    command_processed: 0,
                    command_results: VecDeque::new(),
                    rotation: None,
                },
            );
            self.ever_joined = true;
        }
        let response = self.sync(
            if resume {
                "room.resumed"
            } else {
                "room.entered"
            },
            Some(&request_id),
            &player,
        );
        // Clear any snapshot from a former room before enqueueing the full sync.
        output.latest.send_replace(None);
        if !output.send(response.clone()) {
            self.reserve(&player, generation);
        }
        let _ = reply.send(Ok(response));
    }
    fn input(&mut self, input: Input) {
        self.settle_rotations();
        let outcome = (|| {
            let member = self
                .members
                .get_mut(&input.player)
                .ok_or_else(|| Error::new("ROOM_NOT_FOUND", "player has no room membership"))?;
            if member.generation != input.generation
                || member.output.is_none()
                || member.reserved_until.is_some()
            {
                return Err(Error::new(
                    "UNAUTHENTICATED",
                    "connection generation cannot control this seat",
                ));
            }
            if member.token_deadline <= Instant::now() {
                return Err(Error::new("UNAUTHENTICATED", "guest token expired"));
            }
            if input.sequence <= member.highest_received {
                return Err(Error::new(
                    "STALE_INPUT",
                    "input_seq must strictly increase",
                ));
            }
            if input.sequence - member.highest_received > self.config.max_input_seq_jump {
                return Err(Error::new(
                    "STALE_INPUT",
                    "input_seq jump exceeds configured maximum",
                ));
            }
            self.game
                .validate_input(&input.player, &input.payload)
                .map_err(Error::from)?;
            member.highest_received = input.sequence;
            member.pending = Some(Pending {
                sequence: input.sequence,
                generation: input.generation,
                payload: input.payload,
                received: input.received,
            });
            Ok(())
        })();
        let _ = input.reply.send(outcome);
    }
    fn command(&mut self, command: GameCommand) {
        self.settle_rotations();
        let outcome = (|| {
            let member = self
                .members
                .get_mut(&command.player)
                .ok_or_else(|| Error::new("ROOM_NOT_FOUND", "player has no room membership"))?;
            if member.generation != command.generation
                || member.reserved_until.is_some()
                || !member
                    .output
                    .as_ref()
                    .is_some_and(|output| output.is_alive())
                || member.token_deadline <= Instant::now()
            {
                return Err(Error::new(
                    "UNAUTHENTICATED",
                    "connection cannot control this seat",
                ));
            }
            let body = fingerprint(&command.payload);
            if command.sequence <= member.command_processed {
                if let Some((_, previous, response)) = member
                    .command_results
                    .iter()
                    .find(|(sequence, _, _)| *sequence == command.sequence)
                {
                    if *previous != body {
                        return Err(Error::new(
                            "COMMAND_SEQUENCE_CONFLICT",
                            "command_seq already identifies another payload",
                        ));
                    }
                    let mut response = response.clone();
                    response["request_id"] = json!(command.request_id);
                    return Ok(response);
                }
                return Err(Error::new(
                    "STALE_COMMAND",
                    "command result is no longer retained",
                ));
            }
            if member.command_processed.checked_add(1) != Some(command.sequence) {
                return Err(Error::new(
                    "COMMAND_SEQUENCE_GAP",
                    "command_seq must be the next contiguous sequence",
                ));
            }
            // Validation and mutation share the room actor's serialization boundary.
            let applied = self
                .game
                .validate_command(&command.player, &command.payload)
                .and_then(|()| self.game.apply_command(&command.player, &command.payload));
            let mut response = json!({"v":1,"type":"game.command.result","request_id":command.request_id,
                "game_id":self.reference.game_id,"room_id":self.reference.id,"room_instance_id":self.reference.instance,
                "command_seq":command.sequence,"ok":applied.is_ok()});
            match applied {
                Ok(result) => response["result"] = result,
                Err(error) => {
                    response["error"] =
                        json!({"code":error.code,"message":error.message,"retryable":false})
                }
            }
            member.command_processed = command.sequence;
            if member.command_results.len() == self.config.request_cache_capacity {
                member.command_results.pop_front();
            }
            member
                .command_results
                .push_back((command.sequence, body, response.clone()));
            Ok(response)
        })();
        let _ = command.reply.send(outcome);
    }
    fn settle_rotation(&mut self, player: &str) {
        let Some(member) = self.members.get_mut(player) else {
            return;
        };
        let Some(pending) = member.rotation.as_ref() else {
            return;
        };
        match pending.transaction.outcome() {
            RotationDecision::Committed(_accepted_at) => {
                let pending = member.rotation.take().expect("committed rotation");
                member.token_deadline = pending.transaction.deadline;
                member.reserved_until = Some(pending.reservation.min(pending.transaction.deadline));
                self.game
                    .player_identity_changed(player, &pending.transaction.identity);
            }
            RotationDecision::Aborted => {
                member.rotation = None;
            }
            RotationDecision::Pending { .. } => {}
        }
    }
    fn settle_rotations(&mut self) {
        let players: Vec<_> = self
            .members
            .iter()
            .filter(|(_, member)| member.rotation.is_some())
            .map(|(player, _)| player.clone())
            .collect();
        for player in players {
            self.settle_rotation(&player);
        }
    }
    fn expire(&mut self) {
        self.settle_rotations();
        let now = Instant::now();
        let players: Vec<_> = self
            .members
            .iter()
            .filter(|(_, member)| {
                member.token_deadline <= now
                    || member
                        .reserved_until
                        .is_some_and(|deadline| deadline <= now)
            })
            .map(|(player, _)| player.clone())
            .collect();
        for player in players {
            let expired = self.members.get(&player).is_some_and(|member| {
                member.token_deadline <= now
                    || member
                        .reserved_until
                        .is_some_and(|deadline| deadline <= now)
            });
            if !expired {
                continue;
            }
            if let Some(rotation) = self
                .members
                .get(&player)
                .and_then(|member| member.rotation.as_ref())
                && !rotation.transaction.abort()
            {
                // The issuer committed while expiry examined the old token. Apply it before removal.
                self.settle_rotation(&player);
                if self.members.get(&player).is_some_and(|member| {
                    member.token_deadline > now
                        && member.reserved_until.is_none_or(|deadline| deadline > now)
                }) {
                    continue;
                }
            }
            if let Some(member) = self.members.remove(&player) {
                self.observation_dirty = true;
                if let Some(output) = member.output {
                    output.close();
                }
                self.game.player_left(&player);
            }
        }
        let disconnected: Vec<_> = self
            .members
            .iter()
            .filter(|(_, m)| m.output.as_ref().is_some_and(|o| !o.is_alive()))
            .map(|(id, m)| (id.clone(), m.generation))
            .collect();
        for (player, generation) in disconnected {
            self.reserve(&player, generation);
        }
    }
    fn step(&mut self, period: Duration) {
        self.expire();
        self.tick += 1;
        inc(&self.metrics.ticks);
        for (player, member) in &mut self.members {
            if let Some(pending) = member.pending.take() {
                // Revalidate immediately before the actual game mutation, independently of receive acceptance.
                if pending.generation == member.generation
                    && member.reserved_until.is_none()
                    && member.output.as_ref().is_some_and(|o| o.is_alive())
                {
                    let began = std::time::Instant::now();
                    self.game.apply_input(player, &pending.payload);
                    max(
                        &self.metrics.max_input_duration_micros,
                        began.elapsed().as_micros() as u64,
                    );
                    member.processed = pending.sequence;
                    inc(&self.metrics.inputs_processed);
                    max(
                        &self.metrics.max_input_latency_micros,
                        Instant::now()
                            .saturating_duration_since(pending.received)
                            .as_micros() as u64,
                    );
                }
            }
        }
        self.game.tick(period);
    }
    fn sync(&self, kind: &str, request: Option<&str>, player: &str) -> Value {
        let mut value = json!({"v":1,"type":kind,"game_id":self.reference.game_id,"room_id":self.reference.id,
            "room_instance_id":self.reference.instance,"server_tick":self.tick,"snapshot_seq":self.snapshot_seq,
            "last_processed_input_seq":self.members.get(player).map_or(0,|m|m.processed),
            "last_processed_command_seq":self.members.get(player).map_or(0,|m|m.command_processed),"state":self.game.snapshot()});
        if let Some(private) = self.game.private_snapshot(player) {
            value["private_state"] = private;
        }
        if let Some(request) = request {
            value["request_id"] = json!(request);
            value["command_results"] = json!(
                self.members
                    .get(player)
                    .map(|member| {
                        member
                            .command_results
                            .iter()
                            .map(|(_, _, response)| response)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            );
        }
        value
    }
    fn publish(&mut self) {
        self.settle_rotations();
        self.snapshot_seq += 1;
        inc(&self.metrics.snapshots);
        let state = self.game.snapshot();
        for (player, member) in &self.members {
            if let Some(output) = &member.output {
                if !output.is_alive()
                    || member.token_deadline <= Instant::now()
                    || member.reserved_until.is_some()
                {
                    continue;
                }
                let mut value = json!({"v":1,"type":"snapshot","game_id":self.reference.game_id,"room_id":self.reference.id,
                    "room_instance_id":self.reference.instance,"server_tick":self.tick,"snapshot_seq":self.snapshot_seq,
                    "last_processed_input_seq":member.processed,"last_processed_command_seq":member.command_processed,"state":state});
                if let Some(private) = self.game.private_snapshot(player) {
                    value["private_state"] = private;
                }
                output.latest.send_replace(Some(value));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::watch;

    #[derive(Default)]
    struct TestGame {
        players: BTreeMap<String, (bool, i64)>,
        applied: Vec<u64>,
        commands: Vec<Value>,
        identities: BTreeMap<String, PlayerIdentity>,
    }
    impl Game for TestGame {
        fn player_joined(&mut self, id: &str) {
            self.players.insert(id.into(), (true, 0));
        }
        fn player_left(&mut self, id: &str) {
            self.players.remove(id);
            self.identities.remove(id);
        }
        fn player_joined_with_identity(&mut self, id: &str, identity: &PlayerIdentity) {
            self.player_joined(id);
            self.identities.insert(id.into(), identity.clone());
        }
        fn player_identity_changed(&mut self, id: &str, identity: &PlayerIdentity) {
            self.identities.insert(id.into(), identity.clone());
        }
        fn player_connection_changed(&mut self, id: &str, connected: bool) {
            if let Some(p) = self.players.get_mut(id) {
                p.0 = connected;
                p.1 = 0;
            }
        }
        fn validate_input(&self, _: &str, payload: &Value) -> Result<(), crate::GameError> {
            if payload["direction"].as_i64().is_none_or(|d| d.abs() > 1) {
                Err(crate::GameError::new(
                    "INVALID_INPUT",
                    "direction out of range",
                ))
            } else {
                Ok(())
            }
        }
        fn apply_input(&mut self, id: &str, payload: &Value) {
            self.players.get_mut(id).unwrap().1 = payload["direction"].as_i64().unwrap();
            self.applied.push(payload["n"].as_u64().unwrap_or(0));
        }
        fn tick(&mut self, _: Duration) {}
        fn validate_command(&self, _: &str, payload: &Value) -> Result<(), crate::GameError> {
            if payload["reject"] == true {
                Err(crate::GameError::new("REJECTED", "rejected by validation"))
            } else {
                Ok(())
            }
        }
        fn apply_command(&mut self, _: &str, payload: &Value) -> Result<Value, crate::GameError> {
            if payload["fail"] == true {
                return Err(crate::GameError::new("APPLY_FAILED", "application failed"));
            }
            self.commands.push(payload.clone());
            Ok(json!({"count":self.commands.len()}))
        }
        fn private_snapshot(&self, player: &str) -> Option<Value> {
            match self.identities.get(player) {
                Some(PlayerIdentity::Account(identity)) => Some(identity.claims.clone()),
                _ => Some(json!({"secret":player})),
            }
        }
        fn snapshot(&self) -> Value {
            json!({"players":self.players,"applied":self.applied,"commands":self.commands})
        }
    }
    fn room() -> Room {
        let reference = RoomRef {
            game_id: "a".into(),
            id: "r".into(),
            instance: "i".into(),
        };
        let (observation, _) = watch::channel(PublishedRoom::new(&reference, true, 16));
        Room {
            reference,
            config: Config::default(),
            capacity: 16,
            game: Box::<TestGame>::default(),
            members: BTreeMap::new(),
            tick: 0,
            snapshot_seq: 0,
            ever_joined: false,
            metrics: Arc::new(Metrics::default()),
            observation,
            observation_dirty: true,
            last_observed: Instant::now(),
        }
    }
    fn output(capacity: usize) -> (Arc<Output>, mpsc::Receiver<Value>) {
        let (reliable, receiver) = mpsc::channel(capacity);
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
            receiver,
        )
    }
    fn enter(
        room: &mut Room,
        output: Arc<Output>,
        resume: bool,
        generation: u64,
    ) -> Result<Value, Error> {
        let (tx, mut rx) = oneshot::channel();
        room.enter(Admission {
            player: "p".into(),
            generation,
            token_deadline: Instant::now() + Duration::from_secs(86400),
            output,
            request_id: "join".into(),
            resume,
            identity: PlayerIdentity::Guest,
            reply: tx,
        });
        rx.try_recv().unwrap()
    }
    fn input(room: &mut Room, generation: u64, sequence: u64, direction: i64) -> Result<(), Error> {
        let (tx, mut rx) = oneshot::channel();
        room.input(Input {
            player: "p".into(),
            generation,
            sequence,
            payload: json!({"direction":direction,"n":sequence}),
            received: Instant::now(),
            reply: tx,
        });
        rx.try_recv().unwrap()
    }
    fn command(
        room: &mut Room,
        generation: u64,
        sequence: u64,
        payload: Value,
        request_id: &str,
    ) -> Result<Value, Error> {
        let (tx, mut rx) = oneshot::channel();
        room.command(GameCommand {
            player: "p".into(),
            generation,
            sequence,
            payload,
            request_id: request_id.into(),
            reply: tx,
        });
        rx.try_recv().unwrap()
    }
    #[tokio::test(start_paused = true)]
    async fn observation_keeps_expired_unprocessed_seats_without_callbacks_or_sequence_changes() {
        struct CallbackTrap;
        impl Game for CallbackTrap {
            fn player_joined(&mut self, _: &str) {
                panic!("observation called player_joined");
            }
            fn player_left(&mut self, _: &str) {
                panic!("observation called player_left");
            }
            fn player_connection_changed(&mut self, _: &str, _: bool) {
                panic!("observation called connection callback");
            }
            fn validate_input(&self, _: &str, _: &Value) -> Result<(), crate::GameError> {
                panic!("observation validated input");
            }
            fn apply_input(&mut self, _: &str, _: &Value) {
                panic!("observation applied input");
            }
            fn tick(&mut self, _: Duration) {
                panic!("observation ticked game");
            }
            fn snapshot(&self) -> Value {
                panic!("observation called game snapshot");
            }
            fn private_snapshot(&self, _: &str) -> Option<Value> {
                panic!("observation called private snapshot");
            }
        }
        let mut room = room();
        let (out, _rx) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        room.reserve("p", 1);
        room.observe_if_due();
        let receiver = room.observation.subscribe();
        room.game = Box::new(CallbackTrap);
        tokio::time::advance(room.config.reconnect_grace).await;
        room.observe_if_due();
        let summary = receiver.borrow().summary.clone();
        assert_eq!(
            (summary.connected_members, summary.reserved_members),
            (0, 1)
        );
        assert_eq!((summary.server_tick, summary.snapshot_seq), (0, 0));
        assert_eq!(room.members.len(), 1);
        assert_eq!(room.metrics.snapshot().ticks, 0);
        assert_eq!(room.metrics.snapshot().snapshots, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn commands_are_fifo_while_inputs_coalesce_and_terminal_errors_consume_sequence() {
        let mut room = room();
        let (out, _receiver) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        input(&mut room, 1, 1, 1).unwrap();
        assert!(
            command(&mut room, 1, 1, json!({"n":1}), "1").unwrap()["ok"]
                .as_bool()
                .unwrap()
        );
        input(&mut room, 1, 2, -1).unwrap();
        assert_eq!(
            command(&mut room, 1, 2, json!({"reject":true}), "2").unwrap()["error"]["code"],
            "REJECTED"
        );
        assert_eq!(
            command(&mut room, 1, 3, json!({"fail":true}), "3").unwrap()["error"]["code"],
            "APPLY_FAILED"
        );
        command(&mut room, 1, 4, json!({"n":4}), "4").unwrap();
        room.step(Duration::from_millis(33));
        assert_eq!(room.game.snapshot()["applied"], json!([2]));
        assert_eq!(room.game.snapshot()["commands"], json!([{"n":1},{"n":4}]));
        assert_eq!(room.members["p"].command_processed, 4);
    }
    #[tokio::test(start_paused = true)]
    async fn command_replay_conflict_gap_eviction_and_generation_fences_never_reexecute() {
        let mut room = room();
        room.config.request_cache_capacity = 2;
        let (out, _receiver) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        assert_eq!(
            command(&mut room, 1, 0, json!({}), "zero")
                .unwrap_err()
                .code,
            "STALE_COMMAND"
        );
        assert_eq!(
            command(&mut room, 1, 2, json!({}), "gap").unwrap_err().code,
            "COMMAND_SEQUENCE_GAP"
        );
        let first = command(&mut room, 1, 1, json!({"n":1}), "old").unwrap();
        let replay = command(&mut room, 1, 1, json!({"n":1}), "new").unwrap();
        assert_eq!(first["result"], replay["result"]);
        assert_eq!(replay["request_id"], "new");
        assert_eq!(
            command(&mut room, 1, 1, json!({"n":9}), "conflict")
                .unwrap_err()
                .code,
            "COMMAND_SEQUENCE_CONFLICT"
        );
        command(&mut room, 1, 2, json!({"n":2}), "two").unwrap();
        command(&mut room, 1, 3, json!({"n":3}), "three").unwrap();
        assert_eq!(
            command(&mut room, 1, 1, json!({"n":1}), "evicted")
                .unwrap_err()
                .code,
            "STALE_COMMAND"
        );
        let (tx, _rx) = oneshot::channel();
        room.control(Control::Replace {
            player: "p".into(),
            generation: 2,
            rotation: None,
            reply: tx,
        });
        assert_eq!(
            command(&mut room, 1, 4, json!({"n":4}), "stale")
                .unwrap_err()
                .code,
            "UNAUTHENTICATED"
        );
        let (out, _receiver) = output(16);
        let full = enter(&mut room, out, true, 2).unwrap();
        assert_eq!(full["last_processed_command_seq"], 3);
        assert_eq!(full["command_results"].as_array().unwrap().len(), 2);
        assert_eq!(
            command(&mut room, 2, 3, json!({"n":3}), "resume").unwrap()["result"]["count"],
            3
        );
        assert_eq!(
            room.game.snapshot()["commands"].as_array().unwrap().len(),
            3
        );
    }
    #[tokio::test(start_paused = true)]
    async fn command_result_survives_reply_loss_and_expiry_prevents_replay() {
        let mut room = room();
        let (out, _receiver) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        let (tx, rx) = oneshot::channel();
        drop(rx);
        room.command(GameCommand {
            player: "p".into(),
            generation: 1,
            sequence: 1,
            payload: json!({"n":1}),
            request_id: "lost".into(),
            reply: tx,
        });
        assert_eq!(
            command(&mut room, 1, 1, json!({"n":1}), "retry").unwrap()["result"]["count"],
            1
        );
        room.members.get_mut("p").unwrap().token_deadline = Instant::now();
        assert_eq!(
            command(&mut room, 1, 1, json!({"n":1}), "expired")
                .unwrap_err()
                .code,
            "UNAUTHENTICATED"
        );
        assert_eq!(
            room.game.snapshot()["commands"].as_array().unwrap().len(),
            1
        );
    }
    #[tokio::test(start_paused = true)]
    async fn private_state_is_recipient_scoped_for_initial_periodic_and_resume() {
        let mut room = room();
        let (out, _receiver) = output(16);
        let first = enter(&mut room, out.clone(), false, 1).unwrap();
        let (peer, _peer_receiver) = output(16);
        let (tx, mut rx) = oneshot::channel();
        room.enter(Admission {
            player: "peer".into(),
            generation: 1,
            token_deadline: Instant::now() + Duration::from_secs(60),
            output: peer.clone(),
            request_id: "peer".into(),
            resume: false,
            identity: PlayerIdentity::Guest,
            reply: tx,
        });
        let second = rx.try_recv().unwrap().unwrap();
        assert_eq!(first["private_state"], json!({"secret":"p"}));
        assert_eq!(second["private_state"], json!({"secret":"peer"}));
        room.publish();
        assert_eq!(
            out.latest.borrow().as_ref().unwrap()["private_state"],
            first["private_state"]
        );
        assert_eq!(
            peer.latest.borrow().as_ref().unwrap()["private_state"],
            second["private_state"]
        );
        assert!(
            out.latest
                .borrow()
                .as_ref()
                .unwrap()
                .get("command_results")
                .is_none()
        );
        room.reserve("p", 1);
        let (resumed, _receiver) = output(16);
        assert_eq!(
            enter(&mut room, resumed, true, 1).unwrap()["private_state"],
            first["private_state"]
        );
    }
    #[tokio::test(start_paused = true)]
    async fn full_sync_and_snapshot_include_independent_command_ack() {
        let mut room = room();
        let (out, _receiver) = output(8);
        let sync = enter(&mut room, out.clone(), false, 1).unwrap();
        assert_eq!(sync["last_processed_command_seq"], 0);
        assert_eq!(sync["command_results"], json!([]));
        room.publish();
        assert_eq!(
            out.latest.borrow().as_ref().unwrap()["last_processed_command_seq"],
            0
        );
    }
    #[tokio::test(start_paused = true)]
    async fn cancellation_before_admission_has_no_membership_or_game_side_effect() {
        let mut room = room();
        let (out, _receiver) = output(4);
        out.close();
        assert_eq!(
            enter(&mut room, out, false, 1).unwrap_err().code,
            "CANCELLED"
        );
        assert!(room.members.is_empty());
        assert_eq!(room.game.snapshot()["players"], json!({}));
        assert!(!room.ever_joined);
        let (out, _receiver) = output(4);
        let (tx, rx) = oneshot::channel();
        drop(rx);
        room.enter(Admission {
            player: "p".into(),
            generation: 1,
            token_deadline: Instant::now() + Duration::from_secs(60),
            output: out,
            request_id: "cancel".into(),
            resume: false,
            identity: PlayerIdentity::Guest,
            reply: tx,
        });
        assert!(room.members.is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn response_failure_after_commit_preserves_resumable_reservation() {
        let mut room = room();
        let (out, mut receiver) = output(1);
        out.send(json!({"fill":true}));
        assert!(enter(&mut room, out.clone(), false, 1).is_ok());
        assert!(!out.is_alive());
        assert_eq!(room.members.len(), 1);
        assert_eq!(
            room.members["p"].reserved_until,
            Some(Instant::now() + room.config.reconnect_grace)
        );
        receiver.try_recv().unwrap();
        let (tx, mut rx) = oneshot::channel();
        room.control(Control::Replace {
            player: "p".into(),
            generation: 2,
            rotation: None,
            reply: tx,
        });
        assert!(rx.try_recv().unwrap().is_some());
        let (out, mut receiver) = output(4);
        let sync = enter(&mut room, out, true, 2).unwrap();
        assert_eq!(sync["type"], "room.resumed");
        assert_eq!(receiver.try_recv().unwrap(), sync);
    }
    #[tokio::test(start_paused = true)]
    async fn replacement_discards_unprocessed_old_generation_and_reuses_processed_sequence() {
        let mut room = room();
        let (out, _receiver) = output(8);
        enter(&mut room, out.clone(), false, 1).unwrap();
        input(&mut room, 1, 1, 1).unwrap();
        room.step(Duration::from_millis(33));
        assert_eq!(room.members["p"].processed, 1);
        input(&mut room, 1, 2, -1).unwrap();
        input(&mut room, 1, 3, -1).unwrap();
        out.close();
        let (tx, mut rx) = oneshot::channel();
        room.control(Control::Replace {
            player: "p".into(),
            generation: 2,
            rotation: None,
            reply: tx,
        });
        assert!(rx.try_recv().unwrap().is_some());
        let (new_out, _new_receiver) = output(8);
        let full = enter(&mut room, new_out, true, 2).unwrap();
        assert_eq!(full["last_processed_input_seq"], 1);
        assert_eq!(room.members["p"].highest_received, 1);
        assert!(room.members["p"].pending.is_none());
        assert_eq!(
            input(&mut room, 1, 2, -1).unwrap_err().code,
            "UNAUTHENTICATED"
        );
        input(&mut room, 2, 2, 1).unwrap();
        room.step(Duration::from_millis(33));
        assert_eq!(room.members["p"].processed, 2);
        assert_eq!(room.game.snapshot()["applied"], json!([1, 2]));
    }
    #[tokio::test(start_paused = true)]
    async fn replacement_does_not_extend_existing_grace_and_old_close_cannot_disconnect_resume() {
        let mut room = room();
        let (out, _receiver) = output(8);
        enter(&mut room, out, false, 1).unwrap();
        room.reserve("p", 1);
        let deadline = room.members["p"].reserved_until.unwrap();
        tokio::time::advance(Duration::from_secs(10)).await;
        let (tx, _) = oneshot::channel();
        room.control(Control::Replace {
            player: "p".into(),
            generation: 2,
            rotation: None,
            reply: tx,
        });
        assert_eq!(room.members["p"].reserved_until, Some(deadline));
        let (out, _receiver) = output(8);
        enter(&mut room, out, true, 2).unwrap();
        room.reserve("p", 1);
        assert!(room.members["p"].reserved_until.is_none());
        assert!(room.members["p"].output.is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn account_rotation_refreshes_token_deadline_without_reusing_older_token_lifetime() {
        let mut room = room();
        let (out, _receiver) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        room.members.get_mut("p").unwrap().token_deadline = Instant::now() + Duration::from_secs(1);
        let (tx, mut rx) = oneshot::channel();
        room.control(Control::Replace {
            player: "p".into(),
            generation: 2,
            rotation: Some(Rotation::new(
                Instant::now() + Duration::from_secs(60),
                PlayerIdentity::Guest,
            )),
            reply: tx,
        });
        assert_eq!(
            rx.try_recv().unwrap(),
            Some(Instant::now() + room.config.reconnect_grace)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn cancelled_short_grant_preparation_does_not_shorten_real_actor_reservation() {
        let (control, controls) = mpsc::channel(16);
        let (_inputs, input_rx) = mpsc::channel(16);
        let (_commands, command_rx) = mpsc::channel(16);
        let actor = tokio::spawn(run(
            room().reference,
            Config::default(),
            16,
            Box::<TestGame>::default(),
            Arc::new(Metrics::default()),
            Channels {
                observation: room().observation,
                controls,
                inputs: input_rx,
                commands: command_rx,
            },
        ));
        let (out, _rx) = output(16);
        let (tx, rx) = oneshot::channel();
        control
            .send(Control::Enter(Admission {
                player: "p".into(),
                generation: 1,
                token_deadline: Instant::now() + Duration::from_secs(60),
                output: out,
                request_id: "join".into(),
                resume: false,
                identity: PlayerIdentity::Guest,
                reply: tx,
            }))
            .await
            .unwrap_or_else(|_| panic!("enter"));
        rx.await.unwrap().unwrap();
        let (peer, _peer_rx) = output(16);
        let (tx, rx) = oneshot::channel();
        control
            .send(Control::Enter(Admission {
                player: "peer".into(),
                generation: 1,
                token_deadline: Instant::now() + Duration::from_secs(3600),
                output: peer,
                request_id: "peer".into(),
                resume: false,
                identity: PlayerIdentity::Guest,
                reply: tx,
            }))
            .await
            .unwrap_or_else(|_| panic!("peer"));
        rx.await.unwrap().unwrap();
        let (tx, rx) = oneshot::channel();
        control
            .send(Control::Disconnect {
                player: "p".into(),
                generation: 1,
                reply: tx,
            })
            .await
            .unwrap_or_else(|_| panic!("disconnect"));
        rx.await.unwrap();
        let (tx, rx) = oneshot::channel();
        let transaction = Rotation::new(
            Instant::now() + Duration::from_secs(1),
            PlayerIdentity::Guest,
        );
        control
            .send(Control::Replace {
                player: "p".into(),
                generation: 2,
                rotation: Some(transaction.clone()),
                reply: tx,
            })
            .await
            .unwrap_or_else(|_| panic!("prepare"));
        rx.await.unwrap();
        // No commit follows: the requesting HTTP future was cancelled after preparation.
        assert!(transaction.abort());
        tokio::time::advance(Duration::from_secs(2)).await;
        let (out, _resumed_rx) = output(16);
        let (tx, rx) = oneshot::channel();
        control
            .send(Control::Enter(Admission {
                player: "p".into(),
                generation: 2,
                token_deadline: Instant::now() + Duration::from_secs(58),
                output: out,
                request_id: "resume".into(),
                resume: true,
                identity: PlayerIdentity::Guest,
                reply: tx,
            }))
            .await
            .unwrap_or_else(|_| panic!("resume"));
        assert!(
            rx.await.unwrap().is_ok(),
            "cancelled grant must preserve the old reservation"
        );
        control
            .send(Control::Stop)
            .await
            .unwrap_or_else(|_| panic!("stop"));
        actor.await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn pending_rotation_cannot_commit_after_room_is_dropped() {
        let mut room = room();
        let (out, _rx) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        let transaction = Rotation::new(
            Instant::now() + Duration::from_secs(60),
            PlayerIdentity::Guest,
        );
        let (tx, mut rx) = oneshot::channel();
        room.control(Control::Replace {
            player: "p".into(),
            generation: 2,
            rotation: Some(transaction.clone()),
            reply: tx,
        });
        assert!(rx.try_recv().unwrap().is_some());
        drop(room);
        assert!(!transaction.commit());
    }
    #[tokio::test(start_paused = true)]
    async fn prepared_rotation_keeps_old_identity_and_deadline_until_commit() {
        let mut room = room();
        let (out, _rx) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        let old_deadline = room.members["p"].token_deadline;
        let new_deadline = Instant::now() + Duration::from_secs(1);
        let transaction = Rotation::new(
            new_deadline,
            PlayerIdentity::Account(crate::AccountIdentity {
                issuer: "issuer".into(),
                subject: "subject".into(),
                claims: json!({"updated":"private"}),
            }),
        );
        let (tx, mut rx) = oneshot::channel();
        room.control(Control::Replace {
            player: "p".into(),
            generation: 2,
            rotation: Some(transaction.clone()),
            reply: tx,
        });
        assert_eq!(rx.try_recv().unwrap(), Some(new_deadline));
        assert_eq!(room.members["p"].token_deadline, old_deadline);
        assert_eq!(room.game.private_snapshot("p"), Some(json!({"secret":"p"})));
        assert!(transaction.commit());
        room.settle_rotations();
        assert_eq!(room.members["p"].token_deadline, new_deadline);
        assert_eq!(
            room.game.private_snapshot("p"),
            Some(json!({"updated":"private"}))
        );
    }
    #[tokio::test(start_paused = true)]
    async fn queued_old_generation_commands_are_fenced_by_control_barrier() {
        let reference = room().reference;
        let (control, controls) = mpsc::channel(8);
        let (_inputs, input_rx) = mpsc::channel(8);
        let (commands, command_rx) = mpsc::channel(8);
        let (out, _rx) = output(16);
        let (joined, joined_rx) = oneshot::channel();
        control
            .try_send(Control::Enter(Admission {
                player: "p".into(),
                generation: 1,
                token_deadline: Instant::now() + Duration::from_secs(60),
                output: out,
                request_id: "join".into(),
                resume: false,
                identity: PlayerIdentity::Guest,
                reply: joined,
            }))
            .unwrap_or_else(|_| panic!("admission queue"));
        let actor = tokio::spawn(run(
            reference,
            Config::default(),
            16,
            Box::<TestGame>::default(),
            Arc::new(Metrics::default()),
            Channels {
                observation: room().observation,
                controls,
                inputs: input_rx,
                commands: command_rx,
            },
        ));
        joined_rx.await.unwrap().unwrap();
        let (old_result, old_rx) = oneshot::channel();
        commands
            .try_send(GameCommand {
                player: "p".into(),
                generation: 1,
                sequence: 1,
                payload: json!({"n":1}),
                request_id: "old".into(),
                reply: old_result,
            })
            .unwrap_or_else(|_| panic!("command queue"));
        let (replaced, replaced_rx) = oneshot::channel();
        control
            .try_send(Control::Replace {
                player: "p".into(),
                generation: 2,
                rotation: None,
                reply: replaced,
            })
            .unwrap_or_else(|_| panic!("control queue"));
        replaced_rx.await.unwrap();
        assert_eq!(old_rx.await.unwrap().unwrap_err().code, "UNAUTHENTICATED");
        let (out, _rx) = output(16);
        let (resumed, resumed_rx) = oneshot::channel();
        control
            .send(Control::Enter(Admission {
                player: "p".into(),
                generation: 2,
                token_deadline: Instant::now() + Duration::from_secs(60),
                output: out,
                request_id: "resume".into(),
                resume: true,
                identity: PlayerIdentity::Guest,
                reply: resumed,
            }))
            .await
            .unwrap_or_else(|_| panic!("resume queue"));
        resumed_rx.await.unwrap().unwrap();
        let (new_result, new_rx) = oneshot::channel();
        commands
            .send(GameCommand {
                player: "p".into(),
                generation: 2,
                sequence: 1,
                payload: json!({"n":1}),
                request_id: "new".into(),
                reply: new_result,
            })
            .await
            .unwrap_or_else(|_| panic!("new queue"));
        assert_eq!(new_rx.await.unwrap().unwrap()["result"]["count"], 1);
        control
            .send(Control::Stop)
            .await
            .unwrap_or_else(|_| panic!("stop queue"));
        assert_eq!(actor.await.unwrap(), "shutdown");
    }
    #[tokio::test(start_paused = true)]
    async fn newest_valid_input_is_applied_once_per_tick_and_invalid_does_not_advance_ack() {
        let mut room = room();
        let (out, _receiver) = output(16);
        enter(&mut room, out, false, 1).unwrap();
        input(&mut room, 1, 1, 1).unwrap();
        input(&mut room, 1, 2, -1).unwrap();
        assert_eq!(input(&mut room, 1, 3, 2).unwrap_err().code, "INVALID_INPUT");
        assert_eq!(room.members["p"].processed, 0);
        assert_eq!(room.members["p"].highest_received, 2);
        room.step(Duration::from_millis(33));
        assert_eq!(room.members["p"].processed, 2);
        assert_eq!(room.game.snapshot()["applied"], json!([2]));
        assert_eq!(input(&mut room, 1, 2, 1).unwrap_err().code, "STALE_INPUT");
    }
    #[tokio::test(start_paused = true)]
    async fn admission_full_sync_is_reliable_before_latest_snapshot_and_expiry_releases_seat() {
        let mut room = room();
        let (out, mut receiver) = output(8);
        let sync = enter(&mut room, out.clone(), false, 1).unwrap();
        assert_eq!(receiver.try_recv().unwrap(), sync);
        assert!(out.latest.borrow().is_none());
        room.step(Duration::from_millis(33));
        room.publish();
        assert!(
            out.latest.borrow().as_ref().unwrap()["snapshot_seq"]
                .as_u64()
                .unwrap()
                > sync["snapshot_seq"].as_u64().unwrap()
        );
        room.reserve("p", 1);
        tokio::time::advance(room.config.reconnect_grace).await;
        room.expire();
        assert!(room.members.is_empty());
        assert_eq!(room.game.snapshot()["players"], json!({}));
    }
    #[tokio::test(start_paused = true)]
    async fn same_manual_clock_trace_replays_tick_ack_expiry_and_state_deterministically() {
        async fn replay() -> Value {
            let mut room = room();
            let (out, _receiver) = output(32);
            enter(&mut room, out, false, 1).unwrap();
            for tick in 1..=8 {
                input(&mut room, 1, tick, if tick % 2 == 0 { -1 } else { 1 }).unwrap();
                tokio::time::advance(Duration::from_millis(33)).await;
                room.step(Duration::from_millis(33));
                if tick % 2 == 0 {
                    room.publish();
                }
            }
            let before = json!({"state":room.game.snapshot(),"tick":room.tick,"snapshots":room.snapshot_seq,"ack":room.members["p"].processed});
            room.reserve("p", 1);
            tokio::time::advance(room.config.reconnect_grace).await;
            room.expire();
            json!({"before_expiry":before,"members_after_expiry":room.members.len(),"state_after_expiry":room.game.snapshot()})
        }
        let first = replay().await;
        let second = replay().await;
        assert_eq!(first, second);
        assert_eq!(first["before_expiry"]["tick"], 8);
        assert_eq!(first["before_expiry"]["ack"], 8);
        assert_eq!(first["members_after_expiry"], 0);
    }
}
