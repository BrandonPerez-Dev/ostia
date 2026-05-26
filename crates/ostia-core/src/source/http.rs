//! HTTP-backed profile source. V0a stub — implementation lands in V0b.

use async_trait::async_trait;

use super::profile::{HttpSourceDef, ProfileSource, SourcedConfig};

#[allow(dead_code)]
pub struct HttpProfileSource {
    def: HttpSourceDef,
}

impl HttpProfileSource {
    pub fn new(def: HttpSourceDef) -> anyhow::Result<Self> {
        Ok(Self { def })
    }
}

#[async_trait]
impl ProfileSource for HttpProfileSource {
    async fn load(&self) -> anyhow::Result<SourcedConfig> {
        anyhow::bail!(
            "profile source (http): provider not yet implemented in V0a; landing in V0b. \
             URL was: {}",
            self.def.url
        );
    }
}
