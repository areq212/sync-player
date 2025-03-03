use std::error::Error;
use std::fmt::Debug;
use std::sync::Arc;
use axum::Router;
use serde::de::DeserializeOwned;
use serde::Serialize;
use crate::api::AppState;
use crate::domain::MessageHandler;
use crate::infrastructure::{init_actor_proxy, InMemoryRoomRepo};

mod api;
mod app;
pub mod domain;
mod infrastructure;

pub async fn setup<Inbound, Outbound, Err>(
    message_handler: Arc<dyn MessageHandler<Inbound, Outbound=Outbound, Err=Err> + Send + Sync + 'static>
) -> anyhow::Result<Router>
where
    Inbound: DeserializeOwned + Debug + Clone + Send + Sync + 'static,
    Outbound: Serialize + Debug + Clone + Send + Sync + 'static,
    Err: Error + Send + Sync + 'static,
    Err: Clone,
{
    let (actor, message_sender) = init_actor_proxy::<Outbound>(100);
    let room_repo = InMemoryRoomRepo::new();

    let app_state = AppState {
        room_repo,
        message_sender,
        message_handler,
    };

    tokio::spawn(async move { actor.process().await; });

    Ok(api::router(app_state))
}
