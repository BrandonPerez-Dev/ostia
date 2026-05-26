//! Postgres-backed profile source. V0a stub — implementation lands in V0b.

use async_trait::async_trait;

use super::profile::{PostgresSourceDef, ProfileSource, SourcedConfig};

#[allow(dead_code)]
pub struct PostgresProfileSource {
    def: PostgresSourceDef,
}

impl PostgresProfileSource {
    pub fn new(def: PostgresSourceDef) -> anyhow::Result<Self> {
        Ok(Self { def })
    }
}

#[async_trait]
impl ProfileSource for PostgresProfileSource {
    async fn load(&self) -> anyhow::Result<SourcedConfig> {
        anyhow::bail!(
            "profile source (postgres): provider not yet implemented in V0a; landing in V0b. \
             DSN was: {}",
            self.def.dsn
        );
    }
}
