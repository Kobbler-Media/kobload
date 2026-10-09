use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard, OnceCell};

static TEST_DB_INIT: OnceCell<()> = OnceCell::const_new();
static TEST_DB_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub async fn test_db_lock() -> MutexGuard<'static, ()> {
    TEST_DB_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

pub async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://kobload:kobload@localhost:5434/kobload_test".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&url)
        .await
        .expect("pool");

    TEST_DB_INIT
        .get_or_init(|| async {
            sqlx::migrate!().run(&pool).await.expect("migrate");
        })
        .await;

    pool
}
