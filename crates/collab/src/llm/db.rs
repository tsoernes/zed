use std::future::Future;
use std::sync::Arc;

use anyhow::anyhow;
pub use sea_orm::ConnectOptions;
use sea_orm::{DatabaseConnection, DatabaseTransaction, IsolationLevel, TransactionTrait};

use crate::Result;
use crate::db::TransactionHandle;
use crate::executor::Executor;
use crate::tokio_runtime::{ensure_global_tokio_runtime, run_on_global};

/// The database for the LLM service.
pub struct LlmDatabase {
    options: ConnectOptions,
    pool: DatabaseConnection,
    #[allow(unused)]
    executor: Executor,
    #[cfg(test)]
    runtime: Option<tokio::runtime::Runtime>,
}

impl LlmDatabase {
    /// Connects to the database with the given options
    pub async fn new(options: ConnectOptions, executor: Executor) -> Result<Self> {
        sqlx::any::install_default_drivers();

        // Use global Tokio runtime so sqlx / sea-orm background tasks remain active
        ensure_global_tokio_runtime()?; // initialize global runtime
        let saved_options = options;
        let connect_options = saved_options.clone();
        let inner =
            run_on_global(async move { sea_orm::Database::connect(connect_options).await })?;
        let pool = inner?;

        Ok(Self {
            options: saved_options,
            pool,
            executor,
            #[cfg(test)]
            runtime: None,
        })
    }

    pub fn options(&self) -> &ConnectOptions {
        &self.options
    }

    pub async fn transaction<F, Fut, T>(&self, f: F) -> Result<T>
    where
        F: Send + Fn(TransactionHandle) -> Fut,
        Fut: Send + Future<Output = Result<T>>,
    {
        let body = async {
            let (tx, result) = self.with_transaction(&f).await?;
            match result {
                Ok(result) => match tx.commit().await.map_err(Into::into) {
                    Ok(()) => Ok(result),
                    Err(error) => Err(error),
                },
                Err(error) => {
                    tx.rollback().await?;
                    Err(error)
                }
            }
        };

        self.run(body).await
    }

    async fn with_transaction<F, Fut, T>(&self, f: &F) -> Result<(DatabaseTransaction, Result<T>)>
    where
        F: Send + Fn(TransactionHandle) -> Fut,
        Fut: Send + Future<Output = Result<T>>,
    {
        let tx = self
            .pool
            .begin_with_config(Some(IsolationLevel::ReadCommitted), None)
            .await?;

        let mut tx = Arc::new(Some(tx));
        let result = f(TransactionHandle(tx.clone())).await;
        let tx = Arc::get_mut(&mut tx)
            .and_then(|tx| tx.take())
            .ok_or_else(|| anyhow!("couldn't complete transaction because it's still in use"))?;

        Ok((tx, result))
    }

    async fn run<F, T>(&self, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        #[cfg(test)]
        {
            if let Executor::Deterministic(executor) = &self.executor {
                executor.simulate_random_delay().await;
            }

            self.runtime.as_ref().unwrap().block_on(future)
        }

        #[cfg(not(test))]
        {
            future.await
        }
    }
}
