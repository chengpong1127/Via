use super::*;

impl FixedCorridorEvaluator<'_> {
    pub(super) fn finalize_states(
        &self,
        corridor: &EvaluationCorridor,
        states: Vec<EvaluationState>,
    ) -> Result<Vec<EvaluatedCandidate>, RoutingError> {
        let mut candidates = Vec::new();
        for state in states {
            let metrics = state.metrics();
            if metrics.transfer_count as usize > self.query.limits.max_transfers {
                continue;
            }
            let mut anchors = corridor
                .anchors()
                .iter()
                .map(|a| JourneyAnchor {
                    kind: a.kind.clone(),
                    reached_at: self.query.departure_time,
                })
                .collect::<Vec<_>>();
            let mut reached_at = self.query.departure_time;
            for (index, anchor) in anchors.iter_mut().enumerate() {
                if let Some(movement) = state.movements.iter().find(|m| m.to_anchor == index) {
                    reached_at = movement.arrival_at;
                }
                anchor.reached_at = reached_at;
            }
            match RealizedJourney::from_movements(
                anchors,
                state.movements,
                self.query.departure_time,
            ) {
                Ok(journey) => candidates.push(EvaluatedCandidate { metrics, journey }),
                Err(error) => {
                    return Err(RoutingError::Internal(format!(
                        "evaluated journey has invalid movement continuity: {error}"
                    )));
                }
            }
        }
        Ok(candidates)
    }
}
