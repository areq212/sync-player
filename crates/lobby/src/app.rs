use crate::domain::{MessageHandler, MessageSender, MessageSenderError, Participant, Room, RoomError, RoomId, RoomRepository};
use std::error::Error;
use thiserror::Error;

pub(crate) async fn list_rooms(room_repo: &impl RoomRepository) -> Result<Vec<Room>, RoomAppError> {
    room_repo
        .get_all()
        .await
        .map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))
}

pub(crate) async fn open_room(
    room_repo: &impl RoomRepository,
    name: impl Into<String>,
    capacity: usize,
    participant: Participant,
) -> Result<Room, RoomAppError> {
    let room = Room::new(name, capacity, participant);
    room_repo
        .save(room)
        .await
        .map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))
}

pub(crate) async fn close_room(
    room_repo: &impl RoomRepository,
    room_id: RoomId,
    participant: Participant,
) -> Result<(), RoomAppError> {
    let room = room_repo
        .get(room_id)
        .await
        .map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))?;
    let room = room.ok_or(RoomAppError::RoomNotFound { room_id })?;
    room.close(participant)?;
    room_repo
        .delete(room_id)
        .await
        .map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))
}

pub(crate) async fn join_room(
    room_repo: &impl RoomRepository,
    room_id: RoomId,
    participant: Participant,
) -> Result<(), RoomAppError> {
    let room = room_repo
        .get(room_id)
        .await
        .map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))?;
    let mut room = room.ok_or(RoomAppError::RoomNotFound { room_id })?;
    room.join(participant)?;
    room_repo.save(room).await.map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))?;
    Ok(())
}

pub(crate) async fn leave_room(
    room_repo: &impl RoomRepository,
    room_id: RoomId,
    participant_id: Participant,
) -> Result<(), RoomAppError> {
    let room = room_repo
        .get(room_id)
        .await
        .map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))?;
    let mut room = room.ok_or(RoomAppError::RoomNotFound { room_id })?;
    room.leave(participant_id);
    Ok(())
}

pub(crate) async fn handle_message<Inbound, Outbound: Clone>(
    room_repo: &impl RoomRepository,
    msg_sender: &impl MessageSender<Outbound>,
    msg_handler: &dyn MessageHandler<Inbound, Outbound=Outbound, Err=impl Error + Send + Sync + 'static>,
    room_id: RoomId,
    participant: Participant,
    inbound_msg: Inbound,
) -> Result<(), RoomAppError>
where
    Inbound: Send + Sync + 'static,
    Outbound: Send + Sync + 'static,
{
    let room = room_repo
        .get(room_id)
        .await
        .map_err(|e| RoomAppError::RoomRepositoryError(Box::new(e)))?;
    let room = room.ok_or(RoomAppError::RoomNotFound { room_id })?;
    let responses = room
        .handle_message(msg_handler, participant, inbound_msg)
        .await?;
    for (to, outbound_msg) in responses {
        let result = msg_sender
            .send(to, outbound_msg)
            .await;
        if let Err(e) = result {
            match e {
                MessageSenderError::ParticipantDisconnected(participant, _) => {
                    leave_room(room_repo, room_id, participant).await?
                }
                MessageSenderError::MessageSenderError(_) => {
                    return Err(RoomAppError::MessageSenderError(Box::new(e)));
                }
            }
        }
    }
    Ok(())
}

#[derive(Error, Debug)]
pub enum RoomAppError {
    #[error("room not found: {room_id}")]
    RoomNotFound { room_id: RoomId },
    #[error(transparent)]
    RoomDomain(#[from] RoomError),
    #[error("room repository error: {0}")]
    RoomRepositoryError(#[source] Box<dyn Error + Send + Sync + 'static>),
    #[error("message sender error: {0}")]
    MessageSenderError(#[source] Box<dyn Error + Send + Sync + 'static>),
}
