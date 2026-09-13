//! Stable Via identities. Provider aliases are deliberately not part of the
//! routing graph.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_id!(OperatorId);
opaque_id!(StopPlaceId);
opaque_id!(BoardingPointId);
opaque_id!(BikeShareStationId);
opaque_id!(TransitServiceId);
opaque_id!(ServicePatternId);
opaque_id!(ScheduledRunId);
opaque_id!(VehicleId);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityId {
    Operator(OperatorId),
    StopPlace(StopPlaceId),
    BoardingPoint(BoardingPointId),
    BikeShareStation(BikeShareStationId),
    TransitService(TransitServiceId),
    ServicePattern(ServicePatternId),
    ScheduledRun(ScheduledRunId),
    Vehicle(VehicleId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    Tdx,
    Taipei,
    NewTaipei,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AliasKey {
    provider: Provider,
    entity_kind: String,
    authority_namespace: String,
    upstream_key: String,
}

impl AliasKey {
    pub fn new(
        provider: Provider,
        entity_kind: impl Into<String>,
        authority_namespace: impl Into<String>,
        upstream_key: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            entity_kind: entity_kind.into(),
            authority_namespace: authority_namespace.into(),
            upstream_key: upstream_key.into(),
        }
    }
}

#[derive(Clone, Debug)]
struct AliasRegistration {
    entity: EntityId,
    valid_from: OffsetDateTime,
    valid_until: Option<OffsetDateTime>,
}

#[derive(Default)]
pub struct AliasRegistry {
    registrations: HashMap<AliasKey, Vec<AliasRegistration>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AliasError {
    #[error("provider alias already identifies another Via entity in this interval")]
    ConflictingIdentity,
}

impl AliasRegistry {
    pub fn register_reviewed(
        &mut self,
        alias: AliasKey,
        entity: EntityId,
        valid_from: OffsetDateTime,
    ) -> Result<(), AliasError> {
        let registrations = self.registrations.entry(alias).or_default();
        if registrations.iter().any(|registration| {
            registration.entity != entity
                && registration.valid_from <= valid_from
                && registration
                    .valid_until
                    .is_none_or(|valid_until| valid_from < valid_until)
        }) {
            return Err(AliasError::ConflictingIdentity);
        }
        registrations.push(AliasRegistration {
            entity,
            valid_from,
            valid_until: None,
        });
        registrations.sort_by_key(|registration| registration.valid_from);
        Ok(())
    }

    pub fn retire(&mut self, alias: &AliasKey, valid_until: OffsetDateTime) {
        if let Some(registration) = self
            .registrations
            .get_mut(alias)
            .and_then(|registrations| registrations.last_mut())
        {
            registration.valid_until = Some(valid_until);
        }
    }

    pub fn resolve(&self, alias: &AliasKey, at: OffsetDateTime) -> Option<&EntityId> {
        self.registrations.get(alias).and_then(|registrations| {
            registrations
                .iter()
                .rev()
                .find(|registration| {
                    registration.valid_from <= at
                        && registration
                            .valid_until
                            .is_none_or(|valid_until| at < valid_until)
                })
                .map(|registration| &registration.entity)
        })
    }
}

/// Provider-neutral identity persistence boundary. Implementations may use a
/// database or an offline registry, but routing never needs to know which one.
#[async_trait::async_trait]
pub trait IdentityRepository: Send + Sync {
    async fn resolve(
        &self,
        entity_kind: &str,
        authority_namespace: &str,
        upstream_key: &str,
    ) -> Option<String>;

    async fn upstream_key(
        &self,
        _entity_kind: &str,
        _authority_namespace: &str,
        via_id: &str,
    ) -> Option<String> {
        Some(via_id.to_owned())
    }
}
