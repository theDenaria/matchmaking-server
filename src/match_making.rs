use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum PoolType {
    NormalOneVsOne,
    NormalTwoVsTwo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MatchmakingCommand {
    // AddPlayer(PoolType, String, std::sync::mpsc::Sender<MatchmakingEvent>),
    RemovePlayer(PoolType, String),
    CreateMatch(PoolType, Vec<String>),
    SetPlayerReady(u32, String),
    RemoveMatch(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MatchmakingEvent {
    MatchFound(MatchFoundInfo),
    PlayerReady(String),
    MatchStart(MatchStartInfo),
    MatchNotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerQueue {
    pub player_id: String,
}

#[derive(Debug, Clone)]
pub struct PlayerMatchInfo {
    pub player_id: String,
    pub ready: bool,
    pub team: u8,
}

#[derive(Debug, Clone)]
pub struct Match {
    pub session_id: u32,
    pub players_match_info_map: HashMap<String, PlayerMatchInfo>,
    pub remaining_ready: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchFoundInfo {
    pub session_id: u32,
    pub players: Vec<MatchFoundPlayerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchFoundPlayerInfo {
    pub player_id: String,
    pub team: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchStartInfo {
    pub session_id: u32,
    pub tps_server_address: String,
    pub tps_server_port: u16,
}

pub struct MatchmakingState {
    next_session_id: u32,
    queues: HashMap<PoolType, VecDeque<PlayerQueue>>,
    matches: HashMap<u32, Match>,
    command_tx: mpsc::Sender<MatchmakingCommand>,
    player_event_sender_map: HashMap<String, mpsc::Sender<MatchmakingEvent>>,
}

impl MatchmakingState {
    pub fn new() -> (Arc<RwLock<Self>>, mpsc::Receiver<MatchmakingCommand>) {
        let (command_tx, command_rx) = mpsc::channel(100);
        let state = Arc::new(RwLock::new(MatchmakingState {
            queues: HashMap::new(),
            matches: HashMap::new(),
            command_tx,
            player_event_sender_map: HashMap::new(),
            next_session_id: 0,
        }));
        (state, command_rx)
    }

    pub async fn add_player(
        &mut self,
        pool_type: PoolType,
        player_id: String,
        event_sender: mpsc::Sender<MatchmakingEvent>,
    ) {
        self.queues
            .entry(pool_type.clone())
            .or_default()
            .push_back(PlayerQueue {
                player_id: player_id.clone(),
            });
        self.player_event_sender_map
            .insert(player_id.clone(), event_sender);

        tracing::debug!("Player {} added to pool {:?}", player_id, pool_type);
    }

    pub async fn remove_player(&self, pool_type: PoolType, player_id: String) {
        self.command_tx
            .send(MatchmakingCommand::RemovePlayer(pool_type, player_id))
            .await
            .unwrap();
    }

    async fn create_match(&self, pool_type: PoolType, players: Vec<String>) {
        self.command_tx
            .send(MatchmakingCommand::CreateMatch(pool_type, players))
            .await
            .unwrap();
    }

    pub async fn set_player_ready(&self, session_id: u32, player_id: String) {
        self.command_tx
            .send(MatchmakingCommand::SetPlayerReady(session_id, player_id))
            .await
            .unwrap();
    }

    pub async fn remove_match(&self, session_id: u32) {
        self.command_tx
            .send(MatchmakingCommand::RemoveMatch(session_id))
            .await
            .unwrap();
    }

    pub async fn start_matchmaking_loop(
        state: Arc<RwLock<Self>>,
        mut command_rx: mpsc::Receiver<MatchmakingCommand>,
    ) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        tracing::debug!("In start matchmaking loop");
        loop {
            tokio::select! {
                Some(command) = command_rx.recv() => {
                    tracing::debug!("Received command: {:?}", command);
                    let mut state = state.write().await;
                    state.handle_command(command).await;
                }
                _ = interval.tick() => {
                    let mut state = state.write().await;
                    state.process_matchmaking().await;
                }
            }
        }
    }

    async fn handle_command(&mut self, command: MatchmakingCommand) {
        match command {
            MatchmakingCommand::RemovePlayer(pool_type, player_id) => {
                if let Some(queue) = self.queues.get_mut(&pool_type) {
                    queue.retain(|p| p.player_id != player_id);
                }
                tracing::trace!("Player {} removed from pool {:?}", player_id, pool_type);
            }
            // Called by process_matchmaking internally when a match is found
            MatchmakingCommand::CreateMatch(pool_type, players) => {
                let session_id = self.next_session_id;
                self.next_session_id += 1;
                let match_data = Match {
                    session_id: session_id.clone(),
                    players_match_info_map: players
                        .iter()
                        .map(|player_id| {
                            (
                                player_id.clone(),
                                PlayerMatchInfo {
                                    player_id: player_id.clone(),
                                    ready: false,
                                    team: 0,
                                },
                            )
                        })
                        .collect(),
                    remaining_ready: players.len() as u8,
                };
                self.matches.insert(session_id.clone(), match_data);
                if let Some(queue) = self.queues.get_mut(&pool_type) {
                    for player_id in players.iter() {
                        queue.retain(|p| p.player_id != *player_id);
                    }
                }
                for player_id in players.iter() {
                    self.player_event_sender_map
                        .get(player_id)
                        .unwrap()
                        .send(MatchmakingEvent::MatchFound(MatchFoundInfo {
                            session_id: session_id.clone(),
                            players: players
                                .iter()
                                .map(|player_id| MatchFoundPlayerInfo {
                                    player_id: player_id.clone(),
                                    team: 0,
                                })
                                .collect(),
                        }))
                        .await
                        .unwrap();
                }
                tracing::trace!(
                    "Match created with session_id: {} and players: {:?}",
                    session_id,
                    players
                );
            }
            MatchmakingCommand::SetPlayerReady(session_id, player_id) => {
                if let Some(match_data) = self.matches.get_mut(&session_id) {
                    if let Some(player_match_info) =
                        match_data.players_match_info_map.get_mut(&player_id)
                    {
                        if !player_match_info.ready {
                            player_match_info.ready = true;
                            match_data.remaining_ready -= 1;
                        }

                        if match_data.remaining_ready == 0 {
                            if !send_create_session_request(
                                session_id,
                                match_data.players_match_info_map.keys().cloned().collect(),
                            )
                            .await
                            {
                                tracing::error!(
                                    "Failed to create session for match: {}",
                                    session_id
                                );
                                return;
                            }
                        }

                        for player_match_info in match_data.players_match_info_map.values() {
                            self.player_event_sender_map
                                .get(&player_match_info.player_id)
                                .unwrap()
                                .send(MatchmakingEvent::PlayerReady(player_id.clone()))
                                .await
                                .unwrap();
                            if match_data.remaining_ready == 0 {
                                self.player_event_sender_map
                                    .get(&player_match_info.player_id)
                                    .unwrap()
                                    .send(MatchmakingEvent::MatchStart(MatchStartInfo {
                                        session_id: session_id.clone(),
                                        tps_server_address: "192.168.1.151".to_string(),
                                        tps_server_port: 5001,
                                    }))
                                    .await
                                    .unwrap();
                                tracing::debug!("All players Ready!")
                            }
                        }
                    }
                }
            }
            MatchmakingCommand::RemoveMatch(session_id) => {
                self.matches.remove(&session_id);
            }
        }
    }

    async fn process_matchmaking(&mut self) {
        for pool_type in [PoolType::NormalOneVsOne, PoolType::NormalTwoVsTwo].iter() {
            match pool_type {
                PoolType::NormalOneVsOne => self.process_1v1_matchmaking(pool_type).await,
                PoolType::NormalTwoVsTwo => self.process_2v2_matchmaking(pool_type).await,
            }
        }
    }

    async fn process_1v1_matchmaking(&mut self, pool_type: &PoolType) {
        while let Some(queue) = self.queues.get_mut(pool_type) {
            if queue.len() < 2 {
                break;
            }
            let players: Vec<String> = (0..2)
                .map(|_| queue.pop_front().unwrap().player_id)
                .collect();
            tracing::debug!("Creating match with players: {:?}", players);
            self.create_match(pool_type.clone(), players).await;
        }
    }

    async fn process_2v2_matchmaking(&mut self, pool_type: &PoolType) {
        while let Some(queue) = self.queues.get_mut(pool_type) {
            if queue.len() < 4 {
                break;
            }
            let players: Vec<String> = (0..4)
                .map(|_| queue.pop_front().unwrap().player_id)
                .collect();
            self.create_match(pool_type.clone(), players).await;
        }
    }
}

async fn send_create_session_request(session_id: u32, player_ids: Vec<String>) -> bool {
    let client = reqwest::Client::new();
    let playfab_api_url = std::env::var("SESSION_API_URL").unwrap();

    let header: u8 = 100;
    let client_identifier = 1;

    let response = client
        .post(format!("{}/create_session", playfab_api_url))
        .json(&serde_json::json!({
            "header": header,
            "client_identifier": client_identifier,
            "session_id": session_id,
            "player_ids": player_ids,
        }))
        .send()
        .await;
    match response {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<serde_json::Value>().await {
                    Ok(response_body) => {
                        tracing::debug!("Session created: {}", response_body);
                        return true;
                    }
                    Err(e) => {
                        tracing::error!("Failed to create session: {}", e);
                        return false;
                    }
                }
            } else {
                tracing::error!("Failed to create session: {}", response.status());
                return false;
            }
        }
        Err(e) => {
            tracing::error!("Failed to create session: {}", e);
            return false;
        }
    }
}
