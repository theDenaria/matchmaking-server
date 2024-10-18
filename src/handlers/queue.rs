use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::{
    match_making::{MatchmakingEvent, MatchmakingState, PoolType},
    AppState,
};

pub async fn queue(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.matchmaking_state.clone()))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<RwLock<MatchmakingState>>) {
    if let Some(Ok(Message::Text(message))) = socket.next().await {
        let (op_code, remaining) = parse_message(&message);
        match op_code.as_str() {
            "JOIN_QUEUE" => {
                let (player_id, remaining) = parse_message(&remaining);
                let (pool_type_str, session_ticket) = parse_message(&remaining);

                let is_authenticated = authenticate_player(session_ticket).await;

                if !is_authenticated {
                    tracing::error!("Player {} is not authenticated", player_id);
                    let response_msg = Message::Text(String::from("AUTHENTICATION_FAILED"));
                    if socket.send(response_msg).await.is_err() {
                        tracing::error!(
                            "Failed to send AUTHENTICATION_FAILED message to player {}",
                            player_id
                        );
                    }
                    socket.close().await.unwrap();
                    return;
                }

                let pool_type = match pool_type_str.as_str() {
                    "1v1" => PoolType::NormalOneVsOne,
                    "2v2" => PoolType::NormalTwoVsTwo,
                    _ => {
                        tracing::error!("Invalid pool type: {}", pool_type_str);
                        return;
                    }
                };

                let (player_event_sender, mut player_event_receiver) =
                    mpsc::channel::<MatchmakingEvent>(10);

                tracing::info!("Player {} joining the {} queue", player_id, pool_type_str);

                state
                    .write()
                    .await
                    .add_player(pool_type.clone(), player_id.clone(), player_event_sender)
                    .await;

                let response_msg = Message::Text(String::from("QUEUE_JOINED"));
                if socket.send(response_msg).await.is_err() {
                    tracing::error!(
                        "Failed to send QUEUE_JOINED message to player {}",
                        player_id
                    );
                }

                loop {
                    tokio::select! {
                        Some(msg) = socket.next() => {
                            if let Ok(Message::Text(text)) = msg {
                                let (op_code, remaining) = parse_message(&text);
                                match op_code.as_str() {
                                    "LEAVE_QUEUE" => {
                                        state
                                            .read()
                                            .await
                                        .remove_player(pool_type.clone(), player_id.clone())
                                        .await;
                                        break;
                                    }
                                    "PLAYER_READY" => {
                                        let (session_id, _) = parse_message(&remaining);
                                        state
                                            .read()
                                            .await
                                            .set_player_ready(session_id.parse::<u32>().unwrap(), player_id.clone())
                                            .await;
                                    }
                                    _ => {
                                        tracing::error!("Invalid op code: {}", op_code);
                                    }
                                }
                            } else {
                                break;
                            }
                        }
                        Some(event) = player_event_receiver.recv() => {
                            match event {
                                MatchmakingEvent::MatchFound(match_info) => {
                                    let match_info_string = serde_json::to_string(&match_info).unwrap();
                                    let match_found_msg = Message::Text(format!("MATCH_FOUND,{}", match_info_string));

                                    if socket.send(match_found_msg).await.is_err() {
                                        tracing::error!("Failed to send MATCH_FOUND message to player {}", player_id);
                                        break;
                                    }
                                }
                                MatchmakingEvent::PlayerReady(ready_player_id) => {
                                    let player_ready_msg = Message::Text(format!("PLAYER_READY,{}", ready_player_id));
                                    tracing::debug!("Sending {},PLAYER_READY message to player {}", ready_player_id, player_id);
                                    if socket.send(player_ready_msg).await.is_err() {
                                        tracing::error!("Failed to send PLAYER_READY message to player {}", player_id);
                                        break;
                                    }
                                }
                                MatchmakingEvent::MatchStart(match_start_info) => {
                                    let match_start_msg = Message::Text(format!("MATCH_START,{}", serde_json::to_string(&match_start_info).unwrap()));
                                    tracing::debug!("Sending {},MATCH_START message to player {}", player_id, match_start_info.session_id);
                                    if socket.send(match_start_msg).await.is_err() {
                                        tracing::error!("Failed to send MATCH_START message to player {}", player_id);
                                        break;
                                    }
                                    break;  // Exit the loop after match starts
                                }
                                MatchmakingEvent::MatchNotFound => {
                                    let match_not_found_msg = Message::Text(String::from("MATCH_NOT_FOUND"));
                                    if socket.send(match_not_found_msg).await.is_err() {
                                        tracing::error!("Failed to send MATCH_NOT_FOUND message to player {}", player_id);
                                        break;
                                    }
                                }
                            }
                        }
                        else => break,
                    }
                }

                tracing::info!("WebSocket connection closed for player {}", player_id);
                state.read().await.remove_player(pool_type, player_id).await;
            }
            _ => {
                tracing::error!("Invalid op code: {}", op_code);
            }
        }
    }
}

fn parse_message(message: &str) -> (String, String) {
    match message.split_once(',') {
        Some((first, rest)) => (first.to_string(), rest.to_string()),
        None => (message.to_string(), String::new()),
    }
}

// TODO: After authenticating session_ticket, player_id should be checkted to match!
async fn authenticate_player(session_ticket: String) -> bool {
    let client = reqwest::Client::new();
    let playfab_api_key = std::env::var("PLAYFAB_API_KEY").unwrap();
    let playfab_api_url = std::env::var("PLAYFAB_API_URL").unwrap();
    let response = client
        .post(format!(
            "{}/Server/AuthenticateSessionTicket", // /Server/AuthenticateSessionTicket
            playfab_api_url
        ))
        .header("X-SecretKey", playfab_api_key)
        .json(&serde_json::json!({
            "SessionTicket": session_ticket,
        }))
        .send()
        .await;
    match response {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<serde_json::Value>().await {
                    Ok(response_body) => {
                        tracing::debug!("Authenticated player: {}", response_body);
                        return true;
                    }
                    Err(e) => {
                        tracing::error!("Failed to authenticate player: {}", e);
                        return false;
                    }
                }
            } else {
                tracing::error!("Failed to authenticate player: {}", response.status());
                return false;
            }
        }
        Err(e) => {
            tracing::error!("Failed to authenticate player: {}", e);
            return false;
        }
    }
}
