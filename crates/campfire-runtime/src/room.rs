use crate::{
    Config, Game,
    metrics::{Metrics, inc, max},
    output::Output,
    protocol::Error,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot},
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
pub(crate) enum Control {
    Enter(Admission),
    Replace {
        player: String,
        generation: u64,
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
}

pub(crate) async fn run(
    reference: RoomRef,
    config: Config,
    capacity: usize,
    game: Box<dyn Game>,
    metrics: Arc<Metrics>,
    mut controls: mpsc::Receiver<Control>,
    mut inputs: mpsc::Receiver<Input>,
) -> &'static str {
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
    };
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
            input=inputs.recv()=> {if let Some(input)=input {
                let began=std::time::Instant::now();room.input(input);
                max(&room.metrics.max_input_duration_micros,began.elapsed().as_micros() as u64);
            } }
        }
        if room.ever_joined && room.members.is_empty() {
            return "empty";
        }
    }
}
impl Room {
    fn reserve(&mut self, player: &str, generation: u64) {
        if let Some(member) = self.members.get_mut(player) {
            if member.generation != generation {
                return;
            }
            member.pending = None;
            member.highest_received = member.processed;
            member.output = None;
            member.reserved_until.get_or_insert_with(|| {
                (Instant::now() + self.config.reconnect_grace).min(member.token_deadline)
            });
            self.game.player_connection_changed(player, false);
        }
    }
    fn control(&mut self, command: Control) {
        match command {
            Control::Enter(admission) => {
                self.enter(admission);
            }
            Control::Replace {
                player,
                generation,
                reply,
            } => {
                let old = self.members.get(&player).map(|m| m.generation);
                if let Some(old) = old {
                    self.reserve(&player, old);
                    if let Some(member) = self.members.get_mut(&player) {
                        member.generation = generation;
                    }
                }
                let _ = reply.send(self.members.get(&player).and_then(|m| m.reserved_until));
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
                    self.members.remove(&player);
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
            self.game.player_joined(&player);
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
    fn expire(&mut self) {
        let now = Instant::now();
        let expired: Vec<_> = self
            .members
            .iter()
            .filter(|(_, m)| m.token_deadline <= now || m.reserved_until.is_some_and(|d| d <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for player in expired {
            if let Some(member) = self.members.remove(&player) {
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
            "last_processed_input_seq":self.members.get(player).map_or(0,|m|m.processed),"state":self.game.snapshot()});
        if let Some(request) = request {
            value["request_id"] = json!(request);
        }
        value
    }
    fn publish(&mut self) {
        self.snapshot_seq += 1;
        inc(&self.metrics.snapshots);
        let state = self.game.snapshot();
        for member in self.members.values() {
            if let Some(output) = &member.output {
                output.latest.send_replace(Some(json!({"v":1,"type":"snapshot","game_id":self.reference.game_id,"room_id":self.reference.id,
                    "room_instance_id":self.reference.instance,"server_tick":self.tick,"snapshot_seq":self.snapshot_seq,
                    "last_processed_input_seq":member.processed,"state":state})));
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
    }
    impl Game for TestGame {
        fn player_joined(&mut self, id: &str) {
            self.players.insert(id.into(), (true, 0));
        }
        fn player_left(&mut self, id: &str) {
            self.players.remove(id);
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
        fn snapshot(&self) -> Value {
            json!({"players":self.players,"applied":self.applied})
        }
    }
    fn room() -> Room {
        Room {
            reference: RoomRef {
                game_id: "a".into(),
                id: "r".into(),
                instance: "i".into(),
            },
            config: Config::default(),
            capacity: 16,
            game: Box::<TestGame>::default(),
            members: BTreeMap::new(),
            tick: 0,
            snapshot_seq: 0,
            ever_joined: false,
            metrics: Arc::new(Metrics::default()),
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
