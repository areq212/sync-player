use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::error::Error;
use thiserror::Error;
use uuid::Uuid;

pub type RoomId = Uuid;
pub type Participant = Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    pub participants: Vec<Participant>,
    pub capacity: usize,
    pub created_at: DateTime<Utc>,
    pub created_by: Participant,
}

impl Room {
    pub(crate) fn new(name: impl Into<String>, capacity: usize, participant: Participant) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            participants: Vec::with_capacity(capacity),
            capacity,
            created_at: Utc::now(),
            created_by: participant,
        }
    }

    pub(crate) fn join(&mut self, participant: Participant) -> Result<(), RoomError> {
        if self.is_full() {
            return Err(RoomError::RoomFull { room_id: self.id });
        }
        self.participants.push(participant);
        Ok(())
    }

    pub(crate) fn leave(&mut self, participant_id: Participant) {
        self.participants.retain(|p| *p != participant_id);
    }

    pub(crate) fn close(&self, participant: Participant) -> Result<(), RoomError> {
        if self.created_by != participant {
            return Err(RoomError::NotOwner {
                room_id: self.id,
                participant,
            });
        }
        Ok(())
    }

    pub(crate) async fn handle_message<In, Out>(
        &self,
        msg_handler: &dyn MessageHandler<In, Outbound=Out, Err=impl Error + Send + Sync + 'static>,
        from: Participant,
        message: In,
    ) -> Result<Vec<(Participant, Out)>, RoomError>
    where
        In: Send + Sync + 'static,
        Out: Clone + Send + Sync + 'static,
    {
        if !self.is_participant(from) {
            return Err(RoomError::NotParticipant {
                room_id: self.id,
                participant: from,
            });
        }
        match msg_handler
            .handle_message(&self, from, message)
            .await
            .map_err(|e| RoomError::MessageHandlerError(Box::new(e)))?
        {
            MessageResponse::Unicast { to, .. } if !self.is_participant(to) => {
                Err(RoomError::NotParticipant {
                    room_id: self.id,
                    participant: to,
                })
            }
            MessageResponse::Unicast { to, msg } => Ok(vec![(to, msg)]),
            MessageResponse::Broadcast { msg } => Ok(self
                .participants
                .iter()
                .map(|to| (*to, msg.clone()))
                .collect()),
            MessageResponse::Void => Ok(vec![]),
        }
    }

    pub fn is_full(&self) -> bool {
        self.participants.len() >= self.capacity
    }

    pub fn is_participant(&self, participant: Participant) -> bool {
        self.participants.contains(&participant)
    }
}

#[derive(Error, Debug)]
pub enum RoomError {
    #[error("room is full: {room_id}")]
    RoomFull { room_id: RoomId },
    #[error("room: {room_id} is not owned by: {participant}")]
    NotOwner {
        room_id: RoomId,
        participant: Participant,
    },
    #[error("room: {room_id} is not participant by: {participant}")]
    NotParticipant {
        room_id: RoomId,
        participant: Participant,
    },
    #[error("message handler error: {0}")]
    MessageHandlerError(#[source] Box<dyn std::error::Error>),
}

#[async_trait]
pub trait MessageSender<Outbound> {
    async fn send(&self, to: Participant, outbound_msg: Outbound) -> Result<(), MessageSenderError>;
}

#[derive(Error, Debug)]
pub enum MessageSenderError {
    #[error("participant disconnected: {0}")]
    ParticipantDisconnected(Participant, #[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error(transparent)]
    MessageSenderError(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
}

#[async_trait]
pub(crate) trait RoomRepository {
    type Err: Error + Send + Sync + 'static;

    async fn get(&self, room_id: RoomId) -> Result<Option<Room>, Self::Err>;
    async fn get_all(&self) -> Result<Vec<Room>, Self::Err>;
    async fn save(&self, room: Room) -> Result<Room, Self::Err>;
    async fn delete(&self, room_id: RoomId) -> Result<(), Self::Err>;
}

#[async_trait]
pub trait MessageHandler<Inbound>: Send + Sync + 'static {
    type Outbound;
    type Err: Error;

    async fn handle_message(
        &self,
        room: &Room,
        from: Participant,
        msg: Inbound,
    ) -> Result<MessageResponse<Self::Outbound>, Self::Err>;
}

pub enum MessageResponse<M> {
    Unicast { to: Participant, msg: M },
    Broadcast { msg: M },
    Void,
}
