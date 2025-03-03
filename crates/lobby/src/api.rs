use crate::app;
use crate::app::RoomAppError;
use crate::domain::{MessageHandler, Participant, RoomError, RoomId};
use crate::infrastructure::{InMemoryRoomRepo, MessageSenderProxy};
use axum::{Json, Router};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use futures_util::StreamExt;
use futures_util::stream::{SplitSink, SplitStream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::Debug;
use std::str::FromStr;
use std::sync::Arc;
use axum::routing::{delete, get};
use thiserror::Error;
use uuid::Uuid;

const PARTICIPANT: &str = "participant";

#[derive(Clone)]
pub(crate) struct AppState<Inbound, Outbound, Err>
where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    pub(crate) room_repo: InMemoryRoomRepo,
    pub(crate) message_sender: MessageSenderProxy<Outbound>,
    pub(crate) message_handler:
        Arc<dyn MessageHandler<Inbound, Outbound=Outbound, Err=Err> + Send + Sync + 'static>,
}

pub(crate) fn router<Inbound, Outbound, Err>(app_state: AppState<Inbound, Outbound, Err>) -> Router
where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    Router::new()
        .route("/rooms", get(get_rooms).post(create_room))
        .route("/rooms/{room_id}", delete(delete_room).get(join_room))
        .with_state(app_state)
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateRoomRequest {
    name: String,
    capacity: usize,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("invalid participant cookie")]
    InvalidParticipantCookie,
    #[error(transparent)]
    RoomAppError(#[from] RoomAppError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::InvalidParticipantCookie => {
                (StatusCode::BAD_REQUEST, "invalid participant id").into_response()
            }
            ApiError::RoomAppError(e) => e.into_response(),
        }
    }
}

impl IntoResponse for RoomAppError {
    fn into_response(self) -> Response {
        match self {
            RoomAppError::RoomNotFound { room_id } => {
                (StatusCode::NOT_FOUND, format!("room {room_id} not found")).into_response()
            }
            RoomAppError::RoomDomain(e) => match e {
                RoomError::RoomFull { .. } => {
                    (StatusCode::BAD_REQUEST, "room full").into_response()
                }
                RoomError::NotOwner { room_id, .. } => (
                    StatusCode::UNAUTHORIZED,
                    format!("not an owner of the room {room_id}"),
                )
                    .into_response(),
                RoomError::NotParticipant { room_id, .. } => (
                    StatusCode::BAD_REQUEST,
                    format!("not participant of the room {room_id}"),
                )
                    .into_response(),
                RoomError::MessageHandlerError(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
                }
            },
            RoomAppError::RoomRepositoryError(_) | RoomAppError::MessageSenderError(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
            }
        }
    }
}

pub(crate) async fn get_rooms<Inbound, Outbound, Err>(
    State(app_state): State<AppState<Inbound, Outbound, Err>>,
) -> Result<impl IntoResponse, ApiError>
where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    let rooms = app::list_rooms(&app_state.room_repo).await?;
    Ok(Json(rooms))
}

pub(crate) async fn create_room<Inbound, Outbound, Err>(
    State(app_state): State<AppState<Inbound, Outbound, Err>>,
    cookie_jar: CookieJar,
    Json(request): Json<CreateRoomRequest>,
) -> Result<impl IntoResponse, ApiError>
where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    let (participant, cookie_jar) =
        get_participant(cookie_jar).map_err(|_| ApiError::InvalidParticipantCookie)?;
    let room = app::open_room(
        &app_state.room_repo,
        request.name,
        request.capacity,
        participant,
    )
        .await?;
    Ok((StatusCode::OK, cookie_jar, Json(room)))
}

pub(crate) async fn delete_room<Inbound, Outbound, Err>(
    State(app_state): State<AppState<Inbound, Outbound, Err>>,
    cookie_jar: CookieJar,
    Path(room_id): Path<RoomId>,
) -> Result<impl IntoResponse, ApiError>
where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    let (participant, cookie_jar) =
        get_participant(cookie_jar).map_err(|_| ApiError::InvalidParticipantCookie)?;
    app::close_room(&app_state.room_repo, room_id, participant).await?;
    Ok((StatusCode::OK, cookie_jar))
}

pub(crate) async fn join_room<Inbound, Outbound, Err>(
    State(app_state): State<AppState<Inbound, Outbound, Err>>,
    cookie_jar: CookieJar,
    Path(room_id): Path<RoomId>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError>
where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    let (participant, cookie_jar) =
        get_participant(cookie_jar).map_err(|_| ApiError::InvalidParticipantCookie)?;
    let app_state_clone = app_state.clone();
    app::join_room(&app_state.room_repo, room_id, participant).await?;
    tracing::info!("Participant {participant} joined room");
    let response =
        ws.on_upgrade(move |ws| handle_socket(app_state_clone, room_id, participant, ws));
    Ok((cookie_jar, response))
}

async fn handle_socket<Inbound, Outbound, Err>(
    app_state: AppState<Inbound, Outbound, Err>,
    room_id: RoomId,
    participant: Participant,
    socket: WebSocket,
) where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    let (sender, mut receiver): (SplitSink<WebSocket, Message>, SplitStream<WebSocket>) =
        socket.split();
    app_state
        .message_sender
        .register(participant, sender)
        .await
        .expect("should never happen");
    while let Some(msg) = receiver.next().await {
        let Ok(msg) = msg else {
            tracing::info!("participant disconnected: {}", participant);
            app_state
                .message_sender
                .unregister(participant)
                .await
                .expect("should never happen");
            return;
        };
        match msg {
            Message::Text(msg) => {
                tracing::info!("{participant}: {}", msg.as_str());
                let maybe_inbound = serde_json::from_slice(msg.as_bytes());
                let Ok(inbound) = maybe_inbound else {
                    tracing::error!("failed to deserialize inbound message: {:?}", maybe_inbound);
                    app_state
                        .message_sender
                        .unregister(participant)
                        .await
                        .expect("should never happen");
                    return;
                };
                let app_state_clone = app_state.clone();
                let handle_result = app::handle_message(
                    &app_state_clone.room_repo,
                    &app_state_clone.message_sender,
                    app_state_clone.message_handler.as_ref(),
                    room_id,
                    participant,
                    inbound,
                )
                    .await;
                if let Err(e) = handle_result {
                    tracing::error!("failed to handle message {:?}", e)
                }
            }
            Message::Close(_) => {
                tracing::info!("participant disconnected: {}", participant);
                let _ = app::leave_room(&app_state.room_repo, room_id, participant).await;
                app_state
                    .message_sender
                    .unregister(participant)
                    .await
                    .expect("should never happen");
                return;
            }
            _ => tracing::warn!("received unknown message type"),
        }
    }
}

fn get_participant(cookie_jar: CookieJar) -> Result<(Participant, CookieJar), uuid::Error> {
    match cookie_jar.get(PARTICIPANT) {
        Some(cookie) => Ok((Uuid::from_str(cookie.value())?, cookie_jar)),
        None => {
            let participant = Uuid::new_v4();
            Ok((
                participant,
                cookie_jar.add(Cookie::new(PARTICIPANT, participant.to_string())),
            ))
        }
    }
}
