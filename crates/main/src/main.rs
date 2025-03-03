use std::sync::Arc;
use async_trait::async_trait;
use axum::Router;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use lobby::domain::{MessageHandler, MessageResponse, Participant, Room};

#[tokio::main]
async fn main() -> anyhow::Result<()>{
    tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::DEBUG)
        .with_current_span(false)
        .init();

    let message_handler = Arc::new(ChatMessageHandler);
    let lobby_router = lobby::setup(message_handler).await?;
    let router = Router::new()
        .nest("/chat", lobby_router);

    let listener = TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("failed to bind tcp listener");
    axum::serve(listener, router)
        .await
        .expect("http server failed unexpectedly");
    Ok(())
}

struct ChatMessageHandler;

#[derive(Clone, Debug, Deserialize)]
enum ChatInbound {
    SendPrivateMessage {
        to: Participant,
        content: String,
    },
    SendPublicMessage {
        content: String,
    },
    ListParticipants,
}

#[derive(Clone, Debug, Serialize)]
enum ChatOutbound {
    PrivateMessage {
        from: Participant,
        content: String,
    },
    PublicMessage {
        from: Participant,
        content: String,
    },
    ListOfParticipants {
        participants: Vec<Participant>,
    },
}

#[derive(Clone, Debug, Error)]
enum ChatError {}

#[async_trait]
impl MessageHandler<ChatInbound> for ChatMessageHandler {
    type Outbound = ChatOutbound;
    type Err = ChatError;

    async fn handle_message(&self, room: &Room, from: Participant, msg: ChatInbound) -> Result<MessageResponse<Self::Outbound>, Self::Err> {
        match msg {
            ChatInbound::SendPrivateMessage { to, content } =>
                Ok(MessageResponse::Unicast { to, msg: ChatOutbound::PrivateMessage { from, content } }),
            ChatInbound::SendPublicMessage { content } =>
                Ok(MessageResponse::Broadcast { msg: ChatOutbound::PublicMessage { from, content } }),
            ChatInbound::ListParticipants => {
                Ok(MessageResponse::Unicast { to: from, msg: ChatOutbound::ListOfParticipants { participants: room.participants.clone() } })
            }
        }
    }
}