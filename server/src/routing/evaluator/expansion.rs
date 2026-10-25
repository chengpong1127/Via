use super::*;
use super::{bike::BikeShareExpander, transit::TransitExpander, walking::WalkingExpander};

pub(super) struct ExpansionContext<'a> {
    pub corridor: &'a EvaluationCorridor,
    pub edge: &'a CorridorEdge,
    pub index: usize,
    pub query: &'a PlanningQuery,
    pub stats: &'a mut Phase2EvaluationStats,
    pub failures: &'a mut EvaluationFailures,
    pub surface_planners: &'a mut [Box<dyn SurfaceMovementPlanner>],
}

#[async_trait::async_trait]
pub(super) trait MovementExpander: Send {
    async fn extend_state(
        &mut self,
        context: &mut ExpansionContext<'_>,
        state: &EvaluationState,
    ) -> Result<Vec<EvaluationState>, RoutingError>;
}

impl ExpansionContext<'_> {
    pub fn check_deadline(&self) -> Result<(), RoutingError> {
        if self.query.deadline.is_elapsed() {
            Err(RoutingError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }

    pub fn charge(&mut self) -> Result<(), RoutingError> {
        self.check_deadline()?;
        if self.stats.corridor_states_evaluated >= self.query.limits.max_phase2_expansions {
            return Err(RoutingError::Phase2EvaluationLimit);
        }
        self.stats.corridor_states_evaluated += 1;
        Ok(())
    }

    pub async fn realize(
        &mut self,
        planner_index: usize,
        request: &SurfaceRequest<'_>,
    ) -> Result<Vec<SurfaceMovement>, RoutingError> {
        self.charge()?;
        let result = self.surface_planners[planner_index].realize(request).await;
        self.check_deadline()?;
        match result {
            Ok(items) => Ok(items),
            Err(error) => {
                match error.failure_class() {
                    ProviderFailureClass::Transient => self.failures.transient = true,
                    ProviderFailureClass::Internal => self.failures.internal = true,
                    ProviderFailureClass::Unreachable => {}
                }
                Ok(Vec::new())
            }
        }
    }
}

impl FixedCorridorEvaluator<'_> {
    pub(super) async fn expand_state(
        &mut self,
        state: &EvaluationState,
        corridor: &EvaluationCorridor,
        edge: &CorridorEdge,
        index: usize,
    ) -> Result<Vec<EvaluationState>, RoutingError> {
        let mut transit = TransitExpander {
            network: self.network,
            corridor: self.corridor,
            snapshot: self.snapshot,
        };
        let mut walking = WalkingExpander;
        let mut bike = BikeShareExpander;
        let mut context = ExpansionContext {
            corridor,
            edge,
            index,
            query: self.query,
            stats: &mut self.stats,
            failures: &mut self.failures,
            surface_planners: self.surface_planners,
        };
        let expanders: [&mut dyn MovementExpander; 3] = [&mut transit, &mut walking, &mut bike];
        let mut next = Vec::new();
        for expander in expanders {
            context.check_deadline()?;
            let successors = expander.extend_state(&mut context, state).await?;
            next.extend(successors);
        }
        Ok(next)
    }
}
