use crate::domain::{MessageSender, MessageSenderError, Participant, Room, RoomId, RoomRepository};
use anyhow::anyhow;
use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use futures_util::SinkExt;
use futures_util::stream::SplitSink;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Clone, Default)]
pub(crate) struct InMemoryRoomRepo {
    map: Arc<Mutex<HashMap<RoomId, Room>>>,
}

impl InMemoryRoomRepo {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[derive(Error, Debug)]
#[error(transparent)]
pub struct InfrastructureError(#[from] anyhow::Error);

#[async_trait]
impl RoomRepository for InMemoryRoomRepo {
    type Err = InfrastructureError;

    async fn get(&self, room_id: RoomId) -> Result<Option<Room>, Self::Err> {
        let guard = self.map.lock().await;
        Ok(guard.get(&room_id).cloned())
    }

    async fn get_all(&self) -> Result<Vec<Room>, Self::Err> {
        let guard = self.map.lock().await;
        Ok(guard.values().cloned().collect())
    }

    async fn save(&self, room: Room) -> Result<Room, Self::Err> {
        let mut guard = self.map.lock().await;
        guard.insert(room.id, room.clone());
        Ok(room)
    }

    async fn delete(&self, room_id: RoomId) -> Result<(), Self::Err> {
        let mut guard = self.map.lock().await;
        guard.remove(&room_id);
        Ok(())
    }
}

pub(crate) enum Command<M: Send + Sync + 'static> {
    RegisterParticipant {
        participant: Participant,
        ws_sender: SplitSink<WebSocket, Message>,
        result_sender: oneshot::Sender<()>,
    },
    UnregisterParticipant {
        participant: Participant,
        result_sender: oneshot::Sender<()>,
    },
    SendMessage {
        participant: Participant,
        message: M,
        result_sender: oneshot::Sender<Result<(), MessageSenderError>>,
    },
}

pub(crate) struct MessageSenderActor<M: Send + Sync + 'static> {
    receiver: Receiver<Command<M>>,
    map: HashMap<Participant, SplitSink<WebSocket, Message>>,
}

impl<M: Serialize + Send + Sync + 'static> MessageSenderActor<M> {
    pub(crate) async fn process(mut self) {
        while let Some(command) = self.receiver.recv().await {
            match command {
                Command::RegisterParticipant {
                    participant,
                    ws_sender,
                    result_sender,
                } => {
                    self.map.insert(participant, ws_sender);
                    let _ = result_sender.send(());
                }
                Command::UnregisterParticipant {
                    participant,
                    result_sender,
                } => {
                    self.map.remove(&participant);
                    let _ = result_sender.send(());
                }
                Command::SendMessage {
                    participant,
                    message,
                    result_sender,
                } => {
                    let Some(sink) = self.map.get_mut(&participant) else {
                        let _ = result_sender.send(Err(MessageSenderError::MessageSenderError(Box::new(InfrastructureError(anyhow!(
                            "sink not found for participant: {participant}"
                        ))))));
                        return;
                    };
                    let msg_json = serde_json::to_string(&message);
                    let Ok(msg_json) = msg_json else {
                        let _ = result_sender.send(msg_json.map(|_| ()).map_err(|e| MessageSenderError::MessageSenderError(Box::new(e))));
                        return;
                    };
                    let send = sink.send(Message::from(msg_json)).await;

                    // remove participant when disconnected
                    let response = match send {
                        Ok(_) => Ok(()),
                        Err(e) => {
                            tracing::info!("participant disconnected: {}", participant);
                            self.map.remove(&participant);
                            Err(MessageSenderError::ParticipantDisconnected(participant, Box::new(e)))
                        }
                    };
                    let _ = result_sender.send(response);
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct MessageSenderProxy<M: Send + Sync + 'static> {
    sender: Sender<Command<M>>,
}

impl<M: Send + Sync + 'static> MessageSenderProxy<M> {
    pub async fn register(
        &self,
        participant: Participant,
        ws_sender: SplitSink<WebSocket, Message>,
    ) -> Result<(), anyhow::Error> {
        let (result_sender, result_receiver) = oneshot::channel();
        self.sender
            .send(Command::RegisterParticipant {
                participant,
                ws_sender,
                result_sender,
            })
            .await?;
        Ok(result_receiver
            .await
            .unwrap_or_else(|_| panic!("Failed to receive result from actor")))
    }

    pub async fn unregister(&self, participant: Participant) -> Result<(), anyhow::Error> {
        let (result_sender, result_receiver) = oneshot::channel();
        self.sender
            .send(Command::UnregisterParticipant {
                participant,
                result_sender,
            })
            .await?;
        Ok(result_receiver
            .await
            .unwrap_or_else(|_| panic!("Failed to receive result from actor")))
    }
}

#[async_trait]
impl<M: Send + Sync + 'static> MessageSender<M> for MessageSenderProxy<M> {
    async fn send(&self, participant: Participant, message: M) -> Result<(), MessageSenderError> {
        let (result_sender, result_receiver) = oneshot::channel();
        self.sender
            .send(Command::SendMessage {
                participant,
                message,
                result_sender,
            })
            .await
            .map_err(|e| MessageSenderError::MessageSenderError(Box::new(e)))?;
        result_receiver
            .await
            .unwrap_or_else(|_| panic!("Failed to receive result from actor"))?;
        Ok(())
    }
}

pub(crate) fn init_actor_proxy<M: Send + Sync + 'static>(
    size: usize,
) -> (MessageSenderActor<M>, MessageSenderProxy<M>) {
    let (sender, receiver) = mpsc::channel(size);
    let actor = MessageSenderActor {
        receiver,
        map: Default::default(),
    };
    let proxy = MessageSenderProxy { sender };
    (actor, proxy)
}
