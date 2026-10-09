mod handlers;
mod helpers;

use axum::Router;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    handlers::routes()
}
